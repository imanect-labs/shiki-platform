//! StorageService: 共有リンク（#342）— パスワード解錠（redeem）と per-user 参照カウント剥奪。
//!
//! - [`StorageService::redeem_share_link`] — token＋パスワードを検証し、呼び出しユーザーへ per-user
//!   タプルを発行する（authenticated なら誰でも・失敗は一律 403）。
//! - [`StorageService::reconcile_user_grants_for_link`] — リンク失効/期限失効時に、そのリンクの
//!   redeem 済み per-user タプルを **(node,user,role) 単位で参照カウント**して剥奪する（他 active
//!   リンクが同じ付与を保持していれば FGA タプルは消さない）。
//! - [`StorageService::list_share_link_grants`] / [`StorageService::revoke_share_link_grant`] —
//!   redeem 済み user の owner 向け可視化と per-user 個別取り消し（#369 C-3）。
//!
//! 共有ヘルパ（`broad_subject`/`verify_password`）は [`super::share_link`] に定義している。

#[allow(clippy::wildcard_imports)]
use super::*;

use super::share_link_util::{kind_of, verify_password};
use crate::model::{GeneralAccessLevel, ShareLinkGrant};

/// token で引く redeem 対象リンク 1 行。
#[derive(sqlx::FromRow)]
struct RedeemRow {
    link_id: Uuid,
    node_id: Uuid,
    org: String,
    kind: String,
    audience: String,
    role: String,
    expires_at: Option<DateTime<Utc>>,
    password_hash: Option<String>,
}

/// verify_redeem が返す、per-user 付与に必要な検証済み項目。
struct VerifiedRedeem {
    link_id: Uuid,
    node_id: Uuid,
    kind: NodeKind,
    role: ShareRole,
    level: GeneralAccessLevel,
    expires_at: Option<DateTime<Utc>>,
}

impl StorageService {
    /// パスワード付き共有リンクを解錠し、呼び出しユーザーへ per-user タプルを発行する（#342）。
    ///
    /// **authenticated であれば誰でも**呼べる（owner ゲート無し）。失敗理由は区別せず一律
    /// `Forbidden` に潰す（オラクル防止・存在秘匿）。token は自テナントの active・パスワード付き
    /// リンクのみ一致する（別テナント/失効/期限切れ/非パスワードは一律 Forbidden）。
    pub async fn redeem_share_link(
        &self,
        ctx: &AuthContext,
        token: &str,
        password: Option<&str>,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        // レート制限・token 引き・audience/パスワード検証は verify_redeem に分離（clippy 行数・可読性）。
        let v = self.verify_redeem(ctx, token, password, trace_id).await?;

        // per-user タプルを発行し、redeem 台帳へ記録する。
        let ns = ctx.ns();
        let obj = node_fga_object(&ns, v.kind, v.node_id);
        let subject = ns.user(&ctx.principal.id);
        // この (node,user,role) が既に redeem 台帳に載っているか（別リンク経由の先行 redeem）。
        let prior: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM node_share_link_grant \
             WHERE node_id = $1 AND user_id = $2 AND role = $3)",
        )
        .bind(v.node_id)
        .bind(&ctx.principal.id)
        .bind(v.role.as_str())
        .fetch_one(&self.db)
        .await?;
        // redeem 由来は **via_link 専用 relation**（viewer_via_link / editor_via_link）で発行する（#366）。
        // 明示共有（viewer / editor）とはタプルの出自が分かれるため、リンク失効時の per-user reconcile が
        // 明示共有を誤剥奪しない（B-2 根治）。granted は「この user に via_link タプルを新規に張ったか」。
        let granted = self
            .authz
            .write_tuple(&subject, v.role.relation_via_link(), &obj)
            .await?;
        // 台帳は via_link タプルの **参照カウント**（複数リンクが同一 (node,user,role) を redeem し得る）。
        // granted（新規付与）または prior（別リンク経由で先行 redeem 済み）なら本リンク分を記録する。
        let record = granted || prior;
        let persisted = self
            .persist_redeem(
                ctx,
                v.link_id,
                v.node_id,
                v.kind,
                v.role,
                v.expires_at,
                v.level,
                record,
                trace_id,
            )
            .await;
        if let Err(e) = persisted {
            if granted {
                let _ = self
                    .authz
                    .delete_tuple(&subject, v.role.relation_via_link(), &obj)
                    .await;
            }
            return Err(e);
        }
        if v.expires_at.is_some() {
            self.expiry_notify.notify_one();
        }
        Ok(())
    }

    /// redeem のレート制限・token 引き（テナント/org スコープ）・audience メンバーシップ・パスワードを
    /// 検証し、付与に必要な項目を返す（#342）。失敗は一律 `Forbidden`＋deny 監査（オラクル防止）。
    async fn verify_redeem(
        &self,
        ctx: &AuthContext,
        token: &str,
        password: Option<&str>,
        trace_id: Option<&str>,
    ) -> Result<VerifiedRedeem, StorageError> {
        // レート制限（総当たり・Argon2 CPU DoS の抑止・B-3）。principal と token の双方で数え、
        // どちらか超過なら Argon2 も DB 参照もせず即 deny する（DoS の芽を先に断つ）。
        let now = Utc::now();
        let principal_key = format!("p:{}|{}", ctx.tenant_id, ctx.principal.id);
        let token_key = format!("t:{token}");
        if !self.redeem_limiter.check(&principal_key, now)
            || !self.redeem_limiter.check(&token_key, now)
        {
            return Err(self
                .deny_redeem(ctx, "rate_limited", "node", "-", None, trace_id)
                .await);
        }

        // token は自テナント **かつ自 org** の active・パスワード付きリンクのみ一致する（Codex P1/B-4:
        // `anyone` でも org を跨がせない。org は storage の隔離境界＝load_node が絞る）。
        let row: Option<RedeemRow> = sqlx::query_as(
            "SELECT link_id, node_id, org, kind, audience, role, expires_at, password_hash \
             FROM node_share_link \
             WHERE token = $1 AND tenant_id = $2 AND org = $3 \
               AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > now())",
        )
        .bind(token)
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .fetch_optional(&self.db)
        .await?;
        let Some(row) = row else {
            return Err(self
                .deny_redeem(ctx, "token_not_found", "node", "-", None, trace_id)
                .await);
        };
        let node = row.node_id.to_string();
        let deny = |reason: &'static str| {
            self.deny_redeem(ctx, reason, &row.kind, &node, Some(row.link_id), trace_id)
        };
        // redeem はパスワード付きリンク専用（broad リンクは通常 ReBAC で開く）。
        let Some(hash) = row.password_hash.as_deref() else {
            return Err(deny("not_password_link").await);
        };
        let (Some(level), Some(role), Some(kind)) = (
            GeneralAccessLevel::parse(&row.audience),
            ShareRole::parse(&row.role),
            NodeKind::parse(&row.kind),
        ) else {
            return Err(deny("corrupt_row").await);
        };
        // audience 該当性: organization / anyone（A-2 で organization へ縮退）は当該組織のメンバーの
        // み、restricted（付与ゼロのポインタ）は redeem 不可。
        match level {
            GeneralAccessLevel::Anyone | GeneralAccessLevel::Organization => {
                let member = self
                    .authz
                    .check(
                        &ctx.subject(),
                        Relation::Member,
                        &ctx.ns().organization(&row.org),
                        Consistency::MinimizeLatency,
                    )
                    .await?;
                if !member {
                    return Err(deny("not_org_member").await);
                }
            }
            GeneralAccessLevel::Restricted => return Err(deny("restricted_link").await),
        }
        // パスワード検証（Argon2id・定数時間）。不一致/未指定は generic Forbidden＋deny 監査。
        if !verify_password(password.unwrap_or(""), hash) {
            return Err(deny("bad_password").await);
        }
        Ok(VerifiedRedeem {
            link_id: row.link_id,
            node_id: row.node_id,
            kind,
            role,
            level,
            expires_at: row.expires_at,
        })
    }

    /// redeem の deny を監査（`Decision::Deny`・非チェーン）し、`Forbidden` を返す（#342 レビュー
    /// B-3）。理由は `metadata.reason` にのみ残し、呼び出し側の応答は一律 403（オラクル防止）。
    /// 監査記録の失敗で redeem 応答は変えない（既に deny・fail する意味が無い）。ログには残す。
    async fn deny_redeem(
        &self,
        ctx: &AuthContext,
        reason: &'static str,
        object_type: &str,
        object_id: &str,
        link_id: Option<Uuid>,
        trace_id: Option<&str>,
    ) -> StorageError {
        let entry = AuditEntry {
            action: "node.share_link.redeem",
            object_type,
            object_id,
            decision: Decision::Deny,
            trace_id,
            metadata: json!({ "reason": reason, "link_id": link_id }),
        };
        if let Err(e) = self.audit.record(ctx, entry).await {
            tracing::warn!(error = %e, reason, "redeem deny の監査記録に失敗しました（#342）");
        }
        StorageError::Forbidden
    }

    /// redeem の台帳 upsert＋監査を 1 tx で。`record_grant == false` なら台帳へ記録しない（既に
    /// 明示共有等でアクセス済みで、後の失効処理がそのタプルを誤剥奪しないため）。
    #[allow(clippy::too_many_arguments)]
    async fn persist_redeem(
        &self,
        ctx: &AuthContext,
        link_id: Uuid,
        node_id: Uuid,
        kind: NodeKind,
        role: ShareRole,
        expires_at: Option<DateTime<Utc>>,
        level: GeneralAccessLevel,
        record_grant: bool,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        let mut tx = self.db.begin().await?;
        if record_grant {
            sqlx::query(
                "INSERT INTO node_share_link_grant \
                   (link_id, node_id, user_id, tenant_id, kind, role, expires_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7) \
                 ON CONFLICT (link_id, user_id) DO UPDATE SET \
                   role = EXCLUDED.role, expires_at = EXCLUDED.expires_at, granted_at = now()",
            )
            .bind(link_id)
            .bind(node_id)
            .bind(&ctx.principal.id)
            .bind(&ctx.tenant_id)
            .bind(kind.as_str())
            .bind(role.as_str())
            .bind(expires_at)
            .execute(&mut *tx)
            .await?;
        }
        audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: "node.share_link.redeem",
                object_type: kind.as_str(),
                object_id: &node_id.to_string(),
                decision: Decision::Allow,
                trace_id,
                metadata: json!({ "link_id": link_id, "audience": level, "role": role }),
            },
            Chain::Yes,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// あるリンクの redeem 済み per-user タプルを参照カウントして剥奪する（失効/期限失効時）。
    ///
    /// (node,user,role) について**他に active リンク由来の grant が残っていれば FGA タプルは
    /// 消さず**、当該リンクの grant 行だけ落とす。最後の 1 本なら FGA タプルを剥奪する。FGA 剥奪に
    /// 失敗したら `?` 伝播で tx を巻き戻す（fail-closed・失効を確定しない）。tx 内で呼ぶこと。
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn reconcile_user_grants_for_link(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        ns: &Namespace<'_>,
        obj: &FgaObject,
        link_id: Uuid,
        node_id: Uuid,
        tenant_id: &str,
        now: DateTime<Utc>,
    ) -> Result<(), StorageError> {
        let grants: Vec<(String, String)> =
            sqlx::query_as("SELECT user_id, role FROM node_share_link_grant WHERE link_id = $1")
                .bind(link_id)
                .fetch_all(&mut **tx)
                .await?;
        for (user_id, grole) in &grants {
            let Some(role) = ShareRole::parse(grole) else {
                continue; // 破損行は残す（黙って消さない）。
            };
            // 同一 (node,user,role) を保持する他 active リンク由来の grant 数。
            let remaining: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM node_share_link_grant g \
                 JOIN node_share_link l ON l.link_id = g.link_id \
                 WHERE g.node_id = $1 AND g.user_id = $2 AND g.role = $3 \
                   AND g.link_id <> $4 AND g.tenant_id = $5 \
                   AND l.revoked_at IS NULL AND (l.expires_at IS NULL OR l.expires_at > $6)",
            )
            .bind(node_id)
            .bind(user_id)
            .bind(grole)
            .bind(link_id)
            .bind(tenant_id)
            .bind(now)
            .fetch_one(&mut **tx)
            .await?;
            if remaining == 0 {
                // 最後の active grant → via_link タプルを剥奪（#366・失敗は ? 伝播で tx 巻き戻し＝
                // fail-closed）。明示共有の viewer/editor は別 relation なので決して触れない。
                self.authz
                    .delete_tuple(&ns.user(user_id), role.relation_via_link(), obj)
                    .await?;
            }
            // どちらの場合も当該リンクの grant 行は落とす（タプルは他 active リンクが保持）。
            sqlx::query("DELETE FROM node_share_link_grant WHERE link_id = $1 AND user_id = $2")
                .bind(link_id)
                .bind(user_id)
                .execute(&mut **tx)
                .await?;
        }
        Ok(())
    }
}

/// grant 1 行（user_id・表示名・付与時刻）。owner 向け可視化に使う（#369 C-3）。
#[derive(sqlx::FromRow)]
struct GrantRow {
    user_id: String,
    display_name: Option<String>,
    granted_at: DateTime<Utc>,
}

impl StorageService {
    /// リンクを redeem した user 一覧を返す（owner 権限・#369 C-3）。表示名は `directory_user` から
    /// 解決する（無ければ `None`）。broad リンクは台帳を持たないため常に空。
    pub async fn list_share_link_grants(
        &self,
        ctx: &AuthContext,
        link_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Vec<ShareLinkGrant>, StorageError> {
        if self
            .authorize_link_owner(ctx, link_id, "node.share_link.grants.list", trace_id)
            .await?
            .is_none()
        {
            return Err(StorageError::Forbidden);
        }
        let rows: Vec<GrantRow> = sqlx::query_as(
            "SELECT g.user_id, d.display_name, g.granted_at \
             FROM node_share_link_grant g \
             LEFT JOIN directory_user d \
               ON d.user_id = g.user_id AND d.tenant_id = g.tenant_id \
             WHERE g.link_id = $1 AND g.tenant_id = $2 \
             ORDER BY g.granted_at DESC",
        )
        .bind(link_id)
        .bind(&ctx.tenant_id)
        .fetch_all(&self.db)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| ShareLinkGrant {
                user_id: r.user_id,
                display_name: r.display_name,
                granted_at: r.granted_at,
            })
            .collect())
    }

    /// 特定 user の redeem を個別に取り消す（owner 権限・#369 C-3）。当該リンクの grant 行を落とし、
    /// 同 (node,user,role) を保持する他 active リンクが無ければ via_link タプルを剥奪する（参照カウント）。
    /// 明示共有（viewer/editor）は別 relation なので決して剥奪されない。存在しない grant は冪等成功。
    pub async fn revoke_share_link_grant(
        &self,
        ctx: &AuthContext,
        link_id: Uuid,
        user_id: &str,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        let Some((node_id, obj)) = self
            .authorize_link_owner(ctx, link_id, "node.share_link.grant.revoke", trace_id)
            .await?
        else {
            return Err(StorageError::Forbidden);
        };
        let ns = ctx.ns();
        let now = Utc::now();

        let mut tx = self.db.begin().await?;
        self.lock_node(&mut tx, node_id).await?;
        // 対象 grant の role を引く（無ければ冪等成功＝既に取り消し済み）。
        let grole: Option<String> = sqlx::query_scalar(
            "SELECT role FROM node_share_link_grant \
             WHERE link_id = $1 AND user_id = $2 AND tenant_id = $3",
        )
        .bind(link_id)
        .bind(user_id)
        .bind(&ctx.tenant_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(grole) = grole else {
            return Ok(());
        };
        let Some(role) = ShareRole::parse(&grole) else {
            return Err(StorageError::Integrity(format!(
                "共有リンク grant の role が不正: {grole}"
            )));
        };
        // 当該リンク分の grant 行を落とす。
        sqlx::query("DELETE FROM node_share_link_grant WHERE link_id = $1 AND user_id = $2")
            .bind(link_id)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        // 同 (node,user,role) を保持する他 active リンク由来の grant が無ければ via_link を剥奪。
        let remaining: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM node_share_link_grant g \
             JOIN node_share_link l ON l.link_id = g.link_id \
             WHERE g.node_id = $1 AND g.user_id = $2 AND g.role = $3 AND g.tenant_id = $4 \
               AND l.revoked_at IS NULL AND (l.expires_at IS NULL OR l.expires_at > $5)",
        )
        .bind(node_id)
        .bind(user_id)
        .bind(&grole)
        .bind(&ctx.tenant_id)
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;
        if remaining == 0 {
            // 最後の active grant → via_link タプルを剥奪（失敗は ? 伝播で tx 巻き戻し＝fail-closed）。
            self.authz
                .delete_tuple(&ns.user(user_id), role.relation_via_link(), &obj)
                .await?;
        }
        self.finalize_share_link_tx(
            tx,
            ctx,
            node_id,
            kind_of(&obj),
            "node.share_link.grant.revoke",
            json!({ "link_id": link_id, "user_id": user_id, "role": role }),
            trace_id,
        )
        .await
    }
}

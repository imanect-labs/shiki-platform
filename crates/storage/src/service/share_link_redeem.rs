//! StorageService: 共有リンク（#342）— パスワード解錠（redeem）と per-user 参照カウント剥奪。
//!
//! - [`StorageService::redeem_share_link`] — token＋パスワードを検証し、呼び出しユーザーへ per-user
//!   タプルを発行する（authenticated なら誰でも・失敗は一律 403）。
//! - [`StorageService::reconcile_user_grants_for_link`] — リンク失効/期限失効時に、そのリンクの
//!   redeem 済み per-user タプルを **(node,user,role) 単位で参照カウント**して剥奪する（他 active
//!   リンクが同じ付与を保持していれば FGA タプルは消さない）。
//!
//! 共有ヘルパ（`broad_subject`/`verify_password`）は [`super::share_link`] に定義している。

#[allow(clippy::wildcard_imports)]
use super::*;

use super::share_link_util::verify_password;
use crate::model::GeneralAccessLevel;

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
        let row: Option<RedeemRow> = sqlx::query_as(
            "SELECT link_id, node_id, org, kind, audience, role, expires_at, password_hash \
             FROM node_share_link \
             WHERE token = $1 AND tenant_id = $2 \
               AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > now())",
        )
        .bind(token)
        .bind(&ctx.tenant_id)
        .fetch_optional(&self.db)
        .await?;
        // 存在秘匿: 見つからないトークンは一律 Forbidden。
        let Some(row) = row else {
            return Err(StorageError::Forbidden);
        };
        // redeem はパスワード付きリンク専用（broad リンクは通常 ReBAC で開く）。
        let Some(hash) = row.password_hash.as_deref() else {
            return Err(StorageError::Forbidden);
        };
        let (Some(level), Some(role), Some(kind)) = (
            GeneralAccessLevel::parse(&row.audience),
            ShareRole::parse(&row.role),
            NodeKind::parse(&row.kind),
        ) else {
            return Err(StorageError::Forbidden);
        };
        // audience 該当性: organization は当該組織のメンバーのみ、anyone は認証済みなら誰でも、
        // restricted（付与ゼロのポインタ）は redeem 不可。
        match level {
            GeneralAccessLevel::Anyone => {}
            GeneralAccessLevel::Organization => {
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
                    return Err(StorageError::Forbidden);
                }
            }
            GeneralAccessLevel::Restricted => return Err(StorageError::Forbidden),
        }
        // パスワード検証（Argon2id・定数時間）。不一致/未指定は generic Forbidden。
        if !verify_password(password.unwrap_or(""), hash) {
            return Err(StorageError::Forbidden);
        }

        // per-user タプルを発行し、redeem 台帳へ記録する。
        let ns = ctx.ns();
        let obj = node_fga_object(&ns, kind, row.node_id);
        let subject = ns.user(&ctx.principal.id);
        // この (node,user,role) が既に redeem 台帳に載っているか（別リンク経由の先行 redeem）。
        let prior: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM node_share_link_grant \
             WHERE node_id = $1 AND user_id = $2 AND role = $3)",
        )
        .bind(row.node_id)
        .bind(&ctx.principal.id)
        .bind(role.as_str())
        .fetch_one(&self.db)
        .await?;
        let granted = self
            .authz
            .write_tuple(&subject, role.relation(), &obj)
            .await?;
        // 台帳記録は `granted OR prior` のときだけ（＝redeem 由来の付与のみ台帳に載せ、既存の
        // 明示共有を台帳に載せない＝後の失効で明示共有を誤剥奪しない）。複数リンクが同一 (node,
        // user,role) を redeem し得るので、先行 redeem 済み（prior）なら本リンク分も必ず記録する。
        let record = granted || prior;
        let persisted = self
            .persist_redeem(
                ctx,
                row.link_id,
                row.node_id,
                kind,
                role,
                row.expires_at,
                level,
                record,
                trace_id,
            )
            .await;
        if let Err(e) = persisted {
            if granted {
                let _ = self
                    .authz
                    .delete_tuple(&subject, role.relation(), &obj)
                    .await;
            }
            return Err(e);
        }
        if row.expires_at.is_some() {
            self.expiry_notify.notify_one();
        }
        Ok(())
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
                // 最後の active grant → FGA タプルを剥奪（失敗は ? 伝播で tx 巻き戻し＝fail-closed）。
                self.authz
                    .delete_tuple(&ns.user(user_id), role.relation(), obj)
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

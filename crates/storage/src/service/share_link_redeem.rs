//! StorageService: 共有リンク（#342）— パスワード解錠（redeem）。
//!
//! - [`StorageService::redeem_share_link`] — token＋パスワードを検証し、呼び出しユーザーへ per-user
//!   タプルを発行する（authenticated なら誰でも・失敗は一律 403）。**#376 で `lock_node` 下の
//!   単一 tx へ再構成**し、owner の失効（revoke/期限失効）と直列化した。
//!
//! grant 台帳の管理（per-user 個別取消・失効時の参照カウント剥奪・owner 向け一覧）は
//! [`super::share_link_grant`]、共有ヘルパ（`verify_password` 等）は [`super::share_link_util`]。

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
    password_hash: Option<String>,
}

/// `verify_redeem`（ロック**外**）が返す候補。ロック外の読みなので、ここに含まれない
/// role/audience/expires_at は信用しない ——「速い否定」のための候補特定に過ぎず、真実は
/// [`StorageService::reverify_redeem_locked`] がロック下で確定する（#376）。
struct RedeemCandidate {
    link_id: Uuid,
    node_id: Uuid,
    /// `verify_password` を通した PHC 文字列。ロック下で同一性を再確認する。
    password_hash: String,
}

/// `lock_node` 下で読み直したリンク行（生の列。パースは `reverify_redeem_locked`）。
#[derive(sqlx::FromRow)]
struct LockedRow {
    kind: String,
    audience: String,
    role: String,
    expires_at: Option<DateTime<Utc>>,
    password_hash: Option<String>,
}

/// ロック下で確定したリンクの状態（付与に必要な項目）。
struct LockedLink {
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
    ///
    /// **直列化（#376）**: 付与は `lock_node` 配下の単一 tx で行い、owner の `revoke_share_link` /
    /// `expire_node_links` / `revoke_share_link_grant` と直列化する。これらは同じ advisory lock を
    /// 取るため、redeem は「失効の完全に前」か「完全に後」のどちらかにしか入らない。
    ///
    /// **レート制限・token 引き・audience 検査・Argon2id 検証はロック外**に置く。Argon2id は 1 回
    /// 50〜100ms の CPU なので、ロック下で回すと任意の認証ユーザーが誤パスワードを連打するだけで
    /// owner の共有操作（create/revoke/extend/失効）を node 単位で止められる（DoS）。
    pub async fn redeem_share_link(
        &self,
        ctx: &AuthContext,
        token: &str,
        password: Option<&str>,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        let c = self.verify_redeem(ctx, token, password, trace_id).await?;
        let node = c.node_id.to_string();

        let mut tx = self.db.begin().await?;
        self.lock_node(&mut tx, c.node_id).await?;

        // ロック取得までの間に失効/期限失効/個別取消が走り得る。ロック下の再読みだけが真実。
        let Some(l) = self.reverify_redeem_locked(&mut tx, ctx, &c).await? else {
            tx.rollback().await?;
            return Err(self
                .deny_redeem(
                    ctx,
                    "link_inactive",
                    "node",
                    &node,
                    Some(c.link_id),
                    trace_id,
                )
                .await);
        };

        // 台帳 upsert（#375 の主防御）。deny 台帳の行（revoked_at IS NOT NULL）は **復活させない**
        // ——`DO UPDATE` の WHERE で弾かれると 0 行＝`None` になり一律 Forbidden へ倒す（fail-closed）。
        // 事前 SELECT で判定しないのは、同じ真実を 2 箇所に書くと片方が腐るから。この 1 文が原子的
        // なので、仮に将来ロックを外しても取消済みユーザーの再 redeem は通らない（多層防御）。
        let recorded: Option<String> = sqlx::query_scalar(
            "INSERT INTO node_share_link_grant \
               (link_id, node_id, user_id, tenant_id, kind, role) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (link_id, user_id) DO UPDATE SET \
               role = EXCLUDED.role, granted_at = now() \
               WHERE node_share_link_grant.revoked_at IS NULL \
             RETURNING role",
        )
        .bind(c.link_id)
        .bind(c.node_id)
        .bind(&ctx.principal.id)
        .bind(&ctx.tenant_id)
        .bind(l.kind.as_str())
        .bind(l.role.as_str())
        .fetch_optional(&mut *tx)
        .await?;
        if recorded.is_none() {
            tx.rollback().await?;
            return Err(self
                .deny_redeem(
                    ctx,
                    "grant_revoked",
                    l.kind.as_str(),
                    &node,
                    Some(c.link_id),
                    trace_id,
                )
                .await);
        }

        audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: "node.share_link.redeem",
                object_type: l.kind.as_str(),
                object_id: &node,
                decision: Decision::Allow,
                trace_id,
                metadata: json!({ "link_id": c.link_id, "audience": l.level, "role": l.role }),
            },
            Chain::Yes,
        )
        .await?;

        // redeem 由来は **via_link 専用 relation**（viewer_via_link / editor_via_link）で発行する
        // （#366）。明示共有（viewer / editor）とはタプルの出自が分かれるため、リンク失効時の
        // per-user reconcile が明示共有を誤剥奪しない（B-2 根治）。
        //
        // タプルは **ロック保持下・commit 前**に張る（#376 の核心）。「commit → write_tuple」だと、
        // commit でロックが解放された隙に revoke がロックを取り、台帳を読み（行はある）→
        // delete_tuple（まだ無いので no-op）→ 行を revoked → commit、その後で write_tuple が着地し、
        // **台帳から参照されない孤児タプル＝恒久 fail-open** になる。
        let ns = ctx.ns();
        let obj = node_fga_object(&ns, l.kind, c.node_id);
        let subject = ns.user(&ctx.principal.id);
        let write = self
            .authz
            .write_tuple(&subject, l.role.relation_via_link(), &obj)
            .await;

        // **書き込み結果が不確定なエラーでも補償する**（Codex P1）。timeout や接続切断では
        // 「FGA はタプルを適用したが応答だけ失われた」が起こり得る。ここで tx をロールバックすると
        // 台帳行が無いままタプルが残り、per-user 取消もリンク失効も台帳から対象を見つけられないため
        // **恒久 fail-open** になる。delete_tuple は冪等（未存在は成功扱い）なので、適用されていなくても
        // 無害。ロック保持下で消すので、この間に別経路がタプルを張り直すこともない。
        let granted = match write {
            Ok(granted) => granted,
            Err(e) => {
                let _ = self
                    .authz
                    .delete_tuple(&subject, l.role.relation_via_link(), &obj)
                    .await;
                tx.rollback().await?;
                return Err(e.into());
            }
        };

        if let Err(e) = tx.commit().await {
            // 補償は best-effort。取りこぼしても、次回 redeem が台帳行を必ず書き直す（記録は無条件）
            // ので孤児タプルは自己修復する。
            //
            // 参照カウントを見ずに消してよい理由（CodeRabbit）: `granted == true` は
            // 「この呼び出しで**新規に**張った」＝直前までタプルが存在しなかったことを意味するので、
            // 同じ (node,user,role) を他リンクの live grant が頼っていることはあり得ない。
            // 既存タプルなら write_tuple は false を返し、ここには入らない。
            if granted {
                let _ = self
                    .authz
                    .delete_tuple(&subject, l.role.relation_via_link(), &obj)
                    .await;
            }
            return Err(e.into());
        }
        if l.expires_at.is_some() {
            self.expiry_notify.notify_one();
        }
        Ok(())
    }

    /// redeem のレート制限・token 引き（テナント/org スコープ）・audience メンバーシップ・パスワードを
    /// 検証し、ロック下で再確認するための候補を返す（#342）。失敗は一律 `Forbidden`＋deny 監査
    /// （オラクル防止）。**ロック外**で走る（Argon2id をロック下に入れない・#376）。
    async fn verify_redeem(
        &self,
        ctx: &AuthContext,
        token: &str,
        password: Option<&str>,
        trace_id: Option<&str>,
    ) -> Result<RedeemCandidate, StorageError> {
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
            "SELECT link_id, node_id, org, kind, audience, role, password_hash \
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
        let Some(level) = GeneralAccessLevel::parse(&row.audience) else {
            return Err(deny("corrupt_row").await);
        };
        if ShareRole::parse(&row.role).is_none() || NodeKind::parse(&row.kind).is_none() {
            return Err(deny("corrupt_row").await);
        }
        // audience 該当性: organization / anyone（A-2 で organization へ縮退）は当該組織のメンバーの
        // み、restricted（付与ゼロのポインタ）は redeem 不可。org メンバーシップはリンク機構と競合
        // しない FGA 状態なので、ロック下で取り直さない（ロック保持時間を HTTP 往復ぶん延ばさない）。
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
        Ok(RedeemCandidate {
            link_id: row.link_id,
            node_id: row.node_id,
            password_hash: hash.to_string(),
        })
    }

    /// `lock_node` 取得後にリンクを再読みし、redeem を許してよいかを確定する（#376）。
    /// active でない／パスワードハッシュが差し替わった／行が破損している場合は `None`（一律 403）。
    async fn reverify_redeem_locked(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        ctx: &AuthContext,
        c: &RedeemCandidate,
    ) -> Result<Option<LockedLink>, StorageError> {
        let row: Option<LockedRow> = sqlx::query_as(
            "SELECT kind, audience, role, expires_at, password_hash FROM node_share_link \
             WHERE link_id = $1 AND tenant_id = $2 AND org = $3 \
               AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > now())",
        )
        .bind(c.link_id)
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .fetch_optional(&mut **tx)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        // パスワードはロック外で検証済み。ハッシュが差し替わっていたら「古いパスワードで解錠された」
        // ことになるので拒否する（パスワード変更 API は現状無いが、足したときに黙って穴が開かない）。
        if row.password_hash.as_deref() != Some(c.password_hash.as_str()) {
            return Ok(None);
        }
        let (Some(level), Some(role), Some(kind)) = (
            GeneralAccessLevel::parse(&row.audience),
            ShareRole::parse(&row.role),
            NodeKind::parse(&row.kind),
        ) else {
            return Ok(None);
        };
        if matches!(level, GeneralAccessLevel::Restricted) {
            return Ok(None);
        }
        Ok(Some(LockedLink {
            kind,
            role,
            level,
            expires_at: row.expires_at,
        }))
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
}

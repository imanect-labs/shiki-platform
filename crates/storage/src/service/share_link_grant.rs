//! StorageService: 共有リンクの redeem 台帳の owner 向け可視化と per-user 取り消し（#369 C-3）。
//!
//! - [`StorageService::list_share_link_grants`] — リンクを redeem した user 一覧（owner ゲート）。
//! - [`StorageService::revoke_share_link_grant`] — 特定 user の redeem を個別に取り消す（owner ゲート）。
//!
//! redeem 由来の付与は via_link 専用 relation（#366）なので、取り消しは他 active リンクの参照カウントを
//! 見て「最後の 1 本」なら via_link タプルを剥奪する。明示共有（別 relation）には決して触れない。

#[allow(clippy::wildcard_imports)]
use super::*;

use super::share_link_util::kind_of;
use crate::model::ShareLinkGrant;

/// grant 1 行（user_id・表示名・付与時刻）。
#[derive(sqlx::FromRow)]
struct GrantRow {
    user_id: String,
    display_name: Option<String>,
    granted_at: DateTime<Utc>,
}

impl StorageService {
    /// リンクを redeem した user 一覧を返す（owner 権限）。表示名は `directory_user` から解決する
    /// （無ければ `None`）。broad リンクは台帳を持たないため常に空。
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
            // 破損行は黙って消さない（監査可能に残す）。
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
        // 同 (node,user,role) を保持する他 active リンク由来の grant が残っていなければ via_link を剥奪。
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

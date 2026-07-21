//! StorageService: 共有リンク（#342）— 有効期限の失効処理（遅延失効・イベント駆動タイマ）。
//!
//! - [`StorageService::enforce_share_link_expiry`] — セッション開始点（`get_metadata` 前段）の
//!   遅延失効（defense-in-depth）。当該 node に期限切れ active リンクがあれば reconcile して
//!   broad タプル・per-user タプルを剥奪し、リンクを失効確定する。
//! - [`StorageService::revoke_expired_share_links`] — イベント駆動タイマから。期限切れリンクを
//!   **node 単位**にまとめて処理する（1 本の失効が同 node の他 active を巻き込まない）。
//! - [`StorageService::next_share_link_expiry`] — タイマの次回起床時刻（active リンクの最小期限）。

#[allow(clippy::wildcard_imports)]
use super::*;

/// 期限切れ active リンクを持つ node（タイマの処理単位）。
#[derive(sqlx::FromRow)]
struct ExpiredNode {
    node_id: Uuid,
    tenant_id: String,
    org: String,
    kind: String,
}

impl StorageService {
    /// セッション開始点の遅延失効（#342・defense-in-depth）。当該 node に期限切れ active リンクが
    /// あれば broad タプルを reconcile し、期限切れリンクの per-user タプルを剥奪して失効確定する。
    ///
    /// 期限切れリンクが無ければ即返す（node index の軽いプローブ・一般アクセスを持たない大多数の
    /// node ではほぼ無コスト）。長寿命セッションの厳密失効はイベント駆動タイマが担う。
    pub(crate) async fn enforce_share_link_expiry(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        kind: NodeKind,
    ) -> Result<(), StorageError> {
        let now = Utc::now();
        let expired: Vec<Uuid> = sqlx::query_scalar(
            "SELECT link_id FROM node_share_link \
             WHERE node_id = $1 AND tenant_id = $2 \
               AND revoked_at IS NULL AND expires_at IS NOT NULL AND expires_at <= $3",
        )
        .bind(node_id)
        .bind(&ctx.tenant_id)
        .bind(now)
        .fetch_all(&self.db)
        .await?;
        if expired.is_empty() {
            return Ok(());
        }
        let ns = ctx.ns();
        let obj = node_fga_object(&ns, kind, node_id);
        let added = self
            .expire_node_links(&ns, &obj, node_id, &ctx.tenant_id, &ctx.org, &expired, now)
            .await?;
        // 遅延失効はチェーン監査しない（タイマ側が監査する・#339 踏襲）。
        let _ = added; // expire では add は起きない（remove のみ）。
        Ok(())
    }

    /// 期限切れリンクを剥奪し失効確定する（#342・イベント駆動タイマから）。
    ///
    /// admin プレーン（`AuthContext` 無し）。期限切れ active リンクを持つ node を LIMIT で束ね、
    /// node ごとに `Namespace::for_tenant` で識別子を再構成して処理する（越境しない）。返り値は
    /// 失効したリンク件数（ログ用の概数）。
    pub async fn revoke_expired_share_links(
        &self,
        now: DateTime<Utc>,
    ) -> Result<u64, StorageError> {
        let nodes: Vec<ExpiredNode> = sqlx::query_as(
            "SELECT DISTINCT node_id, tenant_id, org, kind FROM node_share_link \
             WHERE revoked_at IS NULL AND expires_at IS NOT NULL AND expires_at <= $1 \
             LIMIT 500",
        )
        .bind(now)
        .fetch_all(&self.db)
        .await?;

        let mut count: u64 = 0;
        for n in nodes {
            let Some(kind) = NodeKind::parse(&n.kind) else {
                continue;
            };
            let ns = Namespace::for_tenant(&n.tenant_id);
            let obj = node_fga_object(&ns, kind, n.node_id);
            let expired: Vec<Uuid> = sqlx::query_scalar(
                "SELECT link_id FROM node_share_link \
                 WHERE node_id = $1 AND tenant_id = $2 \
                   AND revoked_at IS NULL AND expires_at IS NOT NULL AND expires_at <= $3",
            )
            .bind(n.node_id)
            .bind(&n.tenant_id)
            .bind(now)
            .fetch_all(&self.db)
            .await?;
            if expired.is_empty() {
                continue;
            }
            self.expire_node_links(&ns, &obj, n.node_id, &n.tenant_id, &n.org, &expired, now)
                .await?;
            // 失効の監査（system ctx・非チェーン。create/redeem はチェーン監査済み）。
            let sctx = system_ctx(&n.tenant_id, &n.org, "system");
            let _ = self
                .audit
                .record(
                    &sctx,
                    AuditEntry {
                        action: "node.share_link.expire",
                        object_type: n.kind.as_str(),
                        object_id: &n.node_id.to_string(),
                        decision: Decision::Allow,
                        trace_id: None,
                        metadata: json!({ "count": expired.len() }),
                    },
                )
                .await;
            count += expired.len() as u64;
        }
        Ok(count)
    }

    /// 1 node の期限切れリンク群を 1 tx で失効確定する（reconcile broad＋per-user 剥奪＋revoked_at）。
    /// コミット失敗時は付与タプル（通常は無い）を補償剥奪する。付与タプルを返す。
    #[allow(clippy::too_many_arguments)]
    async fn expire_node_links(
        &self,
        ns: &Namespace<'_>,
        obj: &FgaObject,
        node_id: Uuid,
        tenant_id: &str,
        org: &str,
        expired: &[Uuid],
        now: DateTime<Utc>,
    ) -> Result<Vec<(Subject, Relation)>, StorageError> {
        let mut tx = self.db.begin().await?;
        self.lock_node(&mut tx, node_id).await?;
        // 期限切れリンクは now 基準で非 active → reconcile で broad タプルが落ちる。
        let added = self
            .reconcile_broad(&mut tx, ns, obj, node_id, tenant_id, org, now)
            .await?;
        for link_id in expired {
            if let Err(e) = self
                .reconcile_user_grants_for_link(&mut tx, ns, obj, *link_id, node_id, tenant_id, now)
                .await
            {
                self.compensate_broad(obj, &added).await;
                return Err(e);
            }
            sqlx::query(
                "UPDATE node_share_link SET revoked_at = now() \
                 WHERE link_id = $1 AND revoked_at IS NULL",
            )
            .bind(link_id)
            .execute(&mut *tx)
            .await?;
        }
        if let Err(e) = tx.commit().await {
            self.compensate_broad(obj, &added).await;
            return Err(e.into());
        }
        Ok(added)
    }

    /// 次に失効する共有リンクの時刻（active リンクの最小 `expires_at`）。タイマの次回起床に使う。
    /// 期限付き active リンクが無ければ `None`。per-user 台帳の期限はリンク期限を追随するため対象外。
    pub async fn next_share_link_expiry(&self) -> Result<Option<DateTime<Utc>>, StorageError> {
        let row: (Option<DateTime<Utc>>,) = sqlx::query_as(
            "SELECT MIN(expires_at) FROM node_share_link \
             WHERE revoked_at IS NULL AND expires_at IS NOT NULL",
        )
        .fetch_one(&self.db)
        .await?;
        Ok(row.0)
    }
}

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
        // 毒行（未知 kind＝FgaObject を再構成できない破損行）は sweep 対象から外す。残すと
        // next_share_link_expiry が過去時刻を返し続け、タイマが全速ループに入る（B-1）。
        let nodes: Vec<ExpiredNode> = sqlx::query_as(
            "SELECT DISTINCT node_id, tenant_id, org, kind FROM node_share_link \
             WHERE revoked_at IS NULL AND expires_at IS NOT NULL AND expires_at <= $1 \
               AND kind IN ('file', 'folder') \
             LIMIT 500",
        )
        .bind(now)
        .fetch_all(&self.db)
        .await?;

        let mut count: u64 = 0;
        let mut failures: u32 = 0;
        for n in nodes {
            let Some(kind) = NodeKind::parse(&n.kind) else {
                continue; // kind IN (...) で弾いているが二重の防御。
            };
            let ns = Namespace::for_tenant(&n.tenant_id);
            let obj = node_fga_object(&ns, kind, n.node_id);
            // 1 node の失敗で sweep 全体を止めない（head-of-line blocking の解消・B-1）。DB/FGA
            // 障害中は当該 node をスキップして次へ進み、タイマ側が backoff で再試行する。
            let expired: Vec<Uuid> = match sqlx::query_scalar(
                "SELECT link_id FROM node_share_link \
                 WHERE node_id = $1 AND tenant_id = $2 \
                   AND revoked_at IS NULL AND expires_at IS NOT NULL AND expires_at <= $3",
            )
            .bind(n.node_id)
            .bind(&n.tenant_id)
            .bind(now)
            .fetch_all(&self.db)
            .await
            {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, node_id = %n.node_id, "期限切れリンクの取得に失敗（次ノードへ）");
                    failures += 1;
                    continue;
                }
            };
            if expired.is_empty() {
                continue;
            }
            if let Err(e) = self
                .expire_node_links(&ns, &obj, n.node_id, &n.tenant_id, &n.org, &expired, now)
                .await
            {
                tracing::warn!(error = %e, node_id = %n.node_id, "node の共有リンク失効に失敗（次ノードへ）");
                failures += 1;
                continue;
            }
            // 失効の監査（system ctx・非チェーン。create/redeem はチェーン監査済み）。
            let sctx = system_ctx(&n.tenant_id, &n.org, "system");
            if let Err(e) = self
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
                .await
            {
                tracing::warn!(error = %e, node_id = %n.node_id, "共有リンク失効の監査記録に失敗");
            }
            count += expired.len() as u64;
        }
        if failures > 0 {
            tracing::warn!(
                failures,
                revoked = count,
                "共有リンク失効 sweep で一部ノードが失敗しました（#342 B-1）"
            );
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
        // ① 失効確定を **先に**、ロック保持下で期限を再確認しながら行う（SELECT〜ロック取得の間に
        // owner が延長した場合、reconcile_broad は延長リンクを active 扱いでタプルを残すため、無条件に
        // revoked_at を立てると「revoked なのに broad タプルが残る」不整合になる・Codex P1）。
        //
        // ⚠️ per-user 剥奪より **前**でなければならない（#375）。台帳行はソフト失効させる（deny 台帳）
        // ため、延長されたリンクの grant に触ると「まだ active なリンクの解錠者が全員 deny 台帳に載り、
        // 二度と再 redeem できない」という永久追放になる。0 行だったリンクには一切触れない。
        let mut revoked: Vec<Uuid> = Vec::with_capacity(expired.len());
        for link_id in expired {
            let updated = sqlx::query(
                "UPDATE node_share_link SET revoked_at = now() \
                 WHERE link_id = $1 AND revoked_at IS NULL \
                   AND expires_at IS NOT NULL AND expires_at <= $2",
            )
            .bind(link_id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
            if updated.rows_affected() > 0 {
                revoked.push(*link_id);
            }
        }
        // ② broad タプルを reconcile（①で確定した active 集合を反映する）。
        let added = self
            .reconcile_broad(&mut tx, ns, obj, node_id, tenant_id, org, now)
            .await?;
        // ③ 実際に失効したリンクの per-user タプルだけを参照カウントして剥奪する。
        for link_id in &revoked {
            if let Err(e) = self
                .reconcile_user_grants_for_link(&mut tx, ns, obj, *link_id, node_id, tenant_id, now)
                .await
            {
                self.compensate_broad(obj, &added).await;
                return Err(e);
            }
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
             WHERE revoked_at IS NULL AND expires_at IS NOT NULL \
               AND kind IN ('file', 'folder')",
        )
        .fetch_one(&self.db)
        .await?;
        Ok(row.0)
    }
}

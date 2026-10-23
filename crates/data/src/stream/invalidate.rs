//! 認可側の変更の世代（data-platform §9.4・PIT-70 ③）。
//!
//! SSE の購読は認可材料を TTL だけ持ち越すが、**ロール変更・共有解除・テーブル権限の変更を検知した
//! 購読は TTL を待たずに引き直す**。検知はレプリカを跨ぐ必要がある（変更を受けたレプリカと購読を
//! 持つレプリカは別）ので、DB の世代（`data_stream_authz_epoch`）を進め、配信側は毎バッチ
//! （250 ms）世代を読み比べる。
//!
//! 世代を進める入口は [`crate::DataStore::notify_authz_changed`]。本 crate の中の変更（レコードの個別
//! 共有・共有解除・テーブル削除）は自分で呼ぶ。外の変更（IdP 同期によるロールの付け外し・テーブルの
//! ReBAC の付け外し）は変えた側が呼ぶ。

use std::collections::HashMap;

use authz::AuthContext;
use uuid::Uuid;

use crate::store::DataStore;
use crate::{map_db, DataError};

/// テナント全体の世代の `scope`。
pub(crate) const TENANT_SCOPE: Uuid = Uuid::nil();

impl DataStore {
    /// 認可側の変更を配信へ知らせる（世代を進める）。`table_id = None` はテナント全体（ロールの変更）。
    ///
    /// 呼ばなくても購読は TTL で引き直すので権限は最大 TTL 遅れで反映されるが、呼べば次のバッチ
    /// （250 ms 以内）で反映される。失敗しても書込の結果は覆さない（呼び出し側はログに留める）。
    pub async fn notify_authz_changed(
        &self,
        ctx: &AuthContext,
        table_id: Option<Uuid>,
    ) -> Result<(), DataError> {
        self.bump_authz_epoch(&ctx.tenant_id, table_id).await
    }

    pub(crate) async fn bump_authz_epoch(
        &self,
        tenant_id: &str,
        table_id: Option<Uuid>,
    ) -> Result<(), DataError> {
        sqlx::query(
            "INSERT INTO data_stream_authz_epoch (tenant_id, scope) VALUES ($1, $2) \
             ON CONFLICT (tenant_id, scope) \
             DO UPDATE SET epoch = data_stream_authz_epoch.epoch + 1, updated_at = now()",
        )
        .bind(tenant_id)
        .bind(table_id.unwrap_or(TENANT_SCOPE))
        .execute(self.pools.for_tenant(tenant_id))
        .await
        .map_err(map_db)?;
        Ok(())
    }

    /// テナントの世代（scope → epoch）。
    pub(crate) async fn authz_epochs(
        &self,
        tenant_id: &str,
    ) -> Result<HashMap<Uuid, i64>, DataError> {
        let rows: Vec<(Uuid, i64)> =
            sqlx::query_as("SELECT scope, epoch FROM data_stream_authz_epoch WHERE tenant_id = $1")
                .bind(tenant_id)
                .fetch_all(self.pools.for_tenant(tenant_id))
                .await
                .map_err(map_db)?;
        Ok(rows.into_iter().collect())
    }
}

/// 前回見た世代から、テーブル `table_id` の購読を無効化すべきか。
pub(crate) fn changed_for(
    before: &HashMap<Uuid, i64>,
    now: &HashMap<Uuid, i64>,
    table_id: Uuid,
) -> bool {
    [TENANT_SCOPE, table_id]
        .iter()
        .any(|s| before.get(s) != now.get(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_for_tenant_or_table_scope() {
        let t = Uuid::new_v4();
        let other = Uuid::new_v4();
        let before: HashMap<Uuid, i64> = [(TENANT_SCOPE, 1), (t, 3)].into();
        let mut now = before.clone();
        assert!(!changed_for(&before, &now, t));
        now.insert(other, 1);
        assert!(!changed_for(&before, &now, t), "他テーブルの世代は無関係");
        now.insert(t, 4);
        assert!(changed_for(&before, &now, t));
        let mut tenant = before.clone();
        tenant.insert(TENANT_SCOPE, 2);
        assert!(
            changed_for(&before, &tenant, other),
            "テナント全体は全テーブルに効く"
        );
    }
}

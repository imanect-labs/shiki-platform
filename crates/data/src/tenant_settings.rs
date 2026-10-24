//! 構造化データのテナント設定（data-platform §5.4: 件数の上限はテナント設定で変更可能）。
//!
//! 変更は組織管理者だけが行う（呼び出し側＝管理 API のルート表が `OrgAdmin` で一律に掛ける。
//! ここは認可しない）。変更は監査ログに残す。

use authz::AuthContext;
use serde::{Deserialize, Serialize};
use serde_json::json;
use storage::audit::AuditAction;
use storage::audit::{AuditEntry, Decision};
use utoipa::ToSchema;

use crate::store::DataStore;
use crate::view_spec::{DEFAULT_MAX_VIEW_LOOKUP_COLUMNS, MAX_VIEW_LOOKUP_COLUMNS_CEILING};
use crate::{map_db, DataError};

/// 件数の上限として設定できる最大値。
pub const MAX_COUNT_CAP: u64 = 10_000_000;

/// 索引列数の上限として設定できる最大値（PIT-72）。
pub const MAX_INDEXED_FIELDS_CEILING: u32 = 1_000;

/// テナントの構造化データ設定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DataTenantSettings {
    /// 件数の上限（`null` は既定）。
    pub count_cap: Option<u64>,
    /// 実際に使われる件数の上限（既定を反映した値）。
    pub effective_count_cap: u64,
    /// テーブルあたりの索引列数の上限（`null` は既定 20・擬似列とリンクを除く・data-platform §10）。
    /// 超える索引の自動昇格は owner の明示の承認が要る。
    #[serde(default)]
    pub max_indexed_fields: Option<u32>,
    /// 実際に使われる索引列数の上限。
    #[serde(default)]
    pub effective_max_indexed_fields: u32,
    /// **製品の約束**（PIT-70）: SSE 購読中の権限の剥奪は、配信への反映が最大この時間（ミリ秒）
    /// 遅れる（購読中だけ認可材料を持ち越す TTL）。REST の読取は遅れない。管理画面に表示する。
    #[serde(default)]
    pub stream_revocation_delay_ms: u64,
    /// 1 ビューで表示（解決）できるルックアップ・ロールアップ列の上限（`null` は既定 12・data-platform §8.2）。
    #[serde(default)]
    pub max_view_lookup_columns: Option<u32>,
    /// 実際に使われるルックアップ・ロールアップ列の上限。
    #[serde(default)]
    pub effective_max_view_lookup_columns: u32,
}

/// `data_tenant_settings` の 1 行（件数の上限・索引列数の上限・ビューのルックアップ列の上限）。
type SettingsRow = (Option<i64>, Option<i32>, Option<i32>);

impl DataStore {
    /// テナントの件数の上限（行が無ければストアの既定）。
    pub(crate) async fn count_cap_for(
        &self,
        conn: &mut sqlx::PgConnection,
        tenant_id: &str,
    ) -> Result<u64, DataError> {
        let cap: Option<Option<i64>> =
            sqlx::query_scalar("SELECT count_cap FROM data_tenant_settings WHERE tenant_id = $1")
                .bind(tenant_id)
                .fetch_optional(&mut *conn)
                .await
                .map_err(map_db)?;
        Ok(cap
            .flatten()
            .and_then(|c| u64::try_from(c).ok())
            .unwrap_or(self.count_cap))
    }

    /// テナント設定を読む。
    pub async fn tenant_settings(
        &self,
        ctx: &AuthContext,
    ) -> Result<DataTenantSettings, DataError> {
        let row: Option<SettingsRow> = sqlx::query_as(
            "SELECT count_cap, max_indexed_fields, max_view_lookup_columns \
             FROM data_tenant_settings WHERE tenant_id = $1",
        )
        .bind(&ctx.tenant_id)
        .fetch_optional(self.pools.for_tenant(&ctx.tenant_id))
        .await
        .map_err(map_db)?;
        let (cap, idx, lookups) = row.unwrap_or((None, None, None));
        let count_cap = cap.and_then(|c| u64::try_from(c).ok());
        let max_indexed_fields = idx.and_then(|c| u32::try_from(c).ok());
        let max_view_lookup_columns = lookups.and_then(|c| u32::try_from(c).ok());
        Ok(self.settings_of(count_cap, max_indexed_fields, max_view_lookup_columns))
    }

    /// 1 ビューのルックアップ・ロールアップ列の上限（行が無ければ既定 12・ビューの保存時検証に使う）。
    pub(crate) async fn view_lookup_cap(&self, ctx: &AuthContext) -> Result<u32, DataError> {
        Ok(self
            .tenant_settings(ctx)
            .await?
            .effective_max_view_lookup_columns)
    }

    fn settings_of(
        &self,
        count_cap: Option<u64>,
        max_indexed_fields: Option<u32>,
        max_view_lookup_columns: Option<u32>,
    ) -> DataTenantSettings {
        DataTenantSettings {
            count_cap,
            effective_count_cap: count_cap.unwrap_or(self.count_cap),
            max_indexed_fields,
            effective_max_indexed_fields: max_indexed_fields
                .unwrap_or(u32::try_from(crate::index::DEFAULT_MAX_INDEXED_FIELDS).unwrap_or(20)),
            stream_revocation_delay_ms: u64::try_from(self.stream_config.authz_ttl.as_millis())
                .unwrap_or(u64::MAX),
            max_view_lookup_columns,
            effective_max_view_lookup_columns: max_view_lookup_columns
                .unwrap_or(DEFAULT_MAX_VIEW_LOOKUP_COLUMNS),
        }
    }

    /// 1 ビューのルックアップ・ロールアップ列の上限を変更する（`None` で既定 12 へ戻す・1〜100・監査に残す）。
    ///
    /// 呼び出し側が組織管理者であることを確認してから呼ぶこと（ここは認可しない）。下げても既存の
    /// ビューは消さない（次の保存から新しい上限で検証する）。
    pub async fn update_tenant_view_lookup_cap(
        &self,
        ctx: &AuthContext,
        max_view_lookup_columns: Option<u32>,
        trace_id: Option<&str>,
    ) -> Result<DataTenantSettings, DataError> {
        if max_view_lookup_columns.is_some_and(|c| c == 0 || c > MAX_VIEW_LOOKUP_COLUMNS_CEILING) {
            return Err(DataError::input_detail(crate::InvalidCode::InvalidInput, None, format!(
                "ルックアップ・ロールアップ列の上限は 1〜{MAX_VIEW_LOOKUP_COLUMNS_CEILING} で指定してください"
            )));
        }
        let v = max_view_lookup_columns.map(|c| i32::try_from(c).unwrap_or(i32::MAX));
        let mut tx = self
            .pools
            .for_tenant(&ctx.tenant_id)
            .begin()
            .await
            .map_err(map_db)?;
        sqlx::query(
            "INSERT INTO data_tenant_settings (tenant_id, max_view_lookup_columns) VALUES ($1, $2) \
             ON CONFLICT (tenant_id) DO UPDATE SET \
               max_view_lookup_columns = excluded.max_view_lookup_columns, updated_at = now()",
        )
        .bind(&ctx.tenant_id)
        .bind(v)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        storage::audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: AuditAction::DataTenantSettingsUpdate,
                object_type: "data_tenant_settings",
                object_id: &ctx.tenant_id,
                decision: Decision::Allow,
                trace_id,
                metadata: json!({ "max_view_lookup_columns": max_view_lookup_columns }),
            },
            storage::audit::Chain::Yes,
        )
        .await
        .map_err(|e| DataError::Internal(format!("audit: {e}")))?;
        tx.commit().await.map_err(map_db)?;
        self.tenant_settings(ctx).await
    }

    /// テーブルあたりの索引列数の上限を変更する（`None` で既定 20 へ戻す・1〜1,000・監査に残す）。
    ///
    /// 呼び出し側が組織管理者であることを確認してから呼ぶこと（ここは認可しない）。下げても既存の
    /// 索引は消さない（新しい自動昇格だけが承認を要するようになる）。
    pub async fn update_tenant_index_cap(
        &self,
        ctx: &AuthContext,
        max_indexed_fields: Option<u32>,
        trace_id: Option<&str>,
    ) -> Result<DataTenantSettings, DataError> {
        if max_indexed_fields.is_some_and(|c| c == 0 || c > MAX_INDEXED_FIELDS_CEILING) {
            return Err(DataError::input_detail(
                crate::InvalidCode::InvalidInput,
                None,
                format!("索引列数の上限は 1〜{MAX_INDEXED_FIELDS_CEILING} で指定してください"),
            ));
        }
        let v = max_indexed_fields.map(|c| i32::try_from(c).unwrap_or(i32::MAX));
        let mut tx = self
            .pools
            .for_tenant(&ctx.tenant_id)
            .begin()
            .await
            .map_err(map_db)?;
        sqlx::query(
            "INSERT INTO data_tenant_settings (tenant_id, max_indexed_fields) VALUES ($1, $2) \
             ON CONFLICT (tenant_id) DO UPDATE SET max_indexed_fields = excluded.max_indexed_fields, \
               updated_at = now()",
        )
        .bind(&ctx.tenant_id)
        .bind(v)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        storage::audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: AuditAction::DataTenantSettingsUpdate,
                object_type: "data_tenant_settings",
                object_id: &ctx.tenant_id,
                decision: Decision::Allow,
                trace_id,
                metadata: json!({ "max_indexed_fields": max_indexed_fields }),
            },
            storage::audit::Chain::Yes,
        )
        .await
        .map_err(|e| DataError::Internal(format!("audit: {e}")))?;
        tx.commit().await.map_err(map_db)?;
        self.tenant_settings(ctx).await
    }

    /// 件数の上限を変更する（`None` で既定へ戻す・1〜[`MAX_COUNT_CAP`]・監査に残す）。
    ///
    /// 呼び出し側が組織管理者であることを確認してから呼ぶこと（ここは認可しない）。
    pub async fn update_tenant_count_cap(
        &self,
        ctx: &AuthContext,
        count_cap: Option<u64>,
        trace_id: Option<&str>,
    ) -> Result<DataTenantSettings, DataError> {
        if count_cap.is_some_and(|c| c == 0 || c > MAX_COUNT_CAP) {
            return Err(DataError::input_detail(
                crate::InvalidCode::InvalidInput,
                None,
                format!("件数の上限は 1〜{MAX_COUNT_CAP} で指定してください"),
            ));
        }
        let cap = count_cap.map(|c| i64::try_from(c).unwrap_or(i64::MAX));
        let mut tx = self
            .pools
            .for_tenant(&ctx.tenant_id)
            .begin()
            .await
            .map_err(map_db)?;
        sqlx::query(
            "INSERT INTO data_tenant_settings (tenant_id, count_cap) VALUES ($1, $2) \
             ON CONFLICT (tenant_id) DO UPDATE SET count_cap = excluded.count_cap, updated_at = now()",
        )
        .bind(&ctx.tenant_id)
        .bind(cap)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        storage::audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: AuditAction::DataTenantSettingsUpdate,
                object_type: "data_tenant_settings",
                object_id: &ctx.tenant_id,
                decision: Decision::Allow,
                trace_id,
                metadata: json!({ "count_cap": count_cap }),
            },
            storage::audit::Chain::Yes,
        )
        .await
        .map_err(|e| DataError::Internal(format!("audit: {e}")))?;
        tx.commit().await.map_err(map_db)?;
        self.tenant_settings(ctx).await
    }
}

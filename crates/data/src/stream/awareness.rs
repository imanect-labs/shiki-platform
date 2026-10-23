//! awareness（他ユーザーのカーソル・選択範囲・data-platform §9.5）。
//!
//! **データを含まない**: 載せるのは主体・レコード id・列の参照名だけ。CRDT（Yjs）は採らない
//! （行・列単位で可視性が異なり authz モデルと衝突する）。編集はサーバ権威＋`rev` の楽観ロック。
//!
//! - 送る側: 指すレコードが本人に見え、列が本人に読めることを確かめてから登録する（主体ごとに 1 行を
//!   上書き・短命）。
//! - 受ける側: 配信（[`super::hub`]）が購読者ごとに、レコードの可視性（行述語）と列のマスクを判定し、
//!   見えないものは送らない（見えない行の id を awareness から漏らさない）。

use authz::AuthContext;
use uuid::Uuid;

use crate::store::DataStore;
use crate::{map_db, DataError};

/// 列の参照名の長さ上限（スキーマの key と同じ桁）。
const MAX_FIELD_KEY_LEN: usize = 128;

impl DataStore {
    /// 自分のカーソル位置を知らせる（viewer・データを含まない）。`record_id = None` は離脱。
    pub async fn post_awareness(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        record_id: Option<Uuid>,
        field: Option<String>,
        trace_id: Option<&str>,
    ) -> Result<(), DataError> {
        let az = self.authz_memo();
        let table = self
            .open_for_read(ctx, &az, table_id, "data.stream.awareness", trace_id)
            .await?;
        if let Some(f) = &field {
            if f.len() > MAX_FIELD_KEY_LEN || table.schema.field(f).is_none() {
                return Err(DataError::Invalid(format!(
                    "フィールド '{f}' はスキーマに存在しません"
                )));
            }
            if self.masked_fields(ctx, &az, &table).await?.contains(f) {
                return Err(DataError::Forbidden);
            }
        }
        if let Some(id) = record_id {
            // 見えない行は「無い」と同じ応答（存在オラクルを作らない）。
            self.select_visible_by_id(ctx, &az, &table, id)
                .await?
                .ok_or(DataError::NotFound)?;
        }
        sqlx::query(
            "INSERT INTO data_stream_awareness (tenant_id, table_id, principal, record_id, field_key) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (tenant_id, table_id, principal) DO UPDATE SET \
               record_id = excluded.record_id, field_key = excluded.field_key, updated_at = now()",
        )
        .bind(&ctx.tenant_id)
        .bind(table_id)
        .bind(&ctx.principal.id)
        .bind(record_id)
        .bind(field.as_deref())
        .execute(self.pools.for_tenant(&ctx.tenant_id))
        .await
        .map_err(map_db)?;
        Ok(())
    }
}

//! レコードの更新（Task 9.2/9.5）。1 件の更新 API と一括書込パスの更新（Task 15.7 の bulk upsert）が
//! **同じ入口**（[`DataStore::update_locked`]）を通る。
//!
//! 入口が 1 つなので、行述語つきロック・書込述語・`rev` の照合・マスク列への書込拒否・検証器・
//! FSM の status 直書き拒否・参照先の可視性（今回足した参照先だけ・見えない既存の参照先は残す・
//! [`crate::link_merge`]）・索引の差分更新・材料化・リビジョン・outbox・監査が
//! どちらの経路でも同じに効く（一括経路だけ抜ける不変条件を作らない）。

use authz::{AuthContext, Relation};
use serde_json::{Map, Value};
use sqlx::types::Json;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::field_key::encode_record;
use crate::index::state::{lock_plan, IndexPlan};
use crate::model::{DataRecord, DataTable};
use crate::policy::memo::AuthzMemo;
use crate::record::{map_unique, WrittenRow, WRITTEN_RETURNING};
use crate::revision::{diff_fields, insert_revision, RevisionInsert};
use crate::store::DataStore;
use crate::stream::outbox::{emit_record_changes, Before, ChangeKind, RecordChange};
use crate::validate::validate_record_data;
use crate::{map_db, DataError};

/// [`DataStore::update_locked`] の結果。
pub(crate) struct Updated {
    /// 更新後（変更が無ければ現在）のレコード。
    pub record: DataRecord,
    /// 書き換えた（変更が無ければ `None`）レコードの `record_int`（crate 内専用・外へ出さない）。
    pub record_int: Option<i64>,
}

impl DataStore {
    /// レコードを更新する（editor・merge patch・楽観ロック・リビジョン同時記録）。
    ///
    /// `patch` は変更フィールドのみの部分オブジェクト。`null` はフィールド除去
    /// （required は除去不可＝マージ後検証で拒否）。`expected_rev` 不一致は 409。
    pub async fn update_record(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        id: Uuid,
        patch: Value,
        expected_rev: i64,
        trace_id: Option<&str>,
    ) -> Result<DataRecord, DataError> {
        let az = self.authz_memo();
        self.require_in(
            &az,
            ctx,
            table_id,
            Relation::Editor,
            "data.record.update",
            trace_id,
        )
        .await?;
        let table = self.fetch_live(ctx, table_id).await?;
        let patch_obj = patch.as_object().ok_or_else(|| {
            DataError::Invalid("patch は JSON オブジェクトである必要があります".into())
        })?;
        let mut tx = self
            .pools
            .for_tenant(&ctx.tenant_id)
            .begin()
            .await
            .map_err(map_db)?;
        let plan = lock_plan(
            &mut tx,
            &ctx.tenant_id,
            table_id,
            table.keys,
            table.schema_version,
        )
        .await?;
        let out = self
            .update_locked(
                &mut tx,
                ctx,
                &az,
                &table,
                &plan,
                id,
                patch_obj,
                Some(expected_rev),
                trace_id,
            )
            .await?;
        if out.record_int.is_none() {
            // 変更なし（リビジョン・outbox を作らない）。
            tx.rollback().await.map_err(map_db)?;
        } else {
            tx.commit().await.map_err(map_db)?;
        }
        // 応答は読取と同じ出口を通す（書き手に見えない参照先の id・件数を応答で漏らさない・§7.2）。
        let masked = self.masked_fields(ctx, &az, &table).await?;
        let mut out = [out.record];
        self.project_read(ctx, &az, &table, &masked, &mut out)
            .await?;
        let [record] = out;
        Ok(record)
    }

    /// 呼び出し側の Tx 上で 1 件を更新する（テーブル行の `FOR KEY SHARE` は呼び出し側が [`lock_plan`] で取得済み）。
    ///
    /// テーブルの editor は呼び出し側が確認済みであること。`expected_rev = None` は `rev` を照合しない
    /// （行ロックの下の後勝ち。一括書込で `rev` を指定しなかった行）。
    #[allow(clippy::too_many_arguments)] // Tx・認可の文脈・計画・行識別・楽観ロックの束。
    #[allow(clippy::too_many_lines)] // 検証・ロック・マスク・リビジョン・索引を 1 Tx で順に行う。
    pub(crate) async fn update_locked(
        &self,
        tx: &mut PgConnection,
        ctx: &AuthContext,
        az: &AuthzMemo<'_>,
        table: &DataTable,
        plan: &IndexPlan,
        id: Uuid,
        patch_obj: &Map<String, Value>,
        expected_rev: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<Updated, DataError> {
        let table_id = table.id;
        // 行述語つきロック: 不可視行は NotFound（rev オラクル封じ・PIT-21）。
        let current = self
            .lock_visible_by_id(ctx, az, tx, table, id)
            .await?
            .ok_or(DataError::NotFound)?;
        if !self.write_allowed(ctx, az, table, id).await? {
            return Err(DataError::Forbidden);
        }
        if let Some(expected) = expected_rev {
            if current.rev != expected {
                return Err(DataError::RevConflict {
                    current: current.rev,
                });
            }
        }
        let current = current.into_record(&table.schema);
        // マスク対象フィールドへの書込（変更）は拒否（盲目上書き防止・PIT-19）。
        let masked = self
            .ensure_fields_writable(ctx, az, table, Some(patch_obj))
            .await?;
        let old_map = current.data.as_object().cloned().unwrap_or_default();
        // リンク列: 書き手に見えない既存の参照先を残し、今回足した参照先だけを書き手の権限で検証する
        // （patch に無いリンク列は検証しない・crate::link_merge）。
        let patch_obj = &self
            .merge_link_patch(ctx, az, table, &old_map, patch_obj)
            .await?;
        // merge: null は除去・それ以外は上書き。
        let mut merged = old_map.clone();
        for (k, v) in patch_obj {
            if v.is_null() {
                merged.remove(k);
            } else {
                merged.insert(k.clone(), v.clone());
            }
        }
        let normalized = validate_record_data(
            ctx,
            &table.schema,
            &Value::Object(merged),
            self.resolver.as_ref(),
            self.max_multi_values,
        )
        .await
        // 検証は merge 後の完全形に掛かる: patch で名指ししていない列の名前は返さない（読めない列の
        // 既存値の誤りが列名を漏らさない）。
        .map_err(|e| crate::scrub_input_field(e, |k| patch_obj.contains_key(k)))?;
        // FSM 管理テーブルの status は直接 update できない（遷移 API 経由でのみ・Task 9.10）。
        Self::reject_fsm_status_write(table, patch_obj.keys())?;
        let patches = diff_fields(&old_map, &normalized);
        if patches.is_empty() {
            let mut out = [current];
            Self::apply_mask_records(&masked, &mut out);
            let [record] = out;
            return Ok(Updated {
                record,
                record_int: None,
            });
        }
        let new_rev = current.rev + 1;
        let new_stored = Value::Object(encode_record(&table.schema, &normalized));
        let written: WrittenRow = sqlx::query_as(&format!(
            "UPDATE data_record SET data = $4, rev = $5, updated_at = now() \
             WHERE tenant_int = $6 AND table_int = $7 AND tenant_id = $1 AND table_id = $2 \
               AND id = $3 \
             RETURNING {WRITTEN_RETURNING}"
        ))
        .bind(&ctx.tenant_id)
        .bind(table_id)
        .bind(id)
        .bind(Json(&new_stored))
        .bind(new_rev)
        .bind(table.keys.tenant_int)
        .bind(table.keys.table_int)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_unique)?;
        // 変更のあったフィールド（patches）の索引行と updated_at だけを書き換える（差分更新）。
        self.index_writer(plan, &table.schema, &ctx.tenant_id)
            .on_update(tx, written.record_int, &patches, written.row.updated_at)
            .await?;
        crate::derived::materialize::after_record_write(
            tx,
            &ctx.tenant_id,
            table,
            written.record_int,
            Some(&normalized),
        )
        .await?;
        let record_int = written.record_int;
        let row = written.row;
        insert_revision(
            tx,
            ctx,
            RevisionInsert {
                table_id,
                record_id: id,
                rev: new_rev,
                change_kind: "update",
                patch: &patches,
                trace_id,
            },
        )
        .await?;
        // outbox（同一 Tx・SSE の源）。before の可視性判定に旧 owner と行述語の参照列の旧値を添える。
        let old_stored = Value::Object(encode_record(&table.schema, &old_map));
        emit_record_changes(
            tx,
            ctx,
            table,
            &[RecordChange {
                record_id: id,
                record_int,
                rev: new_rev,
                kind: ChangeKind::Update,
                before: Some(Before {
                    owner: &current.owner,
                    stored: &old_stored,
                }),
                after: Some(Before {
                    owner: &current.owner,
                    stored: &new_stored,
                }),
            }],
            trace_id,
        )
        .await?;
        self.record_audit_record_on(
            tx,
            ctx,
            "data.record.update",
            id,
            trace_id,
            serde_json::json!({ "table_id": table_id, "rev": new_rev }),
        )
        .await?;
        // 応答はマスクを掛けて返す（書き手が読めない列の既存値を応答で漏らさない・PIT-19）。
        let mut out = [row.into_record(&table.schema)];
        Self::apply_mask_records(&masked, &mut out);
        let [record] = out;
        Ok(Updated {
            record,
            record_int: Some(record_int),
        })
    }
}

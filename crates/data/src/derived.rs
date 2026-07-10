//! 派生フィールド（lookup / computed）の読み出し解決（Task 9.3・PIT-20）。
//!
//! - **lookup**: 自テーブルの record_ref を辿り参照先フィールドを射影する。参照先
//!   テーブルの **row_policy を呼出ユーザー自身で再評価**し（[`crate::query`] 経由）、
//!   不可視の参照先は `null`（「参照なし」と区別できない＝存在オラクルなし）。
//!   テーブル間の権限素通り（データ層 confused-deputy・PIT-20）をここで塞ぐ。
//! - **computed**: 自行のフィールドから sum/concat を評価する（行内で閉じる）。
//!
//! v1 の lookup は**スカラーの実フィールドのみ**射影する（参照先の lookup/computed を
//! 辿らない＝再帰なし。多段射影が必要なら中間テーブルに実フィールドで持たせる）。

use std::collections::{HashMap, HashSet};

use authz::AuthContext;
use serde_json::Value;
use uuid::Uuid;

use crate::model::{ComputedOp, DataRecord, DataTable, FieldType};
use crate::store::DataStore;
use crate::DataError;

impl DataStore {
    /// 取得済み行へ派生フィールドを埋める（get/list の出口で呼ぶ）。
    pub(crate) async fn resolve_derived_fields(
        &self,
        ctx: &AuthContext,
        table: &DataTable,
        rows: &mut [DataRecord],
    ) -> Result<(), DataError> {
        if rows.is_empty() {
            return Ok(());
        }
        let has_derived = table
            .schema
            .fields
            .iter()
            .any(|f| matches!(f.field_type, FieldType::Lookup | FieldType::Computed));
        if !has_derived {
            return Ok(());
        }

        // --- lookup: (via_field, ref_table, target_field) ごとに参照先をバッチ解決 ---
        for f in &table.schema.fields {
            if f.field_type != FieldType::Lookup {
                continue;
            }
            let Some(def) = &f.lookup else { continue };
            let Some(via) = table.schema.field(&def.via_field) else {
                continue;
            };
            let Some(ref_table_id) = via.ref_table else {
                continue;
            };

            // 参照 id を収集。
            let mut ids: HashSet<Uuid> = HashSet::new();
            for row in rows.iter() {
                if let Some(Value::String(s)) = row.data.get(&def.via_field) {
                    if let Ok(id) = Uuid::parse_str(s) {
                        ids.insert(id);
                    }
                }
            }
            let resolved = if ids.is_empty() {
                HashMap::new()
            } else {
                self.lookup_targets(ctx, ref_table_id, &def.target_field, &ids)
                    .await?
            };
            for row in rows.iter_mut() {
                let value = row
                    .data
                    .get(&def.via_field)
                    .and_then(Value::as_str)
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .and_then(|id| resolved.get(&id).cloned())
                    .unwrap_or(Value::Null);
                if let Some(obj) = row.data.as_object_mut() {
                    obj.insert(f.name.clone(), value);
                }
            }
        }

        // --- computed: 行内評価 ---
        for f in &table.schema.fields {
            if f.field_type != FieldType::Computed {
                continue;
            }
            let Some(def) = &f.computed else { continue };
            for row in rows.iter_mut() {
                let value = match def.op {
                    ComputedOp::Sum => {
                        let sum: f64 = def
                            .fields
                            .iter()
                            .filter_map(|src| row.data.get(src).and_then(Value::as_f64))
                            .sum();
                        serde_json::json!(sum)
                    }
                    ComputedOp::Concat => {
                        let joined = def
                            .fields
                            .iter()
                            .filter_map(|src| row.data.get(src).and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("");
                        Value::String(joined)
                    }
                };
                if let Some(obj) = row.data.as_object_mut() {
                    obj.insert(f.name.clone(), value);
                }
            }
        }
        Ok(())
    }

    /// 参照先テーブルの target_field を、**参照先の行述語つき**でバッチ取得する。
    ///
    /// 参照先テーブル自体が削除済み・target が実フィールドでない場合は空（全て null 扱い）。
    /// 参照先テーブルの**テーブル ReBAC は要求しない**: 参照経由の射影可否は参照先の
    /// row_policy（および設計者が参照を張った意図）で決まる。row_policy 未定義の参照先は
    /// 全行射影可（テーブル可視性と同じ既定）。
    async fn lookup_targets(
        &self,
        ctx: &AuthContext,
        ref_table_id: Uuid,
        target_field: &str,
        ids: &HashSet<Uuid>,
    ) -> Result<HashMap<Uuid, Value>, DataError> {
        let ref_table = match self.fetch_live(ctx, ref_table_id).await {
            Ok(t) => t,
            Err(DataError::NotFound) => return Ok(HashMap::new()),
            Err(e) => return Err(e),
        };
        // 射影対象はスカラーの実フィールドのみ（派生の連鎖・再帰はしない）。
        let ok_target = ref_table
            .schema
            .field(target_field)
            .is_some_and(|tf| !matches!(tf.field_type, FieldType::Lookup | FieldType::Computed));
        if !ok_target || !crate::schema::is_valid_field_name(target_field) {
            return Ok(HashMap::new());
        }
        let id_vec: Vec<Uuid> = ids.iter().copied().collect();
        let rows = self
            .select_lookup_values(ctx, &ref_table, target_field, &id_vec)
            .await?;
        Ok(rows)
    }
}

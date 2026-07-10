//! リビジョン履歴（Task 9.5・追記型 changelog）。

use authz::{AuthContext, Relation};
use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use sqlx::types::Json;
use uuid::Uuid;

use crate::model::{FieldPatch, RecordRevision};
use crate::store::DataStore;
use crate::{map_db, DataError};

/// 2 つの保存形（フィールド名 → 値）からフィールド単位差分を計算する。
///
/// create は `old = {}`、delete は `new = {}` で呼ぶ。差分なしなら空。
pub(crate) fn diff_fields(old: &Map<String, Value>, new: &Map<String, Value>) -> Vec<FieldPatch> {
    let mut patches = Vec::new();
    // old 起点: 変更・削除。
    for (k, ov) in old {
        match new.get(k) {
            Some(nv) if nv == ov => {}
            Some(nv) => patches.push(FieldPatch {
                field: k.clone(),
                old: ov.clone(),
                new: nv.clone(),
            }),
            None => patches.push(FieldPatch {
                field: k.clone(),
                old: ov.clone(),
                new: Value::Null,
            }),
        }
    }
    // new 起点: 追加。
    for (k, nv) in new {
        if !old.contains_key(k) {
            patches.push(FieldPatch {
                field: k.clone(),
                old: Value::Null,
                new: nv.clone(),
            });
        }
    }
    patches.sort_by(|a, b| a.field.cmp(&b.field));
    patches
}

/// revision 行。
#[derive(sqlx::FromRow)]
struct RevisionRow {
    record_id: Uuid,
    rev: i64,
    changed_by: String,
    change_kind: String,
    patch: Json<Vec<FieldPatch>>,
    created_at: DateTime<Utc>,
}

impl DataStore {
    /// リビジョン履歴を取得する（テーブル viewer・rev 降順・keyset）。
    ///
    /// 行レベル認可（Task 9.3）導入後は record 本体と同じ述語で行の可視性を検査する
    /// （現段階の可視性はテーブル ReBAC のみ）。
    pub async fn list_revisions(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        record_id: Uuid,
        before_rev: Option<i64>,
        limit: i64,
        trace_id: Option<&str>,
    ) -> Result<Vec<RecordRevision>, DataError> {
        self.require(
            ctx,
            table_id,
            Relation::Viewer,
            "data.record.revisions",
            trace_id,
        )
        .await?;
        // テーブルが生存していること（削除済みテーブルの履歴を残存タプルで読ませない）。
        self.fetch_live(ctx, table_id).await?;
        let limit = limit.clamp(1, 200);
        let rows: Vec<RevisionRow> = sqlx::query_as(
            "SELECT record_id, rev, changed_by, change_kind, patch, created_at \
             FROM data_record_revision \
             WHERE tenant_id = $1 AND table_id = $2 AND record_id = $3 \
               AND ($4::bigint IS NULL OR rev < $4) \
             ORDER BY rev DESC LIMIT $5",
        )
        .bind(&ctx.tenant_id)
        .bind(table_id)
        .bind(record_id)
        .bind(before_rev)
        .bind(limit)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(rows
            .into_iter()
            .map(|r| RecordRevision {
                record_id: r.record_id,
                rev: r.rev,
                changed_by: r.changed_by,
                change_kind: r.change_kind,
                patch: r.patch.0,
                created_at: r.created_at,
            })
            .collect())
    }
}

/// リビジョン挿入の入力（record 書込と同一 Tx で使う）。
pub(crate) struct RevisionInsert<'a> {
    pub table_id: Uuid,
    pub record_id: Uuid,
    pub rev: i64,
    pub change_kind: &'a str,
    pub patch: &'a [FieldPatch],
    pub trace_id: Option<&'a str>,
}

/// リビジョン行を挿入する（record 書込と同一 Tx で呼ぶ内部ヘルパ）。
pub(crate) async fn insert_revision(
    conn: &mut sqlx::PgConnection,
    ctx: &AuthContext,
    ins: RevisionInsert<'_>,
) -> Result<(), DataError> {
    sqlx::query(
        "INSERT INTO data_record_revision \
         (tenant_id, record_id, table_id, rev, changed_by, change_kind, patch, trace_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(&ctx.tenant_id)
    .bind(ins.record_id)
    .bind(ins.table_id)
    .bind(ins.rev)
    .bind(&ctx.principal.id)
    .bind(ins.change_kind)
    .bind(Json(ins.patch))
    .bind(ins.trace_id)
    .execute(conn)
    .await
    .map_err(map_db)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(v: &Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn diff_detects_add_change_remove() {
        let old = map(&json!({"a": 1, "b": "x", "c": true}));
        let new = map(&json!({"a": 1, "b": "y", "d": [1]}));
        let patches = diff_fields(&old, &new);
        assert_eq!(patches.len(), 3);
        // ソート済み（b: 変更, c: 削除, d: 追加）。
        assert_eq!(patches[0].field, "b");
        assert_eq!(patches[0].old, json!("x"));
        assert_eq!(patches[0].new, json!("y"));
        assert_eq!(patches[1].field, "c");
        assert_eq!(patches[1].new, Value::Null);
        assert_eq!(patches[2].field, "d");
        assert_eq!(patches[2].old, Value::Null);
    }

    #[test]
    fn diff_empty_when_unchanged() {
        let doc = map(&json!({"a": 1}));
        assert!(diff_fields(&doc, &doc).is_empty());
    }

    #[test]
    fn create_and_delete_shapes() {
        let doc = map(&json!({"a": 1}));
        let created = diff_fields(&Map::new(), &doc);
        assert_eq!(created[0].old, Value::Null);
        let deleted = diff_fields(&doc, &Map::new());
        assert_eq!(deleted[0].new, Value::Null);
    }
}

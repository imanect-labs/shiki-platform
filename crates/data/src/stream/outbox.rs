//! レコード変更の outbox 発行（data-platform §9.1〜§9.3・Task 15.6）。
//!
//! 書込経路（create / update / delete / FSM 遷移 / 一括取込 / 材料化の再計算）は、本体と**同じ Tx で**
//! `data.record.changed` を outbox（`storage_event_outbox`）へ入れる。リレー（[`super::relay`]）が
//! テーブルのストリームへ採番し、配信（[`super::hub`]）が購読者ごとに可視性を判定して送る。
//!
//! # 公開部と非公開部
//!
//! - `payload`（汎用の consumer も読む）: `event_type`・`table_id`・`record_id`・`rev`・`change`。
//!   値は載せない（workflow / miniapp のトリガが id で引き直す）。
//! - `private_payload`（ストリームだけが読む・外へ出ない）: `table_int`・`record_int` と、
//!   **before の可視性判定に要る旧値**（旧 owner・行述語が参照する列の旧値・保存形 `"f{id}"`）。
//!   更新後の値だけで判定すると可視 → 不可視の購読者に何も届かず、ブラウザが機密値を表示し続ける
//!   （PIT-73）。

use authz::AuthContext;
use serde_json::{json, Map, Value};
use sqlx::PgConnection;
use storage::event::{emit_many_on, WriteEvent, WriteOp};
use uuid::Uuid;

use crate::field_key::json_key;
use crate::model::{DataTable, TableSchema};
use crate::policy::ast::PolicyExpr;
use crate::DataError;

/// outbox payload の `event_type`。
pub const RECORD_CHANGED_EVENT: &str = "data.record.changed";

/// 変更の種別（payload の `change`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChangeKind {
    Create,
    Update,
    Delete,
    /// 派生値（ルックアップ・ロールアップ・数式・材料化）だけが変わった（`rev` は進まない）。
    Derived,
    /// スキーマの改訂（`record_id` はテーブル id・購読者は `schema.changed` を受けて取り直す）。
    Schema,
    /// テーブル全体の取り直し（派生値の影響が大きすぎて行ごとに配れない・`record_id` はテーブル id）。
    Refresh,
}

impl ChangeKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ChangeKind::Create => "create",
            ChangeKind::Update => "update",
            ChangeKind::Delete => "delete",
            ChangeKind::Derived => "derived",
            ChangeKind::Schema => "schema",
            ChangeKind::Refresh => "refresh",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<ChangeKind> {
        match s {
            "create" => Some(ChangeKind::Create),
            "update" => Some(ChangeKind::Update),
            "delete" => Some(ChangeKind::Delete),
            "derived" => Some(ChangeKind::Derived),
            "schema" => Some(ChangeKind::Schema),
            "refresh" => Some(ChangeKind::Refresh),
            _ => None,
        }
    }

    fn write_op(self) -> WriteOp {
        match self {
            ChangeKind::Create => WriteOp::Create,
            ChangeKind::Delete => WriteOp::Delete,
            ChangeKind::Update | ChangeKind::Derived | ChangeKind::Schema | ChangeKind::Refresh => {
                WriteOp::Update
            }
        }
    }
}

/// 変更前の可視性判定の材料（保存形の値）。
pub(crate) struct Before<'a> {
    pub owner: &'a str,
    /// 変更前の本体（保存形 `"f{id}"` キー）。ここから行述語が参照する列だけを抜く。
    pub stored: &'a Value,
}

/// 1 件の変更。
pub(crate) struct RecordChange<'a> {
    pub record_id: Uuid,
    pub record_int: i64,
    pub rev: i64,
    pub kind: ChangeKind,
    pub before: Option<Before<'a>>,
}

/// 行述語（read）が参照する列の保存形キー（`"f{id}"`）。before 判定にはこれと owner だけが要る。
pub(crate) fn policy_keys(schema: &TableSchema) -> Vec<String> {
    fn walk(e: &PolicyExpr, schema: &TableSchema, out: &mut Vec<String>) {
        match e {
            PolicyExpr::Any(v) | PolicyExpr::All(v) => v.iter().for_each(|c| walk(c, schema, out)),
            PolicyExpr::FieldCmp { field, .. } => {
                if let Some(f) = schema.field(field) {
                    let k = json_key(f.id);
                    if !out.contains(&k) {
                        out.push(k);
                    }
                }
            }
            PolicyExpr::HasRole { .. } | PolicyExpr::IsOwner | PolicyExpr::Public => {}
        }
    }
    let mut out = Vec::new();
    if let Some(p) = &schema.row_policy {
        walk(&p.read, schema, &mut out);
    }
    out
}

/// before に載せる列: 行述語の参照列と、**対称列を持つリンク列**（付け替えで外れた参照先の対称列を
/// 配信し直すため・旧値が要る）。
pub(crate) fn before_keys(schema: &TableSchema) -> Vec<String> {
    let mut out = policy_keys(schema);
    for f in &schema.fields {
        if f.link
            .as_ref()
            .is_some_and(|l| !l.reverse && l.symmetric_field.is_some())
        {
            let k = json_key(f.id);
            if !out.contains(&k) {
                out.push(k);
            }
        }
    }
    out
}

/// before の最小材料（旧 owner と行述語の参照列だけ）。
fn before_material(keys: &[String], b: &Before<'_>) -> Value {
    let mut data = Map::new();
    if let Some(obj) = b.stored.as_object() {
        for k in keys {
            if let Some(v) = obj.get(k) {
                data.insert(k.clone(), v.clone());
            }
        }
    }
    json!({ "owner": b.owner, "data": data })
}

/// レコード変更を書込 Tx 上で outbox へ発行する（1 文・一括取込でも 1 文）。
pub(crate) async fn emit_record_changes(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    table: &DataTable,
    changes: &[RecordChange<'_>],
    trace_id: Option<&str>,
) -> Result<(), DataError> {
    if changes.is_empty() {
        return Ok(());
    }
    let keys = before_keys(&table.schema);
    let events: Vec<(WriteEvent, Option<Value>)> = changes
        .iter()
        .map(|c| {
            let public = json!({
                "event_type": RECORD_CHANGED_EVENT,
                "table_id": table.id,
                "record_id": c.record_id,
                "rev": c.rev,
                "change": c.kind.as_str(),
            });
            let private = json!({
                "table_int": table.keys.table_int,
                "record_int": c.record_int,
                "before": c.before.as_ref().map(|b| before_material(&keys, b)),
            });
            (
                WriteEvent {
                    node_id: c.record_id,
                    version: c.rev,
                    op: c.kind.write_op(),
                    payload: public,
                },
                Some(private),
            )
        })
        .collect();
    emit_many_on(conn, ctx, &events, trace_id)
        .await
        .map_err(|e| DataError::Internal(format!("outbox: {e}")))
}

/// スキーマの改訂を発行する（購読者は `schema.changed` を受けて取り直す・ストリームを閉じる）。
pub(crate) async fn emit_schema_changed(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    table: &DataTable,
    trace_id: Option<&str>,
) -> Result<(), DataError> {
    emit_table_event(conn, ctx, table, ChangeKind::Schema, trace_id).await
}

/// 保守経路（材料化の降格など・主体を持たない Tx）からのスキーマ改訂の発行。
pub(crate) async fn emit_schema_changed_by_system(
    conn: &mut PgConnection,
    tenant_id: &str,
    table_id: Uuid,
) -> Result<(), DataError> {
    if let Some((t, org)) = super::relay::table_row_with_org(conn, tenant_id, table_id).await? {
        let ctx = super::relay::system_ctx(tenant_id, &org);
        emit_schema_changed(conn, &ctx, &t, None).await?;
    }
    Ok(())
}

/// テーブル全体の取り直しを発行する（購読者は `stream.reset` を受けて取り直す）。
pub(crate) async fn emit_table_refresh(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    table: &DataTable,
) -> Result<(), DataError> {
    emit_table_event(conn, ctx, table, ChangeKind::Refresh, None).await
}

async fn emit_table_event(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    table: &DataTable,
    kind: ChangeKind,
    trace_id: Option<&str>,
) -> Result<(), DataError> {
    let public = json!({
        "event_type": RECORD_CHANGED_EVENT,
        "table_id": table.id,
        "record_id": table.id,
        "rev": table.schema_version,
        "change": kind.as_str(),
    });
    let private = json!({ "table_int": table.keys.table_int });
    emit_many_on(
        conn,
        ctx,
        &[(
            WriteEvent {
                node_id: table.id,
                version: table.schema_version,
                op: WriteOp::Update,
                payload: public,
            },
            Some(private),
        )],
        trace_id,
    )
    .await
    .map_err(|e| DataError::Internal(format!("outbox: {e}")))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::model::FieldDef;
    use crate::model::FieldType;
    use crate::policy::ast::{CmpOp, PolicyOperand, RowPolicy};

    #[test]
    fn before_material_keeps_only_policy_columns_and_owner() {
        let mut s = TableSchema {
            fields: vec![
                FieldDef::new("dept", FieldType::Text),
                FieldDef::new("salary", FieldType::Number),
            ],
            status_field: None,
            row_policy: Some(RowPolicy {
                read: PolicyExpr::Any(vec![
                    PolicyExpr::IsOwner,
                    PolicyExpr::FieldCmp {
                        field: "dept".into(),
                        op: CmpOp::In,
                        value: PolicyOperand::UserRoles { subtree: false },
                    },
                ]),
                write: None,
            }),
            field_policy: vec![],
            aggregate_min_rows: None,
            fsm_ref: None,
        };
        crate::field_key::assign_field_ids(None, &mut s).unwrap();
        let keys = policy_keys(&s);
        assert_eq!(keys, vec!["f16".to_string()]);
        let stored = json!({ "f16": "sales", "f17": 900 });
        let m = before_material(
            &keys,
            &Before {
                owner: "alice",
                stored: &stored,
            },
        );
        // 給与（述語が参照しない列）は outbox に載せない。
        assert_eq!(m, json!({ "owner": "alice", "data": { "f16": "sales" } }));
        for k in [
            ChangeKind::Create,
            ChangeKind::Update,
            ChangeKind::Delete,
            ChangeKind::Derived,
            ChangeKind::Schema,
            ChangeKind::Refresh,
        ] {
            assert_eq!(ChangeKind::parse(k.as_str()), Some(k));
        }
    }
}

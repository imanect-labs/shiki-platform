//! 構造化データ（data）の能力呼び出し（Task 15.7・data-platform §11）。node 経路と script の
//! `Shiki.data.*` 経路で共用する。
//!
//! - クエリは data の [`DataQuery`] をそのまま渡す（変換層なし）。ノードの比較値に書いた `$from` /
//!   `$template` は [`bind_values`] が実行直前に値へ置き換えるだけで、条件の表現は変えない。
//! - 書込は effect_journal（EngineDedup・cross-TX）で高々 1 回（csv.patch と同じ流儀・PIT-31）。
//!   一括 upsert はさらにポート側で取込 ID を冪等キーから決め、回収窓の再実行でも二重取込しない。
//! - 認可はチョークポイント（DataStore）側。scope 天井は [`exec`](super::exec) / script の HostBridge が
//!   事前に担保する（二重ゲート・個別ノードに認可検査を書かない）。

use data::{BulkUpsertRow, DataQuery};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::capability::{op_digest, CapabilityAudit, EffectJournal, JournalDecision};
use crate::control::eval::{resolve_value, ValueResolver};
use crate::ir::expr::ValueExpr;
use crate::ir::params::{
    DataBulkUpsertParams, DataQueryParams, DataRecordCreateParams, DataRecordDeleteParams,
    DataRecordUpdateParams, DataTransitionParams, MAX_WORKFLOW_BULK_ROWS,
};
use crate::run::NodeContext;

use super::capability::parse_params;
use super::exec::CapabilityNodeExecutor;
use super::ports::{DataWriteOp, DataWriteReq, ExecCtx, NodePorts, PortError};
use super::resolver::{as_string, as_uuid, ParamResolver};

/// JSON の中の `$from` / `$template` オブジェクトを値へ置き換える（クエリの比較値・書込の値の差し込み）。
///
/// 解決できない参照（default も無い）は `Err`（null として黙って比較しない・fail-closed）。
pub(crate) fn bind_values(v: &Value, r: &dyn ValueResolver) -> Result<Value, PortError> {
    match v {
        Value::Object(map) if map.contains_key("$from") || map.contains_key("$template") => {
            let expr: ValueExpr = serde_json::from_value(v.clone())
                .map_err(|e| PortError::invalid(format!("不正な値式: {e}")))?;
            resolve_value(&expr, r).ok_or_else(|| {
                PortError::new("expr_resolve_error", "値式（$from）が解決できません", false)
            })
        }
        Value::Object(map) => {
            let mut out = Map::with_capacity(map.len());
            for (k, x) in map {
                out.insert(k.clone(), bind_values(x, r)?);
            }
            Ok(Value::Object(out))
        }
        Value::Array(items) => items
            .iter()
            .map(|x| bind_values(x, r))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        other => Ok(other.clone()),
    }
}

/// 値式を UUID へ（テーブル・レコードの指定）。
fn resolve_uuid(e: &ValueExpr, r: &dyn ValueResolver, what: &str) -> Result<Uuid, PortError> {
    resolve_value(e, r)
        .and_then(|v| as_uuid(&v))
        .ok_or_else(|| PortError::invalid(format!("{what} が UUID に解決できません")))
}

/// 省略可能な rev（整数）。
fn resolve_rev(e: Option<&ValueExpr>, r: &dyn ValueResolver) -> Result<Option<i64>, PortError> {
    match e {
        None => Ok(None),
        Some(e) => resolve_value(e, r)
            .and_then(|v| v.as_i64())
            .map(Some)
            .ok_or_else(|| PortError::invalid("expected_rev が整数に解決できません")),
    }
}

/// 一括 upsert の行（`{ id?, rev?, key?, data }`）を data の型へ。形の誤りは全体を拒否する。
pub(crate) fn parse_bulk_rows(v: &Value) -> Result<Vec<BulkUpsertRow>, PortError> {
    let items = v
        .as_array()
        .ok_or_else(|| PortError::invalid("data.bulk_upsert: rows は配列である必要があります"))?;
    if items.len() > MAX_WORKFLOW_BULK_ROWS {
        return Err(PortError::invalid(format!(
            "data.bulk_upsert: 1 回の行数は最大 {MAX_WORKFLOW_BULK_ROWS} 行です（{} 行）",
            items.len()
        )));
    }
    items
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let n = i + 1;
            let obj = row.as_object().ok_or_else(|| {
                PortError::invalid(format!("data.bulk_upsert: {n} 行目がオブジェクトではありません"))
            })?;
            if let Some(k) = obj
                .keys()
                .find(|k| !matches!(k.as_str(), "id" | "rev" | "key" | "data"))
            {
                return Err(PortError::invalid(format!(
                    "data.bulk_upsert: {n} 行目に未知のキー '{k}' があります（id / rev / key / data のみ）"
                )));
            }
            let record_id = match obj.get("id") {
                None | Some(Value::Null) => None,
                Some(v) => Some(as_uuid(v).ok_or_else(|| {
                    PortError::invalid(format!("data.bulk_upsert: {n} 行目の id が UUID ではありません"))
                })?),
            };
            let expected_rev = match obj.get("rev") {
                None | Some(Value::Null) => None,
                Some(v) => Some(v.as_i64().ok_or_else(|| {
                    PortError::invalid(format!("data.bulk_upsert: {n} 行目の rev が整数ではありません"))
                })?),
            };
            let key = obj.get("key").and_then(Value::as_str).map(str::to_string);
            let data = obj.get("data").cloned().ok_or_else(|| {
                PortError::invalid(format!("data.bulk_upsert: {n} 行目に data がありません"))
            })?;
            Ok(BulkUpsertRow {
                key,
                record_id,
                expected_rev,
                data,
            })
        })
        .collect()
}

/// data 書込を effect_journal で高々 1 回にする（node と script の共用）。
///
/// `digest_body` は書込の同一性（テーブル・対象・値）。同じ冪等キーで別の書込が来たら permanent。
pub(crate) async fn journaled_data_write(
    ports: &dyn NodePorts,
    journal: &EffectJournal,
    audit: &dyn CapabilityAudit,
    ec: &ExecCtx,
    req: DataWriteReq,
    digest_body: &Value,
    via: &str,
) -> Result<Value, PortError> {
    let api = req.op.api();
    let table_id = req.table_id;
    let key = req.idempotency_key.clone();
    let digest = op_digest(api, digest_body);
    match journal
        .check(&ec.tenant_id, &key, &digest)
        .await
        .map_err(|e| PortError::unavailable(format!("journal: {e}")))?
    {
        JournalDecision::Proceed => {
            let out = match ports.data_write(ec, req).await {
                Ok(v) => v,
                Err(e) => {
                    audit.record(
                        &ec.tenant_id,
                        api,
                        false,
                        &json!({ "table_id": table_id, "code": e.code, "via": via }),
                    );
                    // data の書込は 1 Tx（拒否・検証エラーなら何も書かれていない）。予約を外して同じ
                    // ステップの再実行を待たせない。副作用の有無が不明な一時障害（unavailable）は外さない
                    // （一括 upsert は取込 ID が冪等なので外しても二重にならない）。
                    if e.code != "unavailable" {
                        journal
                            .release(&ec.tenant_id, &key, &digest)
                            .await
                            .map_err(|je| {
                                PortError::unavailable(format!("journal release: {je}"))
                            })?;
                    }
                    return Err(e);
                }
            };
            journal
                .record(&ec.tenant_id, &key, &digest, &out)
                .await
                .map_err(|e| PortError::unavailable(format!("journal record: {e}")))?;
            // 値・行の中身は監査に載せない（PII）。テーブルと対象の外部 ID だけ。
            audit.record(
                &ec.tenant_id,
                api,
                true,
                &json!({ "table_id": table_id, "record_id": out.get("id"), "via": via }),
            );
            Ok(out)
        }
        JournalDecision::AlreadyDone(v) => Ok(v),
        JournalDecision::InProgress => Err(PortError::new(
            "effect_in_progress",
            "別ワーカーがデータを書込中",
            true,
        )),
        JournalDecision::DigestMismatch => Err(PortError::new(
            "effect_conflict",
            "同一冪等キーで別のデータ書込要求",
            false,
        )),
    }
}

/// 件数の要約（監査用・値は載せない）。
fn item_count(out: &Value) -> usize {
    out.get("items")
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

impl CapabilityNodeExecutor {
    /// data.query（viewer・pure）。比較値の `$from` を解決してから data の `DataQuery` として渡す。
    pub(super) async fn node_data_query(
        &self,
        params: &Value,
        ec: &ExecCtx,
        r: &ParamResolver<'_>,
    ) -> Result<Value, PortError> {
        let p: DataQueryParams = parse_params(params)?;
        let table_id = resolve_uuid(&p.table, r, "data.query: table")?;
        let raw = params.get("query").cloned().unwrap_or_else(|| json!({}));
        let query: DataQuery = serde_json::from_value(bind_values(&raw, r)?)
            .map_err(|e| PortError::invalid(format!("data.query: query が不正: {e}")))?;
        let out = self.ports.data_query(ec, table_id, &query).await?;
        self.audit(
            &ec.tenant_id,
            "data.query",
            true,
            &json!({ "table_id": table_id, "items": item_count(&out) }),
        );
        Ok(out)
    }

    /// data の書込ノード（create / update / delete / transition / bulk_upsert）。
    pub(super) async fn node_data_write(
        &self,
        api: &str,
        params: &Value,
        ctx: &NodeContext,
        ec: &ExecCtx,
        r: &ParamResolver<'_>,
    ) -> Result<Value, PortError> {
        let (table_id, op, digest) = match api {
            "data.record.create" => {
                let p: DataRecordCreateParams = parse_params(params)?;
                let table = resolve_uuid(&p.table, r, "data.record.create: table")?;
                let data = bind_values(params.get("data").unwrap_or(&Value::Null), r)?;
                let digest = json!({ "table": table, "data": data });
                (table, DataWriteOp::Create { data }, digest)
            }
            "data.record.update" => {
                let p: DataRecordUpdateParams = parse_params(params)?;
                let table = resolve_uuid(&p.table, r, "data.record.update: table")?;
                let record_id = resolve_uuid(&p.record, r, "data.record.update: record")?;
                let patch = bind_values(params.get("patch").unwrap_or(&Value::Null), r)?;
                let expected_rev = resolve_rev(p.expected_rev.as_ref(), r)?;
                let digest = json!({ "table": table, "record": record_id, "patch": patch,
                                     "rev": expected_rev });
                let op = DataWriteOp::Update {
                    record_id,
                    patch,
                    expected_rev,
                };
                (table, op, digest)
            }
            "data.record.delete" => {
                let p: DataRecordDeleteParams = parse_params(params)?;
                let table = resolve_uuid(&p.table, r, "data.record.delete: table")?;
                let record_id = resolve_uuid(&p.record, r, "data.record.delete: record")?;
                let expected_rev = resolve_rev(p.expected_rev.as_ref(), r)?;
                let digest = json!({ "table": table, "record": record_id, "rev": expected_rev });
                let op = DataWriteOp::Delete {
                    record_id,
                    expected_rev,
                };
                (table, op, digest)
            }
            "data.transition" => {
                let p: DataTransitionParams = parse_params(params)?;
                let table = resolve_uuid(&p.table, r, "data.transition: table")?;
                let record_id = resolve_uuid(&p.record, r, "data.transition: record")?;
                let to = resolve_value(&p.to, r)
                    .and_then(|v| as_string(&v))
                    .ok_or_else(|| {
                        PortError::invalid("data.transition: to が文字列に解決できません")
                    })?;
                let expected_rev = resolve_rev(p.expected_rev.as_ref(), r)?;
                let digest = json!({ "table": table, "record": record_id, "to": to,
                                     "rev": expected_rev });
                let op = DataWriteOp::Transition {
                    record_id,
                    to,
                    expected_rev,
                };
                (table, op, digest)
            }
            "data.bulk_upsert" => {
                let p: DataBulkUpsertParams = parse_params(params)?;
                let table = resolve_uuid(&p.table, r, "data.bulk_upsert: table")?;
                let rows_raw = resolve_value(&p.rows, r)
                    .ok_or_else(|| PortError::invalid("data.bulk_upsert: rows が解決できません"))?;
                let rows = parse_bulk_rows(&rows_raw)?;
                let abort_on_invalid = p.abort_on_invalid.unwrap_or(false);
                let digest = json!({ "table": table, "rows": rows_raw,
                                     "abort_on_invalid": abort_on_invalid });
                let op = DataWriteOp::BulkUpsert {
                    rows,
                    abort_on_invalid,
                };
                (table, op, digest)
            }
            other => {
                return Err(PortError::invalid(format!("未知の data 書込: {other}")));
            }
        };
        journaled_data_write(
            self.ports.as_ref(),
            &self.journal,
            self.audit.as_ref(),
            ec,
            DataWriteReq {
                table_id,
                op,
                idempotency_key: ctx.idempotency_key.clone(),
            },
            &digest,
            "node",
        )
        .await
    }
}

/// script の `Shiki.data.*`（HostCall）を能力ポートへ（scope 天井は呼び出し側の HostBridge が担保済み）。
///
/// 引数の形はミニアプリの前置き（`script_runtime::prelude`）と同じ（`table_id` / `record_id` / `body`）。
/// 書込の冪等キーはホスト呼び出しごと（`<step キー>#c<seq>`・engine.md §7.3）。
pub(crate) async fn script_data_call(
    ports: &dyn NodePorts,
    journal: &EffectJournal,
    audit: &dyn CapabilityAudit,
    ec: &ExecCtx,
    api: &str,
    a: &Value,
    call_key: &str,
) -> Result<Value, PortError> {
    let uuid_arg = |name: &str| {
        a.get(name)
            .and_then(as_uuid)
            .ok_or_else(|| PortError::invalid(format!("{api}: {name} が UUID ではありません")))
    };
    let body = a.get("body").cloned().unwrap_or_else(|| json!({}));
    let rev = body.get("expected_rev").and_then(Value::as_i64);
    let write = |table_id: Uuid, op: DataWriteOp, digest: Value| async move {
        journaled_data_write(
            ports,
            journal,
            audit,
            ec,
            DataWriteReq {
                table_id,
                op,
                idempotency_key: call_key.to_string(),
            },
            &digest,
            "script",
        )
        .await
    };
    match api {
        "data.list_tables" => {
            let out = ports.data_list_tables(ec).await?;
            audit.record(&ec.tenant_id, api, true, &json!({ "via": "script" }));
            Ok(out)
        }
        "data.query" => {
            let table_id = uuid_arg("table_id")?;
            let query: DataQuery = serde_json::from_value(body)
                .map_err(|e| PortError::invalid(format!("data.query: query が不正: {e}")))?;
            let out = ports.data_query(ec, table_id, &query).await?;
            audit.record(
                &ec.tenant_id,
                api,
                true,
                &json!({ "table_id": table_id, "items": item_count(&out), "via": "script" }),
            );
            Ok(out)
        }
        "data.get" => {
            let table_id = uuid_arg("table_id")?;
            let record_id = uuid_arg("record_id")?;
            let out = ports.data_get(ec, table_id, record_id).await?;
            audit.record(
                &ec.tenant_id,
                api,
                true,
                &json!({ "table_id": table_id, "via": "script" }),
            );
            Ok(out)
        }
        "data.create" => {
            let table_id = uuid_arg("table_id")?;
            let data = body.get("data").cloned().unwrap_or(Value::Null);
            let digest = json!({ "table": table_id, "data": data });
            write(table_id, DataWriteOp::Create { data }, digest).await
        }
        "data.update" => {
            let table_id = uuid_arg("table_id")?;
            let record_id = uuid_arg("record_id")?;
            let patch = body.get("patch").cloned().unwrap_or(Value::Null);
            let digest =
                json!({ "table": table_id, "record": record_id, "patch": patch, "rev": rev });
            let op = DataWriteOp::Update {
                record_id,
                patch,
                expected_rev: rev,
            };
            write(table_id, op, digest).await
        }
        "data.transition" => {
            let table_id = uuid_arg("table_id")?;
            let record_id = uuid_arg("record_id")?;
            let to = body
                .get("to")
                .and_then(Value::as_str)
                .ok_or_else(|| PortError::invalid("data.transition: to がありません"))?
                .to_string();
            let digest = json!({ "table": table_id, "record": record_id, "to": to, "rev": rev });
            let op = DataWriteOp::Transition {
                record_id,
                to,
                expected_rev: rev,
            };
            write(table_id, op, digest).await
        }
        other => Err(PortError::invalid(format!("未知の Shiki API: {other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::expr::FromRef;

    struct R(Value);
    impl ValueResolver for R {
        fn resolve_from(&self, from: &FromRef) -> Option<Value> {
            (from.from == "input").then(|| match &from.path {
                Some(p) => self.0.pointer(p).cloned(),
                None => Some(self.0.clone()),
            })?
        }
    }

    #[test]
    fn bind_values_fills_operands_without_changing_the_shape() {
        let r = R(json!({ "min": 10, "who": "alice" }));
        let q = json!({ "filter": { "all": [
            { "cmp": { "field": "amount", "op": "gte", "value": { "$from": "input", "path": "/min" } } },
            { "cmp": { "field": "owner", "op": "in", "value": [ { "$from": "input", "path": "/who" }, "bob" ] } }
        ] }, "limit": 5 });
        let bound = bind_values(&q, &r).unwrap();
        assert_eq!(
            bound,
            json!({ "filter": { "all": [
                { "cmp": { "field": "amount", "op": "gte", "value": 10 } },
                { "cmp": { "field": "owner", "op": "in", "value": ["alice", "bob"] } }
            ] }, "limit": 5 })
        );
        // 解決できない参照は null で比較せず失敗させる（fail-closed）。
        let bad = json!({ "cmp": { "field": "a", "op": "eq", "value": { "$from": "input", "path": "/nope" } } });
        assert_eq!(
            bind_values(&bad, &r).unwrap_err().code,
            "expr_resolve_error"
        );
        // バインド後は data の DataQuery としてそのまま読める（変換層なし）。
        let _: DataQuery = serde_json::from_value(bound).unwrap();
    }

    #[test]
    fn bulk_rows_are_strict() {
        let id = Uuid::new_v4();
        let rows = parse_bulk_rows(&json!([
            { "id": id.to_string(), "rev": 3, "data": { "a": 1 } },
            { "key": "k2", "data": { "a": 2 } }
        ]))
        .unwrap();
        assert_eq!(rows[0].record_id, Some(id));
        assert_eq!(rows[0].expected_rev, Some(3));
        assert_eq!(rows[1].record_id, None);
        assert_eq!(rows[1].key.as_deref(), Some("k2"));
        assert!(parse_bulk_rows(&json!([{ "data": {}, "status": "x" }])).is_err());
        assert!(parse_bulk_rows(&json!([{ "id": "nope", "data": {} }])).is_err());
        assert!(parse_bulk_rows(&json!({ "data": {} })).is_err());
        let too_many: Vec<Value> = (0..=MAX_WORKFLOW_BULK_ROWS)
            .map(|_| json!({ "data": {} }))
            .collect();
        assert!(parse_bulk_rows(&Value::Array(too_many)).is_err());
    }
}

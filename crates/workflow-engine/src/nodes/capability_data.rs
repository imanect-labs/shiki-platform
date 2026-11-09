//! 構造化データ（data）の能力呼び出し（Task 15.7・data-platform §11）。node 経路と script の
//! `Shiki.data.*` 経路で共用する。
//!
//! - クエリは data の [`DataQuery`] をそのまま渡す（変換層なし）。ノードの比較値に書いた `$from` /
//!   `$template` は [`resolve_deep_json`] が実行直前に値へ置き換えるだけで、条件の表現は変えない
//!   （書込の値・一括の行も同じ関数で展開する。値の解決は control::eval の 1 か所）。
//! - 書込は effect_journal（EngineDedup・cross-TX）で高々 1 回（csv.patch と同じ流儀・PIT-31）。
//!   一括 upsert はさらにポート側で取込 ID を冪等キーから決め、回収窓の再実行でも二重取込しない。
//! - 認可はチョークポイント（DataStore）側。scope 天井は [`exec`](super::exec) / script の HostBridge が
//!   事前に担保する（二重ゲート・個別ノードに認可検査を書かない）。

use data::{BulkUpsertRow, DataQuery};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::capability::{op_digest, CapabilityAudit, EffectJournal};
use crate::control::eval::{resolve_deep_json, ValueResolver};
use crate::ir::expr::ValueExpr;
use crate::ir::params::{
    DataBulkRow, DataBulkUpsertParams, DataQueryParams, DataRecordCreateParams,
    DataRecordDeleteParams, DataRecordUpdateParams, DataTransitionParams, MAX_WORKFLOW_BULK_ROWS,
};
use crate::run::NodeContext;

use super::capability::{journaled, parse_params};
use super::capability_data_keyed::{AddingUpdate, MatchedBulk};
use super::data_upsert;
use super::exec::CapabilityNodeExecutor;
use super::ports::{DataWriteOp, DataWriteReq, ExecCtx, NodePorts, PortError};
use super::resolver::{as_string, as_uuid, opt_param, param, typed, ParamResolver};

/// 値式を UUID へ（テーブル・レコードの指定）。
fn resolve_uuid(e: &ValueExpr, r: &dyn ValueResolver, what: &str) -> Result<Uuid, PortError> {
    typed(&param(e, r, what)?, what, "UUID", as_uuid)
}

/// 省略可能な rev（整数）。書いてあれば整数に解決できること（省いたときだけ実行時点の rev）。
fn resolve_rev(
    e: Option<&ValueExpr>,
    r: &dyn ValueResolver,
    what: &str,
) -> Result<Option<i64>, PortError> {
    opt_param(e, r, what)?
        .map(|v| typed(&v, what, "整数", Value::as_i64))
        .transpose()
}

/// 一括 upsert の行（[`DataBulkRow`]・`{ id?, rev?, key?, data }`）を data の型へ。形の誤りは全体を拒否する。
///
/// 行の形は params の [`DataBulkRow`] の 1 か所で決める（AI が読む params の形も同じ型から作る・GAP-131）。
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
            let not_object =
                || PortError::invalid(format!("data.bulk_upsert: {n} 行目がオブジェクトではありません"));
            if !row.is_object() {
                return Err(not_object());
            }
            let row: DataBulkRow = serde_json::from_value(row.clone()).map_err(|_| not_object())?;
            if let Some(k) = row.unknown.keys().next() {
                // data の無い平らな行（csv.query の items・{列: 値} の行をそのまま渡した）は、包み方まで言う（BUG-255）。
                if row.data.is_none() {
                    return Err(PortError::invalid(format!(
                        "data.bulk_upsert: {n} 行目に未知のキー '{k}' があります。行は 1 行ずつ {{\"data\": {{列の参照名: 値}}}} の形に包んで渡してください（行に書けるのは id / rev / key / data だけ）。"
                    )));
                }
                return Err(PortError::invalid(format!(
                    "data.bulk_upsert: {n} 行目に未知のキー '{k}' があります（id / rev / key / data のみ）"
                )));
            }
            let record_id = match row.id {
                None => None,
                Some(v) => Some(as_uuid(&v).ok_or_else(|| {
                    PortError::invalid(format!("data.bulk_upsert: {n} 行目の id が UUID ではありません"))
                })?),
            };
            let expected_rev = match row.rev {
                None => None,
                Some(v) => Some(v.as_i64().ok_or_else(|| {
                    PortError::invalid(format!("data.bulk_upsert: {n} 行目の rev が整数ではありません"))
                })?),
            };
            let key = row.key.as_ref().and_then(Value::as_str).map(str::to_string);
            let data = row.data.ok_or_else(|| {
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
    let effect = DataEffect {
        api: req.op.api(),
        table_id: req.table_id,
        key: req.idempotency_key.clone(),
        via,
    };
    journaled_data_effect(
        journal,
        audit,
        ec,
        &effect,
        digest_body,
        ports.data_write(ec, req),
    )
    .await
}

/// 1 つの step の data の書込（監査の操作名・テーブル・冪等キー・経路）。
pub(crate) struct DataEffect<'a> {
    pub api: &'a str,
    pub table_id: Uuid,
    pub key: String,
    pub via: &'a str,
}

/// data の書込（`apply`）を effect_journal で高々 1 回にし、監査を残す。
///
/// `apply` は 1 回の書込か、探してから書く組み立て（キーで追加か更新・足す数・同じレコードを探す列・GAP-16）。
/// 組み立ても、成功する書込は高々 1 回で、ほかの誤りは何も書かずに返る（[`super::data_upsert`]）ので、予約を外す
/// 決まりは 1 回の書込と同じ。
pub(crate) async fn journaled_data_effect<F>(
    journal: &EffectJournal,
    audit: &dyn CapabilityAudit,
    ec: &ExecCtx,
    effect: &DataEffect<'_>,
    digest_body: &Value,
    apply: F,
) -> Result<Value, PortError>
where
    F: std::future::Future<Output = Result<Value, PortError>>,
{
    let DataEffect {
        api,
        table_id,
        ref key,
        via,
    } = *effect;
    let digest = op_digest(api, digest_body);
    // data の書込は 1 Tx（拒否・検証エラーなら何も書かれていない）。予約を外す決まりは csv・workflow.start と同じ
    // （[`journaled`]・副作用の有無が不明な一時障害は外さない。一括 upsert は取込 ID が冪等なので外しても二重にならない）。
    let apply = async {
        apply.await.inspect_err(|e| {
            audit.record(
                &ec.tenant_id,
                api,
                false,
                &json!({ "table_id": table_id, "code": e.code, "via": via }),
            );
        })
    };
    let (out, wrote) = journaled(
        journal,
        &ec.tenant_id,
        key,
        &digest,
        "テーブルへの書き込み",
        apply,
    )
    .await?;
    if wrote {
        // 値・行の中身は監査に載せない（PII）。テーブルと対象の外部 ID だけ。
        audit.record(
            &ec.tenant_id,
            api,
            true,
            &json!({ "table_id": table_id, "record_id": out.get("id"), "via": via }),
        );
    }
    Ok(out)
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
        // 最上位の未知のキー（綴り誤り）は黙って無視せず不正として返す（data の単一の定義で判定）。
        let bound = resolve_deep_json(&raw, r).map_err(|e| {
            PortError::new(e.code, format!("data.query: query: {}", e.message), false)
        })?;
        let query = DataQuery::from_request(bound).map_err(|e| {
            PortError::invalid(format!(
                "data.query: query が不正: {}",
                e.invalid_message().unwrap_or_else(|| e.to_string())
            ))
        })?;
        let out = self.ports.data_query(ec, table_id, &query).await?;
        self.audit(
            &ec.tenant_id,
            "data.query",
            true,
            &json!({ "table_id": table_id, "items": item_count(&out) }),
        );
        Ok(out)
    }

    /// data の書込ノード（create / update / upsert / delete / transition / bulk_upsert）。
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
                let data = param(&p.data, r, "data.record.create: data")?;
                let digest = json!({ "table": table, "data": data });
                (table, DataWriteOp::Create { data }, digest)
            }
            "data.record.update" => {
                let p: DataRecordUpdateParams = parse_params(params)?;
                let table = resolve_uuid(&p.table, r, "data.record.update: table")?;
                let record_id = resolve_uuid(&p.record, r, "data.record.update: record")?;
                let expected_rev = resolve_rev(
                    p.expected_rev.as_ref(),
                    r,
                    "data.record.update: expected_rev",
                )?;
                let add = data_upsert::parse_add(opt_param(
                    p.add.as_ref(),
                    r,
                    "data.record.update: add",
                )?)?;
                if !add.is_empty() {
                    // 足す数（GAP-16）: 読んだ rev を前提に書き、重なったら読み直して足し直す。
                    let patch = data_upsert::parse_values(
                        param(&p.patch, r, "data.record.update: patch")?,
                        "変更する値",
                    )?;
                    let target = AddingUpdate {
                        table,
                        record_id,
                        patch,
                        add,
                        expected_rev,
                    };
                    return self.node_data_update_adding(api, target, ctx, ec).await;
                }
                let patch = param(&p.patch, r, "data.record.update: patch")?;
                let digest = json!({ "table": table, "record": record_id, "patch": patch,
                                     "rev": expected_rev });
                let op = DataWriteOp::Update {
                    record_id,
                    patch,
                    expected_rev,
                };
                (table, op, digest)
            }
            "data.record.upsert" => return self.node_data_upsert(api, params, ctx, ec, r).await,
            "data.record.delete" => {
                let p: DataRecordDeleteParams = parse_params(params)?;
                let table = resolve_uuid(&p.table, r, "data.record.delete: table")?;
                let record_id = resolve_uuid(&p.record, r, "data.record.delete: record")?;
                let expected_rev = resolve_rev(
                    p.expected_rev.as_ref(),
                    r,
                    "data.record.delete: expected_rev",
                )?;
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
                let what = "data.transition: to";
                let to = typed(&param(&p.to, r, what)?, what, "文字列", as_string)?;
                let expected_rev =
                    resolve_rev(p.expected_rev.as_ref(), r, "data.transition: expected_rev")?;
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
                // 行の中の入れ子の $from / $template も展開する（生の値式を data へ渡さない）。
                let rows_raw = param(&p.rows, r, "data.bulk_upsert: rows")?;
                let rows = parse_bulk_rows(&rows_raw)?;
                let abort_on_invalid = p.abort_on_invalid.unwrap_or(false);
                if let Some(match_on) = p.match_on() {
                    // 同じレコードを探す列（GAP-16）: id の無い行を、同じ値のレコードに当ててから取り込む。
                    let digest = json!({ "table": table, "rows": rows_raw,
                                         "abort_on_invalid": abort_on_invalid,
                                         "match_on": match_on });
                    let bulk = MatchedBulk {
                        table,
                        match_on,
                        rows,
                        abort_on_invalid,
                    };
                    return self
                        .node_data_bulk_matched(api, bulk, &digest, ctx, ec)
                        .await;
                }
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
    fn query_operands_are_filled_without_changing_the_shape() {
        let r = R(json!({ "min": 10, "who": "alice" }));
        let q = json!({ "filter": { "all": [
            { "cmp": { "field": "amount", "op": "gte", "value": { "$from": "input", "path": "/min" } } },
            { "cmp": { "field": "owner", "op": "in", "value": [ { "$from": "input", "path": "/who" }, "bob" ] } }
        ] }, "limit": 5 });
        let bound = resolve_deep_json(&q, &r).unwrap();
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
            resolve_deep_json(&bad, &r).unwrap_err().code,
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
        let message = |rows: Value| parse_bulk_rows(&rows).unwrap_err().message;
        // data のある行の未知のキーは、これまでどおり書けるキーを言う。
        assert_eq!(
            message(json!([{ "data": {}, "status": "x" }])),
            "data.bulk_upsert: 1 行目に未知のキー 'status' があります（id / rev / key / data のみ）"
        );
        // data の無い平らな行（csv.query の items をそのまま渡した）は、包み方まで言う（BUG-255）。
        assert_eq!(
            message(json!([{ "data": { "sku": "A" } }, { "sku": "B", "stock": 3 }])),
            "data.bulk_upsert: 2 行目に未知のキー 'sku' があります。行は 1 行ずつ {\"data\": {列の参照名: 値}} の形に包んで渡してください（行に書けるのは id / rev / key / data だけ）。"
        );
        assert_eq!(
            message(json!([{ "id": id.to_string() }])),
            "data.bulk_upsert: 1 行目に data がありません"
        );
        // null の id・rev は書いていないのと同じ。書いてある data は null も値として渡す（data が断る）。
        let rows = parse_bulk_rows(&json!([{ "id": null, "rev": null, "data": null }])).unwrap();
        assert_eq!(
            (rows[0].record_id, rows[0].expected_rev, &rows[0].data),
            (None, None, &Value::Null)
        );
        assert_eq!(
            message(json!([{ "data": {} }, "x"])),
            "data.bulk_upsert: 2 行目がオブジェクトではありません"
        );
        assert!(parse_bulk_rows(&json!([{ "id": "nope", "data": {} }])).is_err());
        assert!(parse_bulk_rows(&json!({ "data": {} })).is_err());
        let too_many: Vec<Value> = (0..=MAX_WORKFLOW_BULK_ROWS)
            .map(|_| json!({ "data": {} }))
            .collect();
        assert!(parse_bulk_rows(&Value::Array(too_many)).is_err());
    }
}

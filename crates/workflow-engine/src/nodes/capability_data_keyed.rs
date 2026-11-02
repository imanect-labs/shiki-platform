//! 探してから書く data のブロック（GAP-16）の node の経路: キーで追加か更新・足す数・まとめて登録の同じレコードを
//! 探す列。組み立ての中身は [`super::data_upsert`]、予約と監査は 1 回の書込と同じ
//! （[`super::capability_data::journaled_data_effect`]）。

use data::BulkUpsertRow;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::ir::params::DataRecordUpsertParams;
use crate::run::NodeContext;

use super::capability::parse_params;
use super::capability_data::{journaled_data_effect, DataEffect};
use super::data_upsert;
use super::exec::CapabilityNodeExecutor;
use super::ports::{DataWriteOp, DataWriteReq, ExecCtx, PortError};
use super::resolver::{as_uuid, opt_param, param, typed, ParamResolver};

/// 足す数つきのレコードの更新（解決した値）。
pub(super) struct AddingUpdate {
    pub table: Uuid,
    pub record_id: Uuid,
    pub patch: Map<String, Value>,
    pub add: Map<String, Value>,
    pub expected_rev: Option<i64>,
}

/// 同じレコードを探す列つきのまとめて登録（解決した行）。
pub(super) struct MatchedBulk<'a> {
    pub table: Uuid,
    pub match_on: &'a str,
    pub rows: Vec<BulkUpsertRow>,
    pub abort_on_invalid: bool,
}

impl CapabilityNodeExecutor {
    /// 探してから書く組み立てを、1 回の書込と同じ予約と監査で包む（node の経路）。
    async fn journaled_effect<F>(
        &self,
        api: &str,
        table_id: Uuid,
        ctx: &NodeContext,
        ec: &ExecCtx,
        digest: &Value,
        apply: F,
    ) -> Result<Value, PortError>
    where
        F: std::future::Future<Output = Result<Value, PortError>>,
    {
        let effect = DataEffect {
            api,
            table_id,
            key: ctx.idempotency_key.clone(),
            via: "node",
        };
        journaled_data_effect(
            &self.journal,
            self.audit.as_ref(),
            ec,
            &effect,
            digest,
            apply,
        )
        .await
    }

    /// `data.record.update` の足す数: 書き込む人の見え方で読んだ rev を前提に書き、重なったら読み直して足し直す。
    pub(super) async fn node_data_update_adding(
        &self,
        api: &str,
        t: AddingUpdate,
        ctx: &NodeContext,
        ec: &ExecCtx,
    ) -> Result<Value, PortError> {
        let digest = json!({ "table": t.table, "record": t.record_id, "patch": t.patch,
                             "add": t.add, "rev": t.expected_rev });
        let key = ctx.idempotency_key.clone();
        let apply = data_upsert::update_adding(
            self.ports.as_ref(),
            ec,
            t.table,
            t.record_id,
            t.patch,
            &t.add,
            t.expected_rev,
            &key,
        );
        self.journaled_effect(api, t.table, ctx, ec, &digest, apply)
            .await
    }

    /// `data.record.upsert`: 同じレコードを探す列の値が同じレコードがあれば書き換え、なければ追加する。
    pub(super) async fn node_data_upsert(
        &self,
        api: &str,
        params: &Value,
        ctx: &NodeContext,
        ec: &ExecCtx,
        r: &ParamResolver<'_>,
    ) -> Result<Value, PortError> {
        let p: DataRecordUpsertParams = parse_params(params)?;
        let what = "data.record.upsert: table";
        let table = typed(&param(&p.table, r, what)?, what, "UUID", as_uuid)?;
        let data =
            data_upsert::parse_values(param(&p.data, r, "data.record.upsert: data")?, "入れる値")?;
        let add = data_upsert::parse_add(opt_param(p.add.as_ref(), r, "data.record.upsert: add")?)?;
        let digest = json!({ "table": table, "match_on": p.match_on, "data": data, "add": add });
        let req = data_upsert::UpsertReq {
            table_id: table,
            match_on: p.match_on,
            data,
            add,
            idempotency_key: ctx.idempotency_key.clone(),
        };
        let apply = data_upsert::upsert(self.ports.as_ref(), ec, &req);
        self.journaled_effect(api, table, ctx, ec, &digest, apply)
            .await
    }

    /// `data.bulk_upsert` の同じレコードを探す列: id の無い行を同じ値のレコードに当ててから取り込む。照合は予約の
    /// 内側で行う（記録済みの step は探し直さない・取込 ID は冪等キーから決まるので、やり直しても二重に取り込まない）。
    /// 結果に当てた行の数（`rows_matched`）を足す。
    pub(super) async fn node_data_bulk_matched(
        &self,
        api: &str,
        bulk: MatchedBulk<'_>,
        digest: &Value,
        ctx: &NodeContext,
        ec: &ExecCtx,
    ) -> Result<Value, PortError> {
        let MatchedBulk {
            table,
            match_on,
            mut rows,
            abort_on_invalid,
        } = bulk;
        let ports = self.ports.as_ref();
        let key = ctx.idempotency_key.clone();
        let apply = async move {
            let matched =
                data_upsert::match_bulk_rows(ports, ec, table, match_on, &mut rows).await?;
            let op = DataWriteOp::BulkUpsert {
                rows,
                abort_on_invalid,
            };
            let req = DataWriteReq {
                table_id: table,
                op,
                idempotency_key: key,
            };
            let mut out = ports.data_write(ec, req).await?;
            if let Value::Object(m) = &mut out {
                m.insert("rows_matched".into(), json!(matched));
            }
            Ok(out)
        };
        self.journaled_effect(api, table, ctx, ec, digest, apply)
            .await
    }
}

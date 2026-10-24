//! ワークフローの data ノード・`Shiki.data.*` の本番ポート（Task 15.7・data-platform §11）。
//!
//! 実処理は data の `DataStore`（行述語・フィールドマスク・OpenFGA の認可込み）に委ね、ここは
//! **誰の権限で読み書きするか**だけを決める（認可の判断を散らさない・単一チョークポイント）。
//!
//! - interactive run（実行主体 = ユーザー）: そのユーザーの権限。
//! - schedule / event run（実行主体 = workflow プリンシパル）:
//!   ① ワークフロー主体にテーブルが委譲されていること（`data_table` のタプル）を確かめる。棚卸しで
//!   タプルを撤去すればここで受動的に落ちる（engine.md §6.4）。
//!   ② 委譲台帳の委譲元ユーザーの `AuthContext` で読み書きする。行は**委譲元の行述語**で絞られ、
//!   マスク列も委譲元のもの。委譲元が今もテーブルの権限を持つかは data の入口が委譲元で問い直す。
//!   実効権限 = scope 天井（engine 側）∩ 委譲したテーブル ∩ 委譲元の ReBAC・行述語。
//!   ③ 入口から結合・半結合・lookup / rollup・リンク値で辿る参照先も「委譲したテーブル」に限る。
//!   委譲元が自ら委譲し、ワークフロー主体のタプルも残るテーブルだけを天井（`data::TableReach`）にして
//!   data へ渡す。天井の外は委譲元が読めないテーブルと同じ応答になる（engine.md §6.8）。

use authz::{AuthContext, Principal, PrincipalKind, Relation};
use data::{BulkImportOptions, DataError, DataQuery, DataStore, FsmStore, TableReach};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;
use workflow_engine::{DataWriteOp, DataWriteReq, ExecCtx, PortError};

use super::auth::auth_ctx;

/// 1 ノードで返す一括 upsert の拒否行の上限（出力の大きさを有界にする）。
const MAX_REPORTED_ROW_ERRORS: i64 = 100;
/// `data.list_tables` の件数上限。
const MAX_LISTED_TABLES: i64 = 200;
/// 委譲の天井に載せるテーブル数の上限（超過分は辿れない＝fail-closed）。
const MAX_REACHABLE_TABLES: i64 = 500;

/// 読み書きする主体と、その主体で使うストア（ワークフロー主体なら委譲の天井つき）。
struct Actor {
    ctx: AuthContext,
    data: DataStore,
    fsms: FsmStore,
}

/// data のエラー → ポートのエラー（不可視と不在は同じ「見つからない」・再試行で直るものだけ retryable）。
pub(super) fn map_data(e: DataError) -> PortError {
    match e {
        DataError::NotFound => PortError::new("not_found", "対象が見つかりません", false),
        DataError::Forbidden => PortError::forbidden("data の権限がありません"),
        // 存在しない列と読めない列は同じ応答（列の存在オラクルを作らない・data-platform §16.1）。
        DataError::FieldUnavailable => {
            PortError::new("field_unavailable", data::FIELD_UNAVAILABLE_MESSAGE, false)
        }
        DataError::Invalid(m) => PortError::invalid(m),
        DataError::InvalidInput(e) => PortError::invalid(e.message().to_string()),
        // テーブル名の重複（再試行しても直らない）。
        e @ DataError::NameTaken => {
            PortError::new("conflict", e.conflict_detail().unwrap_or_default(), false)
        }
        // unique 違反は再試行しても直らない（同じ値は同じく重複する・3 回目の検証 3-B-3）。REST・取込・
        // チャットと同じ `unique_violation`・列の表示名つきの文言。
        e @ DataError::UniqueViolation { .. } => PortError::new(
            "unique_violation",
            e.conflict_detail().unwrap_or_default(),
            false,
        ),
        // rev の不一致（rev を指定しない書込は再試行で読み直して通り得る）。
        e @ (DataError::Conflict(_)
        | DataError::RevConflict { .. }
        | DataError::DerivedRecomputing { .. }) => {
            PortError::new("conflict", e.conflict_detail().unwrap_or_default(), true)
        }
        DataError::Gone(m) => PortError::new("gone", m, false),
        DataError::ReadOverloaded => {
            PortError::new("rate_limited", "data の読取が混み合っています", true)
        }
        DataError::ReadTimeout => PortError::new(
            "read_timeout",
            "data の読取が時間内に終わりませんでした",
            true,
        ),
        e @ (DataError::IndexPreparing { .. } | DataError::IndexApprovalRequired { .. }) => {
            PortError::new("index_preparing", e.to_string(), true)
        }
        e @ (DataError::QueryTooExpensive { .. } | DataError::Rejected(_)) => {
            PortError::invalid(e.to_string())
        }
        DataError::Internal(m) => PortError::unavailable(format!("data: {m}")),
    }
}

/// 一括 upsert の取込 ID をステップの冪等キーから決める（再実行で同じ ID になり、二重に取り込まない）。
fn import_id_for(tenant_id: &str, idempotency_key: &str) -> Uuid {
    use sha2::{Digest, Sha256};
    let h = Sha256::digest(format!("data.bulk_upsert\0{tenant_id}\0{idempotency_key}").as_bytes());
    let mut b = [0u8; 16];
    b.copy_from_slice(&h[..16]);
    uuid::Builder::from_random_bytes(b).into_uuid()
}

/// data のポート（`ProdNodePorts` の data 部分）。
pub(super) struct DataPort<'a> {
    pub data: &'a DataStore,
    pub fsms: &'a FsmStore,
    pub db: &'a PgPool,
}

impl DataPort<'_> {
    /// 読み書きする主体（上の ①②③）。`relation` は委譲で要るテーブルの関係（読取 viewer・書込 editor）。
    async fn actor(
        &self,
        ec: &ExecCtx,
        table_id: Uuid,
        relation: Relation,
    ) -> Result<Actor, PortError> {
        let ctx = auth_ctx(ec);
        if ec.principal_kind != "workflow" {
            return Ok(Actor {
                ctx,
                data: self.data.clone(),
                fsms: self.fsms.clone(),
            });
        }
        // ① 委譲されたテーブルか（タプル・剥奪は即時に効く）。
        self.data
            .authorize_table(
                &ctx,
                table_id,
                relation,
                storage::audit::AuditAction::WorkflowDataDelegated,
                ec.trace_id.as_deref(),
            )
            .await
            .map_err(map_data)?;
        // ② 委譲元ユーザー。**この操作に足る relation を自ら委譲した**者からだけ選ぶ（書込なら
        //    editor / owner）。①は別の委譲元のタプルで通り得るので、ここで relation を見ないと、
        //    書込を委譲していないユーザーの権限で書込が走る（confused-deputy・R3-2）。該当が無ければ
        //    拒否（fail-closed）。
        let workflow_id = Uuid::parse_str(&ec.principal)
            .map_err(|_| PortError::forbidden("ワークフロー主体の id が不正です"))?;
        let object = ctx.ns().data_table(&table_id.to_string());
        let delegator = workflow_engine::active_delegator(
            self.db,
            &ec.tenant_id,
            workflow_id,
            &object,
            relation,
        )
        .await
        .map_err(|e| PortError::unavailable(format!("delegation: {e}")))?
        .ok_or_else(|| PortError::forbidden("このテーブルは委譲されていません"))?;
        // ③ 辿ってよいテーブルの天井: この委譲元が自ら（viewer 以上で）委譲したテーブル。併せて
        //    ワークフロー主体のタプルも要求する（棚卸しの撤去を即時に効かせる）。
        let reach = self
            .delegated_tables(&ctx, workflow_id, &delegator, table_id)
            .await?
            .also_held_by(ctx.clone());
        let actor = AuthContext::new(
            Principal {
                kind: PrincipalKind::User,
                id: delegator,
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some(ec.tenant_id.clone()),
            },
            ec.org.clone(),
            ec.tenant_id.clone(),
        );
        Ok(Actor {
            ctx: actor,
            data: self.data.within_reach(reach.clone()),
            fsms: self.fsms.within_reach(reach),
        })
    }

    /// 委譲元 `delegator` がワークフローへ委譲したテーブル（入口の `table_id` を含む）の天井。
    async fn delegated_tables(
        &self,
        ctx: &AuthContext,
        workflow_id: Uuid,
        delegator: &str,
        table_id: Uuid,
    ) -> Result<TableReach, PortError> {
        const PREFIX: &str = "data_table:";
        let objects = workflow_engine::delegated_objects(
            self.db,
            &ctx.tenant_id,
            workflow_id,
            delegator,
            PREFIX,
            Relation::Viewer,
            MAX_REACHABLE_TABLES,
        )
        .await
        .map_err(|e| PortError::unavailable(format!("delegation: {e}")))?;
        let ns = ctx.ns();
        let tables = objects
            .iter()
            .filter_map(|o| o.strip_prefix(PREFIX))
            .filter_map(|rest| ns.strip_object_id(rest))
            .filter_map(|local| Uuid::parse_str(local).ok());
        Ok(TableReach::only(tables.chain([table_id])))
    }

    /// 実行主体（ワークフロー主体なら委譲されたテーブル）の一覧。中身ではなく名前と id だけ。
    pub(super) async fn list_tables(&self, ec: &ExecCtx) -> Result<Value, PortError> {
        let ctx = auth_ctx(ec);
        let tables = self
            .data
            .list_tables(&ctx, MAX_LISTED_TABLES)
            .await
            .map_err(map_data)?;
        let items: Vec<Value> = tables
            .iter()
            .map(|t| json!({ "id": t.id, "name": t.name }))
            .collect();
        Ok(json!({ "items": items }))
    }

    pub(super) async fn query(
        &self,
        ec: &ExecCtx,
        table_id: Uuid,
        query: &DataQuery,
    ) -> Result<Value, PortError> {
        let a = self.actor(ec, table_id, Relation::Viewer).await?;
        let out = a
            .data
            .run_query(&a.ctx, table_id, query, ec.trace_id.as_deref())
            .await
            .map_err(map_data)?;
        serde_json::to_value(out).map_err(|e| PortError::unavailable(format!("serialize: {e}")))
    }

    pub(super) async fn get(
        &self,
        ec: &ExecCtx,
        table_id: Uuid,
        record_id: Uuid,
    ) -> Result<Value, PortError> {
        let a = self.actor(ec, table_id, Relation::Viewer).await?;
        let rec = a
            .data
            .get_record(&a.ctx, table_id, record_id, ec.trace_id.as_deref())
            .await
            .map_err(map_data)?;
        serde_json::to_value(rec).map_err(|e| PortError::unavailable(format!("serialize: {e}")))
    }

    /// 現在の rev（rev を指定しない書込が、読んだ rev で楽観ロックを掛けるため）。
    async fn current_rev(
        a: &Actor,
        table_id: Uuid,
        record_id: Uuid,
        trace: Option<&str>,
    ) -> Result<i64, PortError> {
        a.data
            .get_record(&a.ctx, table_id, record_id, trace)
            .await
            .map(|r| r.rev)
            .map_err(map_data)
    }

    pub(super) async fn write(&self, ec: &ExecCtx, req: DataWriteReq) -> Result<Value, PortError> {
        let table_id = req.table_id;
        let trace = ec.trace_id.as_deref();
        let a = self.actor(ec, table_id, Relation::Editor).await?;
        let to_value = |r: data::DataRecord| {
            serde_json::to_value(r).map_err(|e| PortError::unavailable(format!("serialize: {e}")))
        };
        match req.op {
            DataWriteOp::Create { data } => to_value(
                a.data
                    .create_record(&a.ctx, table_id, data, trace)
                    .await
                    .map_err(map_data)?,
            ),
            DataWriteOp::Update {
                record_id,
                patch,
                expected_rev,
            } => {
                let rev = match expected_rev {
                    Some(r) => r,
                    None => Self::current_rev(&a, table_id, record_id, trace).await?,
                };
                to_value(
                    a.data
                        .update_record(&a.ctx, table_id, record_id, patch, rev, trace)
                        .await
                        .map_err(map_data)?,
                )
            }
            DataWriteOp::Delete {
                record_id,
                expected_rev,
            } => {
                let rev = match expected_rev {
                    Some(r) => r,
                    None => Self::current_rev(&a, table_id, record_id, trace).await?,
                };
                a.data
                    .delete_record(&a.ctx, table_id, record_id, rev, trace)
                    .await
                    .map_err(map_data)?;
                Ok(json!({ "id": record_id, "deleted": true }))
            }
            DataWriteOp::Transition {
                record_id,
                to,
                expected_rev,
            } => {
                // テーブルにピンされた FSM をテーブルの editor として読み、当該行で遷移する（api の遷移
                // ルート・ゲートウェイと同じ単一の入口 FsmStore::transition・検証 C-P1-1）。
                let rev = match expected_rev {
                    Some(r) => r,
                    None => Self::current_rev(&a, table_id, record_id, trace).await?,
                };
                to_value(
                    a.fsms
                        .transition(&a.ctx, table_id, record_id, &to, rev, trace)
                        .await
                        .map_err(map_data)?,
                )
            }
            DataWriteOp::BulkUpsert {
                rows,
                abort_on_invalid,
            } => {
                Self::bulk_upsert(
                    ec,
                    &a,
                    table_id,
                    rows,
                    abort_on_invalid,
                    &req.idempotency_key,
                )
                .await
            }
        }
    }

    /// 一括 upsert を始めて最後まで進め、件数と拒否行を返す（取込 ID は冪等キーから決める）。
    async fn bulk_upsert(
        ec: &ExecCtx,
        a: &Actor,
        table_id: Uuid,
        rows: Vec<data::BulkUpsertRow>,
        abort_on_invalid: bool,
        idempotency_key: &str,
    ) -> Result<Value, PortError> {
        let import_id = import_id_for(&ec.tenant_id, idempotency_key);
        let opts = BulkImportOptions {
            abort_on_invalid,
            ..BulkImportOptions::default()
        };
        a.data
            .start_bulk_upsert_with_id(
                &a.ctx,
                table_id,
                import_id,
                rows,
                opts,
                ec.trace_id.as_deref(),
            )
            .await
            .map_err(map_data)?;
        // ノードの timeout の内側で同期に最後まで進める（同じ取込をジョブのワーカーが並行に進めても
        // チャンクのカーソルで直列化される）。
        a.data
            .run_bulk_import(&ec.tenant_id, table_id, import_id)
            .await
            .map_err(map_data)?;
        let st = a
            .data
            .bulk_import_status(&a.ctx, table_id, import_id)
            .await
            .map_err(map_data)?;
        let errors = a
            .data
            .bulk_import_errors(&a.ctx, table_id, import_id, 0, MAX_REPORTED_ROW_ERRORS)
            .await
            .map_err(map_data)?;
        let errors: Vec<Value> = errors
            .iter()
            .map(|e| json!({ "row": e.row_no, "key": e.key, "message": e.message }))
            .collect();
        Ok(json!({
            "import_id": import_id,
            "status": st.status,
            "rows_total": st.rows_total,
            "rows_applied": st.rows_applied,
            "rows_invalid": st.rows_invalid,
            "rows_rejected": st.rows_rejected,
            "errors": errors,
            "last_error": st.last_error,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_id_is_stable_per_step_and_tenant() {
        let a = import_id_for("t1", "wf:t1:r:n1");
        assert_eq!(a, import_id_for("t1", "wf:t1:r:n1"));
        assert_ne!(a, import_id_for("t2", "wf:t1:r:n1"));
        assert_ne!(a, import_id_for("t1", "wf:t1:r:n2"));
    }

    #[test]
    fn invisible_and_missing_map_to_the_same_error() {
        assert_eq!(map_data(DataError::NotFound).code, "not_found");
        assert_eq!(map_data(DataError::Forbidden).code, "forbidden");
        assert!(map_data(DataError::Conflict("rev".into())).retryable);
        assert!(!map_data(DataError::Invalid("x".into())).retryable);
        assert!(map_data(DataError::Internal("db".into())).retryable);
    }

    /// unique 違反は REST・取込・チャットと同じ `unique_violation`・再試行不可・列の表示名つき（3-B-3）。
    #[test]
    fn unique_violation_is_not_retryable_and_names_the_display_name() {
        let e = map_data(DataError::UniqueViolation {
            field: Some(data::ConflictField {
                key: "code".into(),
                display_name: "社員番号".into(),
                link: false,
            }),
        });
        assert_eq!(e.code, "unique_violation");
        assert!(!e.retryable);
        assert!(
            e.message.contains("社員番号") && !e.message.contains("code"),
            "{}",
            e.message
        );
        let e = map_data(DataError::UniqueViolation { field: None });
        assert_eq!(e.code, "unique_violation");
        assert!(!e.retryable);
    }
}

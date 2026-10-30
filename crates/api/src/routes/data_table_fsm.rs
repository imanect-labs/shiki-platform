//! テーブルの「状態の進め方」の読み書き（テーブルの持ち主だけ・GAP-44・D-84）。
//!
//! テーブルの設定の画面が使う。定義の保存は data のチョークポイント（[`data::FsmStore::save_for_table`]）で、
//! ここは保存の前に**承認を待っているレコードと保存済みのワークフローへの影響**を集めて足す。
//!
//! - 承認を待っているレコード（`workflow_approval`）: いまの状態から判断の行き先への組ごとの数。保存で組の進め方が
//!   無くなるなら、`confirm_pending` が無い限り 409 `fsm_in_use` で止める（判断が遷移の段で断られ、承認の期限か
//!   実行の期限まで残るため）。どのワークフロー・どのレコードかは返さない（数だけ）。
//! - 保存済みのワークフロー: このテーブルの状態を使う（承認の判断の行き先・状態を進める先・状態のきっかけと
//!   待ちの絞り込み）もののうち、持ち主が見られるもの。止めずに画面で注意を出す材料にする。
//! - 書き込みの決まり（最初の状態を過ぎたら申請者は書き換えられない）を付けると、だれも書き換え・削除できなくなる
//!   レコード（状態が最初の状態でなく、申請者の列が空）: `confirm_locked` が無い限り 409 `fsm_locks_records`
//!   （数だけ）で止める。途中から申請者の列を足したテーブルで、保存済みのワークフローの更新・削除が断られ始めるため。

use std::collections::BTreeSet;

use axum::{
    extract::{Path, State},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use storage::audit::AuditAction;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// 保存済みのワークフローを探す上限（新しく更新した順）。
const MAX_WORKFLOWS: i64 = 50;

/// 承認を待っているレコードの、いまの状態から判断の行き先への 1 組と、その数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct PendingApprovalMove {
    /// 承認を始めたときのレコードの状態。
    pub from: String,
    /// 判断で進める状態。
    pub to: String,
    pub count: i64,
}

/// このテーブルの状態を使う保存済みのワークフロー（見られるものだけ）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct WorkflowStateUse {
    pub id: Uuid,
    /// 一覧に出る名前。
    pub name: String,
    /// 使っている状態（判断の行き先・状態を進める先・きっかけと待ちの絞り込みの状態）。
    pub states: Vec<String>,
}

/// テーブルの状態の進め方と、変えたときに影響を受けるもの。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TableFsmResponse {
    /// 状態の列の参照名（無ければ null）。
    pub status_field: Option<String>,
    /// テーブルが指している状態の進め方（無ければ null）。
    pub fsm: Option<data::PinnedFsm>,
    /// 「最初の状態を過ぎたら、申請者はレコードを書き換えられない」決まりの申請者の列（オフなら null）。
    pub applicant_write_lock: Option<String>,
    /// いまのスキーマの版（保存の `expected_schema_version` に渡す）。
    pub schema_version: i64,
    /// 承認を待っているレコード（いまの状態 → 判断の行き先ごとの数）。
    pub pending_approvals: Vec<PendingApprovalMove>,
    /// このテーブルの状態を使う保存済みのワークフロー。
    pub workflows: Vec<WorkflowStateUse>,
}

/// 状態の進め方の保存。
#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveTableFsmRequest {
    /// 状態の列（選択肢の列）。状態の進め方を作った後は変えられない。
    pub status_field: String,
    /// 状態の一覧（先頭が最初の状態）と進め方。状態の列の選択肢は、この一覧（この順）になる。
    pub body: data::FsmBody,
    /// 「最初の状態を過ぎたら、申請者はレコードを書き換えられない」決まりの申請者の列（メンバーの列）。
    /// 省略（null）なら、この決まりを外す。
    #[serde(default)]
    pub applicant_write_lock: Option<String>,
    /// 読んだときのスキーマの版（違えば 409）。
    pub expected_schema_version: i64,
    /// 承認を待っているレコードが判断できなくなる変更でも保存する（無ければ 409 `fsm_in_use` で止める）。
    #[serde(default)]
    pub confirm_pending: bool,
    /// 書き込みの決まりを付けると、だれも書き換え・削除できなくなるレコードがあっても保存する（無ければ 409
    /// `fsm_locks_records` で止める）。
    #[serde(default)]
    pub confirm_locked: bool,
}

/// 409 `fsm_in_use`: 保存すると、承認を待っているレコードが判断できなくなる。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FsmInUseBody {
    /// 常に 409。
    pub status: u16,
    pub title: String,
    /// 常に `fsm_in_use`。
    pub code: String,
    /// 利用者に見せてよい文言。
    pub message: String,
    /// 判断できなくなる組（いまの状態 → 判断の行き先・数）。
    pub pending: Vec<PendingApprovalMove>,
}

/// 409 `fsm_locks_records`: 書き込みの決まりを付けると、だれも書き換え・削除できなくなるレコードがある
/// （状態が最初の状態でなく、申請者の列が空のレコード）。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FsmLocksRecordsBody {
    /// 常に 409。
    pub status: u16,
    pub title: String,
    /// 常に `fsm_locks_records`。
    pub code: String,
    /// 利用者に見せてよい文言。
    pub message: String,
    /// 書き換えられなくなるレコードの数（上限で打ち切る）。
    pub count: i64,
    /// 上限を超えた（「{count} 件以上」）。
    pub more: bool,
}

fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError::Internal(e.to_string())
}

/// 承認を待っているレコードの組ごとの数。
async fn pending_moves(
    state: &AppState,
    tenant_id: &str,
    table_id: Uuid,
) -> Result<Vec<PendingApprovalMove>, ApiError> {
    Ok(state
        .workflow_approvals
        .pending_moves(tenant_id, table_id)
        .await
        .map_err(internal)?
        .into_iter()
        .map(|m| PendingApprovalMove {
            from: m.from,
            to: m.to,
            count: m.count,
        })
        .collect())
}

/// テーブルの状態の進め方（持ち主だけ・見えないテーブルは 404・持ち主でなければ 403）。
#[utoipa::path(
    get,
    path = "/data/tables/{id}/fsm",
    params(("id" = Uuid, Path, description = "テーブル ID")),
    responses(
        (status = 200, description = "状態の進め方と、変えたときに影響を受けるもの", body = TableFsmResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "テーブルの持ち主でない"),
        (status = 404, description = "テーブルが見つからない（見えないを含む）"),
    ),
    security(("session" = [])),
)]
pub async fn get_table_fsm(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<TableFsmResponse>, ApiError> {
    let view = state.fsms.table_fsm(&ctx, id, trace.as_deref()).await?;
    let pending_approvals = pending_moves(&state, &ctx.tenant_id, id).await?;
    let workflows = state
        .artifacts
        .list_visible_mentioning(&ctx, artifact::ArtifactKind::Workflow, id, MAX_WORKFLOWS)
        .await
        .map_err(internal)?
        .into_iter()
        .filter_map(|(a, body)| {
            let states: Vec<String> = states_used(&body, id).into_iter().collect();
            if states.is_empty() {
                return None;
            }
            let name = body
                .get("display_name")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map_or_else(|| a.name.clone(), str::to_string);
            Some(WorkflowStateUse {
                id: a.id,
                name,
                states,
            })
        })
        .collect();
    Ok(Json(TableFsmResponse {
        status_field: view.status_field,
        fsm: view.fsm,
        applicant_write_lock: view.applicant_write_lock,
        schema_version: view.schema_version,
        pending_approvals,
        workflows,
    }))
}

/// テーブルの状態の進め方を保存する（持ち主だけ・状態の列の選択肢・指す版・書き込みの決まりを 1 回の改訂で替える）。
#[utoipa::path(
    put,
    path = "/data/tables/{id}/fsm",
    params(("id" = Uuid, Path, description = "テーブル ID")),
    request_body = SaveTableFsmRequest,
    responses(
        (status = 200, description = "改訂後のテーブル", body = data::DataTable),
        (status = 400, description = "状態の進め方が正しくない（code=fsm_invalid）・使われている状態を消す（code=schema_conflicts_with_data）", body = crate::error_body::BadRequestBody),
        (status = 401, description = "未認証"),
        (status = 403, description = "テーブルの持ち主でない"),
        (status = 404, description = "テーブルが見つからない（見えないを含む）"),
        (status = 409, description = "スキーマの版が違う（code=conflict）・承認を待っているレコードが判断できなくなる（code=fsm_in_use・FsmInUseBody）・書き込みの決まりでだれも書き換えられなくなるレコードがある（code=fsm_locks_records・FsmLocksRecordsBody）", body = FsmInUseBody),
    ),
    security(("session" = [])),
)]
pub async fn save_table_fsm(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<SaveTableFsmRequest>,
) -> Result<Json<data::DataTable>, ApiError> {
    // 承認を待っている数は持ち主にだけ見せる（先に持ち主を確かめる）。
    state
        .data
        .authorize_table(
            &ctx,
            id,
            authz::Relation::Owner,
            AuditAction::DataTableUpdateSchema,
            trace.as_deref(),
        )
        .await?;
    if !req.confirm_pending {
        let lost: Vec<PendingApprovalMove> = pending_moves(&state, &ctx.tenant_id, id)
            .await?
            .into_iter()
            .filter(|m| req.body.transition(&m.from, &m.to).is_none())
            .collect();
        if !lost.is_empty() {
            let n: i64 = lost.iter().map(|m| m.count).sum();
            let body = FsmInUseBody {
                status: 409,
                title: "Conflict".into(),
                code: "fsm_in_use".into(),
                message: format!(
                    "承認を待っているレコードが {n} 件あり、この変更で判断できなくなります。進め方を戻すか、承認が終わってから保存してください。"
                ),
                pending: lost,
            };
            return Err(ApiError::ConflictJson(
                serde_json::to_value(body).map_err(internal)?,
            ));
        }
    }
    let save = data::SaveTableFsm {
        status_field: req.status_field,
        body: req.body,
        applicant_write_lock: req.applicant_write_lock,
        expected_schema_version: req.expected_schema_version,
    };
    // 書き込みの決まりを付けると、途中から申請者の列を足したテーブルの既存のレコードが、持ち主・編集者・保存済みの
    // ワークフローのどれからも書き換えられなくなる。数を言って確かめてから保存する（承認待ちと同じく数だけ返す）。
    if !req.confirm_locked {
        if let Some(locked) = state.fsms.records_locked_by(&ctx, id, &save).await? {
            let amount = locked.amount();
            let body = FsmLocksRecordsBody {
                status: 409,
                title: "Conflict".into(),
                code: "fsm_locks_records".into(),
                message: format!(
                    "申請者の列が空で、最初の状態を過ぎたレコードが {amount}あります。保存すると、これらのレコードは誰も書き換えたり削除したりできなくなります。"
                ),
                count: locked.count,
                more: locked.more,
            };
            return Err(ApiError::ConflictJson(
                serde_json::to_value(body).map_err(internal)?,
            ));
        }
    }
    let saved = state
        .fsms
        .save_for_table(&ctx, id, &save, trace.as_deref())
        .await?;
    Ok(Json(saved))
}

/// ワークフローの定義（IR）が、テーブル `table` の状態として使っている名前。
///
/// 承認を待つの判断の行き先、状態を進めるの次の状態（直接入力のときだけ）、状態の変化のきっかけと待ちの絞り込みで
/// 比べている状態。値を入力や結果から選ぶものは、実行まで分からないので数えない。
pub(crate) fn states_used(ir: &Value, table: Uuid) -> BTreeSet<String> {
    let same = |v: Option<&Value>| {
        v.and_then(Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
            == Some(table)
    };
    let mut out = BTreeSet::new();
    let mut put = |v: Option<&Value>| {
        if let Some(s) = v.and_then(Value::as_str) {
            out.insert(s.to_string());
        }
    };
    let transition_source =
        |v: Option<&Value>| v.and_then(Value::as_str) == Some("data.transition");
    let mut filters: Vec<&Value> = Vec::new();
    for n in ir
        .get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let params = n.get("params");
        match n.get("type").and_then(Value::as_str) {
            Some("human.approval") if same(n.pointer("/params/record/table")) => {
                for k in ["approve", "reject", "return"] {
                    put(params.and_then(|p| p.get(k)).and_then(|d| d.get("to")));
                }
            }
            Some("data.transition") if same(n.pointer("/params/table")) => {
                put(params.and_then(|p| p.get("to")));
            }
            Some("control.wait")
                if transition_source(n.pointer("/params/source"))
                    && same(n.pointer("/params/scope/table")) =>
            {
                filters.extend(n.pointer("/params/filter"));
            }
            _ => {}
        }
    }
    for t in ir
        .get("triggers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if t.get("kind").and_then(Value::as_str) == Some("event")
            && transition_source(t.get("source"))
            && same(t.pointer("/scope/table"))
        {
            filters.extend(t.get("filter"));
        }
    }
    for f in filters {
        filter_states(f, &mut out);
    }
    out
}

/// 条件木の中で、きっかけ（変更）の前後の状態（`/to`・`/from`）と比べている名前。
fn filter_states(v: &Value, out: &mut BTreeSet<String>) {
    match v {
        Value::Object(m) => {
            if let Some(cmp) = m.get("cmp") {
                let path = cmp.pointer("/left/path").and_then(Value::as_str);
                if matches!(path, Some("/to" | "/from")) {
                    match cmp.get("right") {
                        Some(Value::String(s)) => {
                            out.insert(s.clone());
                        }
                        Some(Value::Array(a)) => {
                            out.extend(a.iter().filter_map(Value::as_str).map(str::to_string));
                        }
                        _ => {}
                    }
                }
            }
            for c in m.values() {
                filter_states(c, out);
            }
        }
        Value::Array(a) => {
            for c in a {
                filter_states(c, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const T: &str = "00000000-0000-0000-0000-0000000000f1";
    const OTHER: &str = "00000000-0000-0000-0000-0000000000a2";

    /// 承認の判断の行き先・状態を進める先・きっかけと待ちの絞り込みの状態を拾い、ほかのテーブル・入力や結果から
    /// 選ぶ値・状態でない比べは拾わない。テーブルの id は大文字でも同じテーブル。
    #[test]
    fn collects_the_states_a_workflow_uses() {
        let table = Uuid::parse_str(T).unwrap_or_default();
        let ir = json!({
            "triggers": [
                { "kind": "event", "source": "data.transition", "scope": { "table": T.to_uppercase() },
                  "filter": { "all": [
                      { "cmp": { "left": { "$from": "trigger", "path": "/to" }, "op": "eq", "right": "申請中" } },
                      { "cmp": { "left": { "$from": "trigger", "path": "/record/amount" }, "op": "gt", "right": "100" } }
                  ] } },
                { "kind": "event", "source": "data.transition", "scope": { "table": OTHER },
                  "filter": { "cmp": { "left": { "$from": "trigger", "path": "/to" }, "op": "eq", "right": "別の表" } } }
            ],
            "nodes": [
                { "id": "ok", "type": "human.approval",
                  "params": { "record": { "table": T }, "approve": { "to": "承認" }, "reject": { "to": "却下" } } },
                { "id": "mv", "type": "data.transition", "params": { "table": T, "to": "受付" } },
                { "id": "ref", "type": "data.transition",
                  "params": { "table": T, "to": { "$from": "nodes", "path": "/x" } } },
                { "id": "w", "type": "control.wait",
                  "params": { "kind": "event", "source": "data.transition", "scope": { "table": T },
                              "filter": { "any": [
                                  { "cmp": { "left": { "$from": "event", "path": "/from" }, "op": "in",
                                             "right": ["差し戻し", "保留"] } } ] } } },
                { "id": "q", "type": "data.transition", "params": { "table": OTHER, "to": "別" } }
            ]
        });
        let got: Vec<String> = states_used(&ir, table).into_iter().collect();
        assert_eq!(got, ["保留", "却下", "受付", "差し戻し", "承認", "申請中"]);
        assert!(states_used(&json!({}), table).is_empty());
    }
}

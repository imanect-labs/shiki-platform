//! 承認の受信箱 API（human.approval・GAP-08・D-24・D-27）。
//!
//! - 一覧・件数・1 件は、台帳（`workflow_approval`）で本人か所属ロール宛ての保留中の承認を引き、**行を承認者
//!   本人の `AuthContext` で data のチョークポイントを通して読み直して**、見える行で今の状態が停止時の状態の
//!   ものだけを出す（二段 authz・見えない行は数えない）。出す列は担当者に読める列だけ（マスク後）。行は
//!   テーブルごとに候補の id を 1 本のクエリで引く。一覧・件数は台帳の候補を絞るだけなので、見えないテーブルを
//!   Deny の監査に残さない（本人が開いた 1 件は残す・BUG-43）。所属ロールと件数の数え方は
//!   [`super::approvals_inbox`]（件数は行の中身を運ばず、見えるかと今の状態だけを確かめる）。
//! - 判断は、承認者本人の `AuthContext` で `FsmStore::transition(expected_rev)` を呼ぶ（ワークフローの権限で
//!   遷移しない）。actor 述語・行の可視性・テーブルの editor・rev はすべて data が判定する（判定の正は 1 か所・
//!   テーブル画面から遷移しても同じ承認が決まる）。run を進めるのは、遷移の outbox を受けた relay。
//! - 担当者でない・存在しない・行が見えないは、どれも同じ 404（存在の手がかりを出さない）。判断も、行が本人に
//!   見えるかを台帳の状態（決まった・期限切れ）・選択肢・コメントの検査より前に確かめる。
//!
//! 判断に添えるコメントは、遷移の後の rev と作者の鍵で遷移の前に置き、遷移に失敗したら外す。置く → 遷移 →
//! 外す、は台帳の行のロックを持った 1 つの Tx で行い、要求が取り消されても別の task で最後まで走らせる（遷移と
//! 同時に起床しても commit を待って拾える・失敗した判断のコメントを残さない・BUG-47）。

use std::collections::BTreeMap;

use authz::AuthContext;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use storage::audit::{AuditAction, AuditEntry, Decision};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;
use workflow_engine::ir::params::{ApprovalComment, ApprovalDecisionKind};
use workflow_engine::{AcceptedDecision, ApprovalRow, DecisionStart, InboxCursor};

use super::approvals_inbox::{self, my_roles, SCAN_LIMIT};
use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// 1 ページの件数。
const PAGE: i64 = 50;
/// コメントの上限（文字数・台帳の check と同じ）。
const MAX_COMMENT_CHARS: usize = 2000;

/// 判断の選択肢（ボタン）。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ApprovalDecisionDto {
    pub kind: ApprovalDecisionKind,
    /// 判断で進める状態（ボタンには文言と一緒に必ず出す）。
    pub to: String,
    /// ボタンの文言。
    pub label: String,
    /// コメントが必須か。
    pub comment_required: bool,
    /// 本人がいま実行できるか（状態の遷移の規則で許されるか・1 件の取得だけ。一覧では null）。
    /// 表示の出し分けだけに使う（判断の API が改めて認可する）。
    pub allowed: Option<bool>,
}

/// 受信箱に出す列 1 つ（担当者に読める列だけ）。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ApprovalFieldDto {
    pub key: String,
    pub display_name: String,
    /// 列の種別（画面が値の見せ方を決める・ユーザーの列は表示名で出すなど）。
    pub field_type: data::FieldType,
    pub value: serde_json::Value,
}

/// 承認するテーブル。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ApprovalTableDto {
    pub id: Uuid,
    pub name: String,
}

/// 承認する行（承認者本人の見え方）。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ApprovalRecordDto {
    pub id: Uuid,
    /// いまの rev（判断の `expected_rev` に渡す）。
    pub rev: i64,
    pub owner: String,
    pub fields: Vec<ApprovalFieldDto>,
    /// リンク列の参照先の表示名（見える参照先だけ）。
    pub link_labels: BTreeMap<String, String>,
}

/// 受信箱の 1 件。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ApprovalItem {
    pub approval_id: Uuid,
    pub workflow_id: Uuid,
    pub title: String,
    pub instructions: Option<String>,
    pub table: ApprovalTableDto,
    pub record: ApprovalRecordDto,
    /// 判断を待っている状態（いまの状態）。
    pub from_state: String,
    /// 判断の期限（承認の期限と実行の期限の早い方）。
    pub deadline: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub decisions: Vec<ApprovalDecisionDto>,
}

/// 受信箱の一覧（新しい順・50 件ずつ）。
#[derive(Debug, Serialize, ToSchema)]
pub struct ApprovalListResponse {
    /// 本人に見える承認（50 件まで）。見えない承認が続くと、読み直す台帳の行の上限で打ち切るので、
    /// 50 件に満たない（空の）ことがある。そのときも `next_cursor` があれば続きがある。
    pub items: Vec<ApprovalItem>,
    /// 次のページの位置（無ければ null・`items` が空でも続きを読める）。
    pub next_cursor: Option<String>,
}

/// 受信箱の件数（99 で打ち切り）。
#[derive(Debug, Serialize, ToSchema)]
pub struct ApprovalCountResponse {
    pub count: u32,
    /// 99 件を超えている、または見えない承認が多く数え切れなかった（読み直す台帳の行の上限で打ち切った・
    /// `count` は数えられた分）。
    pub more: bool,
}

/// 一覧のクエリ。
#[derive(Debug, Deserialize, IntoParams)]
pub struct ApprovalListQuery {
    /// 前のページの `next_cursor`。
    pub cursor: Option<String>,
}

/// 判断の要求。
#[derive(Debug, Deserialize, ToSchema)]
pub struct ApprovalDecisionRequest {
    pub decision: ApprovalDecisionKind,
    /// コメント（必須の判断では空にできない・2000 字以内）。
    pub comment: Option<String>,
    /// 判断する行の rev（受信箱で見た rev・違えば 409）。
    pub expected_rev: i64,
}

/// 判断の受付（run は遷移を受けた relay が進める）。
#[derive(Debug, Serialize, ToSchema)]
pub struct ApprovalDecisionResponse {
    /// 常に `deciding`（遷移は済み・run の起床は数秒以内）。
    pub status: String,
    /// 遷移の後の rev。
    pub rev: i64,
}

/// 判断の誤りの code（画面が日本語の文言に写す）。
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalErrorCode {
    /// コメントが必須の判断でコメントが無い（400）。
    CommentRequired,
    /// コメントが長すぎる（400）。
    CommentTooLong,
    /// この承認に無い判断（400）。
    UnknownDecision,
    /// もう決まった・取り消された（409）。
    Decided,
    /// 期限を過ぎた（409）。
    Expired,
    /// 別の担当者が同じ版で判断中（409）。
    Deciding,
    /// 状態の遷移の規則で、本人にはこの判断ができない（403）。
    NotAllowed,
    /// いまの状態からは判断の行き先へ進めない（422）。
    NotDecidable,
}

/// 判断の誤りの本文。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ApprovalErrorBody {
    pub status: u16,
    pub code: ApprovalErrorCode,
    /// 利用者に見せてよい文言。
    pub message: String,
}

fn approval_error(status: StatusCode, code: ApprovalErrorCode, message: &str) -> ApiError {
    let body = serde_json::to_value(ApprovalErrorBody {
        status: status.as_u16(),
        code,
        message: message.to_string(),
    })
    .unwrap_or_else(|_| json!({ "status": status.as_u16(), "message": message }));
    match status {
        StatusCode::FORBIDDEN => ApiError::ForbiddenJson(body),
        StatusCode::CONFLICT => ApiError::ConflictJson(body),
        StatusCode::UNPROCESSABLE_ENTITY => ApiError::Unprocessable422(body),
        _ => ApiError::UnprocessableJson(body),
    }
}

fn store(state: &AppState) -> &workflow_engine::ApprovalStore {
    &state.workflow_approvals
}

fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError::Internal(format!("approvals: {e}"))
}

/// 台帳の行の読み方（見えないテーブルを Deny の監査に残すか）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reading {
    /// 受信箱の一覧・件数: 他人（IR の作者）が書いた台帳の候補を、本人に見える分へ絞るだけ。見えない
    /// テーブルは監査に残さず黙って落とす（件数を読み直すたびに Deny が増えない・BUG-43）。
    Inbox,
    /// 1 件の取得: 本人が開いた承認。判断ごとに本人が実行できるか（`FsmStore::allowed_transitions`）を添え、
    /// 見えないテーブルは Deny の監査に残す。
    Opened,
}

/// 台帳の行を、承認者本人の見え方で読み直した受信箱の項目にする（見えない・状態が動いたものは落とす）。
///
/// 行はテーブルごとに候補の id を 1 本のクエリで引く（data のまとめ取り・行ごとの読取をしない・BUG-43）。
async fn visible_items(
    state: &AppState,
    ctx: &AuthContext,
    rows: Vec<ApprovalRow>,
    reading: Reading,
    trace: Option<&str>,
) -> Result<Vec<ApprovalItem>, ApiError> {
    let mut tables: Vec<Uuid> = Vec::new();
    for r in &rows {
        if !tables.contains(&r.table_id) {
            tables.push(r.table_id);
        }
    }
    let with_allowed = reading == Reading::Opened;
    let mut seen: BTreeMap<Uuid, ApprovalItem> = BTreeMap::new();
    for table_id in tables {
        let mine: Vec<&ApprovalRow> = rows.iter().filter(|r| r.table_id == table_id).collect();
        let ids: Vec<Uuid> = mine.iter().map(|r| r.record_id).collect();
        let read = match reading {
            Reading::Inbox => state.data.filter_visible_records(ctx, table_id, &ids).await,
            Reading::Opened => opened_records(state, ctx, table_id, &ids, trace).await,
        };
        let Some(data::FilteredRecords { table, records }) = per_table(read, table_id)?.flatten()
        else {
            continue;
        };
        let status_field = table.schema.status_field.clone().unwrap_or_default();
        for (row, rec) in mine.into_iter().zip(records) {
            let Some(rec) = rec else { continue };
            // 今の状態が停止時の状態のものだけ（判断の直後に再取得しても出ない・状態が読めない人にも出ない）。
            if rec
                .data
                .get(&status_field)
                .and_then(serde_json::Value::as_str)
                != Some(row.from_state.as_str())
            {
                continue;
            }
            let allowed = if with_allowed {
                match state
                    .fsms
                    .allowed_transitions(ctx, table_id, row.record_id, trace)
                    .await
                {
                    Ok(a) => Some(a.transitions),
                    Err(e) if e.is_transient() => return Err(e.into()),
                    // 権限が無い・行が見えない・状態の列が読めない・その他の確定的な誤り: 判断できないと出す
                    // （判断の API が改めて認可する）。
                    Err(_) => Some(Vec::new()),
                }
            } else {
                None
            };
            let fields = row
                .show_fields
                .iter()
                .filter_map(|key| {
                    let value = rec.data.get(key)?.clone();
                    let def = table.schema.field(key)?;
                    let display_name = Some(def.display_name.clone())
                        .filter(|d| !d.is_empty())
                        .unwrap_or_else(|| key.clone());
                    Some(ApprovalFieldDto {
                        key: key.clone(),
                        display_name,
                        field_type: def.field_type,
                        value,
                    })
                })
                .collect();
            let decisions = row
                .choices
                .iter()
                .map(|c| ApprovalDecisionDto {
                    kind: c.kind,
                    to: c.to.clone(),
                    label: c.label.clone(),
                    comment_required: c.comment == ApprovalComment::Required,
                    allowed: allowed.as_ref().map(|a| a.contains(&c.to)),
                })
                .collect();
            seen.insert(
                row.approval_id,
                ApprovalItem {
                    approval_id: row.approval_id,
                    workflow_id: row.workflow_id,
                    title: row.title.clone(),
                    instructions: row.instructions.clone(),
                    table: ApprovalTableDto {
                        id: table.id,
                        name: table.name.clone(),
                    },
                    record: ApprovalRecordDto {
                        id: rec.id,
                        rev: rec.rev,
                        owner: rec.owner.clone(),
                        fields,
                        link_labels: rec.link_labels.clone(),
                    },
                    from_state: row.from_state.clone(),
                    deadline: row.deadline,
                    created_at: row.created_at,
                    decisions,
                },
            );
        }
    }
    // 台帳の順（新しい順）に戻す。
    Ok(rows
        .iter()
        .filter_map(|r| seen.remove(&r.approval_id))
        .collect())
}

/// 本人が開いた承認の行を読む（見えないテーブルは data が Deny の監査に残して `Err(NotFound / Forbidden)`）。
async fn opened_records(
    state: &AppState,
    ctx: &AuthContext,
    table_id: Uuid,
    ids: &[Uuid],
    trace: Option<&str>,
) -> Result<Option<data::FilteredRecords>, data::DataError> {
    let table = state.data.get_table(ctx, table_id, trace).await?;
    let records = state.data.get_records(ctx, table_id, ids, trace).await?;
    Ok(Some(data::FilteredRecords { table, records }))
}

/// テーブル単位の読取の結果（見えない・権限が無い・確定的な誤りは、そのテーブルの項目を落とす `None`）。
///
/// 一時障害（[`data::DataError::is_transient`]）だけを応答の誤りにする（壊れたテーブル 1 つで受信箱と件数が
/// まるごと 5xx にならないように）。
fn per_table<T>(r: Result<T, data::DataError>, table_id: Uuid) -> Result<Option<T>, ApiError> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.is_transient() => Err(e.into()),
        Err(data::DataError::NotFound | data::DataError::Forbidden) => Ok(None),
        Err(e) => {
            tracing::warn!(error = %e, %table_id, "承認のテーブルを読めないので受信箱に出さない");
            Ok(None)
        }
    }
}

fn cursor_of(created_at: DateTime<Utc>, approval_id: Uuid) -> InboxCursor {
    InboxCursor {
        created_at,
        approval_id,
    }
}

/// 自分宛ての承認待ち（本人か所属ロール宛て・行が本人に見えるものだけ・新しい順に 50 件ずつ）。
///
/// 台帳を読み進めて、見える項目が 50 件たまるか、読み直した台帳の行が上限（500）に達するまで集める
/// （新しい側に見えない承認が並んでも、見える承認に辿り着ける）。上限で打ち切ったときは、50 件に満たなくても
/// 続きの位置を返す。
#[utoipa::path(
    get,
    path = "/approvals",
    params(ApprovalListQuery),
    responses(
        (status = 200, description = "承認待ち", body = ApprovalListResponse),
        (status = 400, description = "cursor が不正"),
        (status = 401, description = "未認証"),
    ),
    security(("session" = [])),
)]
pub async fn list_approvals(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Query(q): Query<ApprovalListQuery>,
) -> Result<Json<ApprovalListResponse>, ApiError> {
    let after = match q.cursor.as_deref() {
        None | Some("") => None,
        Some(c) => Some(
            InboxCursor::decode(c).ok_or_else(|| ApiError::invalid_input("cursor が不正です"))?,
        ),
    };
    let roles = my_roles(&state, &ctx, None).await?;
    let page = PAGE as usize;
    let mut items: Vec<ApprovalItem> = Vec::new();
    let mut scanned = 0usize;
    let mut after = after;
    let next = loop {
        let rows = store(&state)
            .inbox(
                ctx.tenant_id(),
                ctx.org(),
                &ctx.principal().id,
                &roles,
                after,
                PAGE,
            )
            .await
            .map_err(internal)?;
        let n = rows.len();
        scanned += n;
        let last = rows.last().map(|r| cursor_of(r.created_at, r.approval_id));
        items.extend(visible_items(&state, &ctx, rows, Reading::Inbox, trace.as_deref()).await?);
        let exhausted = (n as i64) < PAGE;
        if items.len() > page {
            // 1 ページ分を超えた: 50 件目の後ろから続ける（その後ろの行は次のページで読み直す）。
            items.truncate(page);
            break items.last().map(|i| cursor_of(i.created_at, i.approval_id));
        }
        if items.len() == page || exhausted || scanned >= SCAN_LIMIT {
            // 台帳を読み切ったら続きは無い。そうでなければ、読んだ行の後ろから続ける。
            break if exhausted { None } else { last };
        }
        after = last;
    };
    let next_cursor = next.map(|c| c.encode());
    Ok(Json(ApprovalListResponse { items, next_cursor }))
}

/// 自分宛ての承認待ちの件数（一覧と同じ絞り込み・99 で打ち切り・読み直す台帳の行の上限で打ち切ったら `more`）。
#[utoipa::path(
    get,
    path = "/approvals/count",
    responses(
        (status = 200, description = "件数", body = ApprovalCountResponse),
        (status = 401, description = "未認証"),
    ),
    security(("session" = [])),
)]
pub async fn count_approvals(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
) -> Result<Json<ApprovalCountResponse>, ApiError> {
    Ok(Json(approvals_inbox::count(&state, &ctx).await?))
}

/// 承認待ち 1 件（判断ごとに本人がいま実行できるかを添える）。担当者でない・存在しない・行が見えない・
/// もう決まったは、どれも 404。
#[utoipa::path(
    get,
    path = "/approvals/{id}",
    params(("id" = Uuid, Path, description = "承認 ID")),
    responses(
        (status = 200, description = "承認待ち", body = ApprovalItem),
        (status = 401, description = "未認証"),
        (status = 404, description = "担当でない・存在しない・行が見えない・もう決まった"),
    ),
    security(("session" = [])),
)]
pub async fn get_approval(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<ApprovalItem>, ApiError> {
    let roles = my_roles(&state, &ctx, Some(id)).await?;
    let row = store(&state)
        .get_assigned(ctx.tenant_id(), ctx.org(), &ctx.principal().id, &roles, id)
        .await
        .map_err(internal)?
        .filter(|r| r.decidable)
        .ok_or(ApiError::NotFound)?;
    visible_items(&state, &ctx, vec![row], Reading::Opened, trace.as_deref())
        .await?
        .pop()
        .map(Json)
        .ok_or(ApiError::NotFound)
}

/// 判断する（承認者本人の権限で行を判断の状態へ遷移する・run は遷移を受けた relay が進める）。
#[utoipa::path(
    post,
    path = "/approvals/{id}/decision",
    params(("id" = Uuid, Path, description = "承認 ID")),
    request_body = ApprovalDecisionRequest,
    responses(
        (status = 202, description = "受け付けた（遷移は済み）", body = ApprovalDecisionResponse),
        (status = 400, description = "コメントが必須・長すぎる・この承認に無い判断（code）", body = ApprovalErrorBody),
        (status = 401, description = "未認証"),
        (status = 403, description = "状態の遷移の規則で本人には判断できない（code=not_allowed）・状態の列が読めない", body = ApprovalErrorBody),
        (status = 404, description = "担当でない・存在しない・行が見えない"),
        (status = 409, description = "もう決まった・期限切れ・別の担当者が判断中（code）・rev 不一致（code=rev_conflict）", body = ApprovalErrorBody),
        (status = 422, description = "いまの状態からは判断の行き先へ進めない（code=not_decidable）", body = ApprovalErrorBody),
        (status = 429, description = "判断の受付が混み合っている（しばらくしてから再試行できる）"),
    ),
    security(("session" = [])),
)]
pub async fn decide_approval(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<ApprovalDecisionRequest>,
) -> Result<(StatusCode, Json<ApprovalDecisionResponse>), ApiError> {
    let roles = my_roles(&state, &ctx, Some(id)).await?;
    let row = store(&state)
        .get_assigned(ctx.tenant_id(), ctx.org(), &ctx.principal().id, &roles, id)
        .await
        .map_err(internal)?
        .ok_or(ApiError::NotFound)?;
    let to = row.choice(req.decision).map(|c| c.to.clone());
    let comment = req
        .comment
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty());
    let audit = || DecisionAudit {
        id,
        row: &row,
        decision: req.decision,
        to: to.as_deref(),
        comment: comment.is_some(),
        trace: trace.as_deref(),
    };
    // 行が本人に見えるか・いまの状態を、台帳の状態と選択肢の判定より前に読む（担当でない・存在しない・
    // 行が見えないは同じ 404。見えない行の承認が決まったか・選択肢・コメントの要否を誤りから読ませない）。
    let now = match state
        .fsms
        .allowed_transitions(&ctx, row.table_id, row.record_id, trace.as_deref())
        .await
    {
        Ok(now) => now,
        Err(e) => {
            audit().record(&state, &ctx, Decision::Deny).await;
            return Err(decision_error(e));
        }
    };
    if !row.decidable {
        let expired = row.status == "pending" && row.deadline.is_some_and(|d| d <= Utc::now());
        return Err(if expired || row.status == "expired" {
            expired_error()
        } else {
            already_decided()
        });
    }
    let choice = row.choice(req.decision).cloned().ok_or_else(|| {
        approval_error(
            StatusCode::BAD_REQUEST,
            ApprovalErrorCode::UnknownDecision,
            "この承認ではその判断はできません",
        )
    })?;
    if choice.comment == ApprovalComment::Required && comment.is_none() {
        return Err(approval_error(
            StatusCode::BAD_REQUEST,
            ApprovalErrorCode::CommentRequired,
            "この判断にはコメントが必要です",
        ));
    }
    if comment.is_some_and(|c| c.chars().count() > MAX_COMMENT_CHARS) {
        return Err(approval_error(
            StatusCode::BAD_REQUEST,
            ApprovalErrorCode::CommentTooLong,
            "コメントは 2000 字以内で書いてください",
        ));
    }
    // 行がいまも停止時の状態か（違えば 409）。判断の外で動いた行は、relay が取り下げる前の数秒でも判断させない
    // （rev を合わせれば、ほかの状態から判断の行き先へ進めてしまい、行は判断の状態なのに run は取り下げになる）。
    // 読んだときの rev と expected_rev が違えば遷移が rev の不一致で止まるので、状態だけ見ればよい。
    if now.current.as_deref() != Some(row.from_state.as_str()) {
        audit().record(&state, &ctx, Decision::Deny).await;
        return Err(already_decided());
    }
    // 置く → 遷移 → 失敗なら外す、を台帳の行のロックを持った 1 つの Tx で行う（BUG-47）。要求が取り消されても
    // 途中で止めないよう、別の task で最後まで走らせる（遷移だけが済んでコメントが消える・置いたコメントが
    // 残って次の判断を塞ぐ、を作らない）。
    let work = DecideWork {
        id,
        row,
        to: choice.to,
        decision: req.decision,
        comment: comment.map(str::to_string),
        expected_rev: req.expected_rev,
        trace: trace.0.clone(),
    };
    let task_state = state.clone();
    tokio::spawn(async move { work.run(&task_state, &ctx).await })
        .await
        .map_err(internal)?
}

/// 判断の受付から遷移・確定までの材料（別の task に渡す）。
struct DecideWork {
    id: Uuid,
    row: ApprovalRow,
    /// 判断の行き先。
    to: String,
    decision: ApprovalDecisionKind,
    comment: Option<String>,
    expected_rev: i64,
    trace: Option<String>,
}

impl DecideWork {
    /// 台帳の行をロックし、コメントを置いて承認者本人の権限で遷移する。遷移に成功したら確定し、失敗したら
    /// 取り消す（コメントは遷移に成功した判断のものだけが残る）。
    async fn run(
        self,
        state: &AppState,
        ctx: &AuthContext,
    ) -> Result<(StatusCode, Json<ApprovalDecisionResponse>), ApiError> {
        let mut decision_tx = match store(state)
            .begin_decision(ctx.tenant_id(), self.id)
            .await
            .map_err(internal)?
        {
            DecisionStart::Open(t) => t,
            DecisionStart::Busy => {
                return Err(approval_error(
                    StatusCode::CONFLICT,
                    ApprovalErrorCode::Deciding,
                    "ほかの担当者がこの承認を判断しています。読み直してください",
                ))
            }
            // 受付の同時実行の枠が空かなかった（プールを使い切らないよう、接続を持たずに待って断った）。
            DecisionStart::Crowded => {
                return Err(ApiError::RateLimited(
                    "判断の受付が混み合っています。しばらくしてから再試行してください".into(),
                ))
            }
            DecisionStart::Closed { expired: true } => return Err(expired_error()),
            DecisionStart::Closed { expired: false } => return Err(already_decided()),
        };
        let next_rev = self.expected_rev.saturating_add(1);
        if let Some(body) = self.comment.as_deref() {
            decision_tx
                .put_comment(next_rev, &ctx.principal().id, self.decision, body)
                .await
                .map_err(internal)?;
        }
        let trace = self.trace.as_deref();
        let result = state
            .fsms
            .transition(
                ctx,
                self.row.table_id,
                self.row.record_id,
                &self.to,
                self.expected_rev,
                trace,
            )
            .await;
        let audit = DecisionAudit {
            id: self.id,
            row: &self.row,
            decision: self.decision,
            to: Some(&self.to),
            comment: self.comment.is_some(),
            trace,
        };
        audit
            .record(
                state,
                ctx,
                if result.is_ok() {
                    Decision::Allow
                } else {
                    Decision::Deny
                },
            )
            .await;
        match result {
            Ok(rec) => {
                // 受け付けた判断を台帳に記録して確定する（期限の回収はこれを見て期限切れにしない・BUG-45）。遷移は
                // 済んでいる（取り消せない）ので、確定に失敗しても受け付けたと返す（コメントは失われ得る）。
                let accepted = AcceptedDecision {
                    kind: self.decision,
                    to: self.to.clone(),
                    by: ctx.principal().id.clone(),
                    rev: rec.rev,
                    at: rec.updated_at,
                };
                if let Err(e) = decision_tx.accept(&accepted).await {
                    tracing::error!(error = %e, approval_id = %self.id, "判断の受付の確定に失敗（遷移は済み）");
                }
                Ok((
                    StatusCode::ACCEPTED,
                    Json(ApprovalDecisionResponse {
                        status: "deciding".into(),
                        rev: rec.rev,
                    }),
                ))
            }
            Err(e) => {
                // コメントの取り消しに失敗したことはログに残し、利用者には遷移の誤り（403・409・422）を返す
                // （500 にすると、押した人には何が起きたか分からない・落とした Tx は接続に戻るときに取り消される）。
                if let Err(rb) = decision_tx.rollback().await {
                    tracing::warn!(error = %rb, approval_id = %self.id, "判断に失敗したコメントの取り消しに失敗");
                }
                Err(decision_error(e))
            }
        }
    }
}

/// 期限を過ぎた承認（409）。
fn expired_error() -> ApiError {
    approval_error(
        StatusCode::CONFLICT,
        ApprovalErrorCode::Expired,
        "期限を過ぎたため判断できません",
    )
}

/// もう決まった・取り消された承認（409）。
fn already_decided() -> ApiError {
    approval_error(
        StatusCode::CONFLICT,
        ApprovalErrorCode::Decided,
        "この承認はもう決まっているか、取り消されています",
    )
}

/// 判断の監査（`workflow.approval.decide`）の材料。遷移の結果（Allow / Deny）と、遷移の前に断った判断
/// （行が見えない 404・テーブルの editor が無い・状態の列が読めない 403・判断の外で行が動いた 409）の Deny を
/// 同じ形で残す（data の遷移の監査と trace_id で突き合わせられる）。
struct DecisionAudit<'a> {
    id: Uuid,
    row: &'a ApprovalRow,
    decision: ApprovalDecisionKind,
    /// 判断の行き先（この承認に無い判断なら `None`）。
    to: Option<&'a str>,
    comment: bool,
    trace: Option<&'a str>,
}

impl DecisionAudit<'_> {
    /// 監査を書く（書込の失敗は警告に留める・判断の応答は変えない）。
    async fn record(&self, state: &AppState, ctx: &AuthContext, decision: Decision) {
        let entry = AuditEntry {
            action: AuditAction::WorkflowApprovalDecide,
            object_type: "workflow_approval",
            object_id: &self.id.to_string(),
            decision,
            trace_id: self.trace,
            metadata: json!({
                "decision": self.decision.as_str(), "to": self.to, "run_id": self.row.run_id,
                "table_id": self.row.table_id, "record_id": self.row.record_id,
                "comment": self.comment,
            }),
        };
        if let Err(e) = state.audit.record(ctx, entry).await {
            tracing::warn!(error = %e, "承認の判断の監査に失敗");
        }
    }
}

/// 遷移の誤りを判断の API の応答へ（actor の不許可は 403・定義外は 422・ほかは data の既定）。
fn decision_error(e: data::DataError) -> ApiError {
    match e {
        data::DataError::Forbidden => approval_error(
            StatusCode::FORBIDDEN,
            ApprovalErrorCode::NotAllowed,
            "この判断をする権限がありません（状態の遷移の規則で許されていません）",
        ),
        data::DataError::InvalidInput(ref i) if i.code() == data::InvalidCode::FsmInvalid => {
            approval_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApprovalErrorCode::NotDecidable,
                "いまの状態からは、この判断の行き先へ進めません",
            )
        }
        other => other.into(),
    }
}

/// 承認の受信箱のルート宣言。
pub(crate) fn approval_route_decls() -> Vec<crate::server::RouteDecl> {
    use crate::server::AccessPolicy::Session;
    use axum::routing::{get, post};
    let r = crate::server::RouteDecl::new;
    vec![
        r("/approvals", &["GET"], Session, || get(list_approvals)),
        r("/approvals/count", &["GET"], Session, || {
            get(count_approvals)
        }),
        r("/approvals/{id}", &["GET"], Session, || get(get_approval)),
        r("/approvals/{id}/decision", &["POST"], Session, || {
            post(decide_approval)
        }),
    ]
}

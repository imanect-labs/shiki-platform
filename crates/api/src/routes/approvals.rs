//! 承認の受信箱 API（human.approval・GAP-08・D-24・D-27）。
//!
//! - 一覧・件数・1 件は、台帳（`workflow_approval`）で本人か所属ロール宛ての保留中の承認を引き、**行を承認者
//!   本人の `AuthContext` で data のチョークポイントを通して読み直して**、見える行で今の状態が停止時の状態の
//!   ものだけを出す（二段 authz・見えない行は数えない）。出す列は担当者に読める列だけ（マスク後）。
//! - 判断は、承認者本人の `AuthContext` で `FsmStore::transition(expected_rev)` を呼ぶ（ワークフローの権限で
//!   遷移しない）。actor 述語・行の可視性・テーブルの editor・rev はすべて data が判定する（判定の正は 1 か所・
//!   テーブル画面から遷移しても同じ承認が決まる）。run を進めるのは、遷移の outbox を受けた relay。
//! - 担当者でない・存在しない・行が見えないは、どれも同じ 404（存在の手がかりを出さない）。
//!
//! 判断に添えるコメントは、遷移の後の rev の鍵で遷移の前に置き、遷移に失敗したら外す（遷移と同時に起床しても
//! 拾える・失敗した判断のコメントを残さない）。

use std::collections::BTreeMap;

use authz::{AuthContext, ObjectType, Relation};
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
use workflow_engine::{ApprovalRow, InboxCursor};

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// 1 ページの件数。
const PAGE: i64 = 50;
/// 件数の打ち切り（バッジは 99+）。
const COUNT_CAP: usize = 99;
/// 件数を数えるときに読み直す台帳の行の上限（見えない行ばかりでも際限なく読まない）。
const COUNT_SCAN: usize = 500;
/// 所属ロールの上限（受信箱の照会の材料・超えた分は使わない）。
const MAX_ROLES: usize = 1000;
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
    #[schema(value_type = Object)]
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
    pub items: Vec<ApprovalItem>,
    /// 次のページの位置（無ければ null）。
    pub next_cursor: Option<String>,
}

/// 受信箱の件数（99 で打ち切り）。
#[derive(Debug, Serialize, ToSchema)]
pub struct ApprovalCountResponse {
    pub count: u32,
    /// 99 件を超えている（バッジは 99+）。
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

/// 本人の所属ロール（ローカル id・上限つき）。
async fn my_roles(state: &AppState, ctx: &AuthContext) -> Result<Vec<String>, ApiError> {
    let raw = state
        .authz
        .list_objects(&ctx.subject(), Relation::Member, ObjectType::Role)
        .await
        .map_err(|e| ApiError::ServiceUnavailable(format!("authz: {e}")))?;
    let ns = ctx.ns();
    Ok(raw
        .iter()
        .filter_map(|o| o.split_once(':').map(|(_, id)| id))
        .filter_map(|id| ns.strip_object_id(id))
        .take(MAX_ROLES)
        .map(str::to_string)
        .collect())
}

/// 台帳の行を、承認者本人の見え方で読み直した受信箱の項目にする（見えない・状態が動いたものは落とす）。
///
/// `with_allowed` なら判断ごとに本人が実行できるか（`FsmStore::allowed_transitions`）を添える。
async fn visible_items(
    state: &AppState,
    ctx: &AuthContext,
    rows: Vec<ApprovalRow>,
    with_allowed: bool,
    trace: Option<&str>,
) -> Result<Vec<ApprovalItem>, ApiError> {
    let mut tables: Vec<Uuid> = Vec::new();
    for r in &rows {
        if !tables.contains(&r.table_id) {
            tables.push(r.table_id);
        }
    }
    let mut seen: BTreeMap<Uuid, ApprovalItem> = BTreeMap::new();
    for table_id in tables {
        let table = match state.data.get_table(ctx, table_id, trace).await {
            Ok(t) => t,
            Err(data::DataError::NotFound | data::DataError::Forbidden) => continue,
            Err(e) => return Err(e.into()),
        };
        let mine: Vec<&ApprovalRow> = rows.iter().filter(|r| r.table_id == table_id).collect();
        let ids: Vec<Uuid> = mine.iter().map(|r| r.record_id).collect();
        let records = match state.data.get_records(ctx, table_id, &ids, trace).await {
            Ok(v) => v,
            Err(data::DataError::NotFound | data::DataError::Forbidden) => continue,
            Err(e) => return Err(e.into()),
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
                    Err(
                        data::DataError::Forbidden
                        | data::DataError::NotFound
                        | data::DataError::FieldUnavailable,
                    ) => Some(Vec::new()),
                    Err(e) => return Err(e.into()),
                }
            } else {
                None
            };
            let fields = row
                .show_fields
                .iter()
                .filter_map(|key| {
                    let value = rec.data.get(key)?.clone();
                    let display_name = table
                        .schema
                        .field(key)
                        .map(|f| f.display_name.clone())
                        .filter(|d| !d.is_empty())
                        .unwrap_or_else(|| key.clone());
                    Some(ApprovalFieldDto {
                        key: key.clone(),
                        display_name,
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

/// 自分宛ての承認待ち（本人か所属ロール宛て・行が本人に見えるものだけ・新しい順に 50 件ずつ）。
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
    let roles = my_roles(&state, &ctx).await?;
    let rows = store(&state)
        .inbox(
            &ctx.tenant_id,
            &ctx.org,
            &ctx.principal.id,
            &roles,
            after,
            PAGE,
        )
        .await
        .map_err(internal)?;
    let next_cursor = (rows.len() as i64 == PAGE)
        .then(|| rows.last())
        .flatten()
        .map(|r| {
            InboxCursor {
                created_at: r.created_at,
                approval_id: r.approval_id,
            }
            .encode()
        });
    let items = visible_items(&state, &ctx, rows, false, trace.as_deref()).await?;
    Ok(Json(ApprovalListResponse { items, next_cursor }))
}

/// 自分宛ての承認待ちの件数（一覧と同じ絞り込み・99 で打ち切り）。
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
    trace: TraceIdExt,
) -> Result<Json<ApprovalCountResponse>, ApiError> {
    let roles = my_roles(&state, &ctx).await?;
    let mut count = 0usize;
    let mut scanned = 0usize;
    let mut after: Option<InboxCursor> = None;
    loop {
        let rows = store(&state)
            .inbox(
                &ctx.tenant_id,
                &ctx.org,
                &ctx.principal.id,
                &roles,
                after,
                PAGE * 2,
            )
            .await
            .map_err(internal)?;
        let n = rows.len();
        scanned += n;
        after = rows.last().map(|r| InboxCursor {
            created_at: r.created_at,
            approval_id: r.approval_id,
        });
        count += visible_items(&state, &ctx, rows, false, trace.as_deref())
            .await?
            .len();
        if count > COUNT_CAP || (n as i64) < PAGE * 2 || scanned >= COUNT_SCAN {
            break;
        }
    }
    Ok(Json(ApprovalCountResponse {
        count: u32::try_from(count.min(COUNT_CAP)).unwrap_or(99),
        more: count > COUNT_CAP,
    }))
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
    let roles = my_roles(&state, &ctx).await?;
    let row = store(&state)
        .get_assigned(&ctx.tenant_id, &ctx.org, &ctx.principal.id, &roles, id)
        .await
        .map_err(internal)?
        .filter(|r| r.decidable)
        .ok_or(ApiError::NotFound)?;
    visible_items(&state, &ctx, vec![row], true, trace.as_deref())
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
    let roles = my_roles(&state, &ctx).await?;
    let row = store(&state)
        .get_assigned(&ctx.tenant_id, &ctx.org, &ctx.principal.id, &roles, id)
        .await
        .map_err(internal)?
        .ok_or(ApiError::NotFound)?;
    if !row.decidable {
        let expired = row.status == "pending" && row.deadline.is_some_and(|d| d <= Utc::now());
        return Err(if expired || row.status == "expired" {
            approval_error(
                StatusCode::CONFLICT,
                ApprovalErrorCode::Expired,
                "期限を過ぎたため判断できません",
            )
        } else {
            approval_error(
                StatusCode::CONFLICT,
                ApprovalErrorCode::Decided,
                "この承認はもう決まっているか、取り消されています",
            )
        });
    }
    let choice = row.choice(req.decision).cloned().ok_or_else(|| {
        approval_error(
            StatusCode::BAD_REQUEST,
            ApprovalErrorCode::UnknownDecision,
            "この承認ではその判断はできません",
        )
    })?;
    let comment = req
        .comment
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty());
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
    let next_rev = req.expected_rev.saturating_add(1);
    if let Some(body) = comment {
        let put = store(&state)
            .put_comment(
                &ctx.tenant_id,
                id,
                next_rev,
                &ctx.principal.id,
                req.decision,
                body,
            )
            .await
            .map_err(internal)?;
        if !put {
            return Err(approval_error(
                StatusCode::CONFLICT,
                ApprovalErrorCode::Deciding,
                "ほかの担当者がこの承認を判断しています。読み直してください",
            ));
        }
    }
    let result = state
        .fsms
        .transition(
            &ctx,
            row.table_id,
            row.record_id,
            &choice.to,
            req.expected_rev,
            trace.as_deref(),
        )
        .await;
    let decision = if result.is_ok() {
        Decision::Allow
    } else {
        Decision::Deny
    };
    let entry = AuditEntry {
        action: AuditAction::WorkflowApprovalDecide,
        object_type: "workflow_approval",
        object_id: &id.to_string(),
        decision,
        trace_id: trace.as_deref(),
        metadata: json!({
            "decision": req.decision.as_str(), "to": choice.to, "run_id": row.run_id,
            "table_id": row.table_id, "record_id": row.record_id, "comment": comment.is_some(),
        }),
    };
    if let Err(e) = state.audit.record(&ctx, entry).await {
        tracing::warn!(error = %e, "承認の判断の監査に失敗");
    }
    match result {
        Ok(rec) => Ok((
            StatusCode::ACCEPTED,
            Json(ApprovalDecisionResponse {
                status: "deciding".into(),
                rev: rec.rev,
            }),
        )),
        Err(e) => {
            if comment.is_some() {
                store(&state)
                    .drop_comment(&ctx.tenant_id, id, next_rev, &ctx.principal.id)
                    .await
                    .map_err(internal)?;
            }
            Err(decision_error(e))
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

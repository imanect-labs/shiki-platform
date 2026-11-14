//! 社内文書の検索への取り込み直し（D-126・GAP-51）。
//!
//! - `POST /files/{id}/reindex`: ファイルを編集できる人が、1 件を取り込み直す（ドライブのメニューの「検索に
//!   取り込み直す」）。認可は StorageService（`authorize_reindex`・`editor@file`）、積むかの判定と積む操作は
//!   社内文書の検索（`rag::SearchService::request_reindex`）が持つ。
//! - `GET`・`POST /org-admin/search/backfill`: テナントの管理者が、取り込みの記録が無いファイルをまとめて取り込む
//!   （管理コンソールの「社内文書の検索」）。積むのは取り込みのリーダーが少しずつ行う。
//!
//! 断るときは `code` と、画面にそのまま出せる日本語の `message` を返す（[`SearchIngestRefusedBody`]）。同じ内容の
//! 版が索引にある・取り込み中のときは断らずに、そのことを答える（冪等・積まない）。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::ApiError;
use crate::extract::{AuthContextExt, TraceIdExt};
use crate::server::{AccessPolicy, RouteDecl};
use crate::state::AppState;

/// 社内文書の検索が使えない環境の文言。
const SEARCH_DISABLED: &str = "この環境では社内文書の検索が使えません。管理者に連絡してください。";
/// 見えない・無いファイル（存在を秘匿する決まった文）。
const NOT_FOUND: &str = "ファイルが見つかりません。削除されたか、見る権限がありません。";
/// 見えるが編集できない人。
const FORBIDDEN: &str = "このファイルを取り込み直せるのは、ファイルを編集できる人だけです。編集できる人に頼んでください。";
/// 取り込めない種類。
const UNSUPPORTED: &str = "このファイルの種類は社内文書の検索に取り込めません。取り込めるのは PDF、Word（.docx）、Excel（.xlsx）、PowerPoint（.pptx）、テキスト、Markdown、CSV、HTML のファイルです。";
/// 取り込まない場所。
const EXCLUDED: &str =
    "AI エージェントの作業用のフォルダーにあるファイルは、社内文書の検索に取り込みません。";
/// 取り込んだが本文が無かった。
const EMPTY: &str = "このファイルは取り込み済みですが、本文を取り出せませんでした。文字の無い文書や、文字が画像になっている文書は検索できません。";

pub(crate) fn search_ingest_route_decls() -> Vec<RouteDecl> {
    vec![
        RouteDecl::new(
            "/files/{id}/reindex",
            &["POST"],
            AccessPolicy::Session,
            || post(reindex_file),
        ),
        RouteDecl::new(
            "/org-admin/search/backfill",
            &["GET", "POST"],
            AccessPolicy::OrgAdmin,
            || get(get_backfill).post(start_backfill),
        ),
    ]
}

/// 取り込み直しの答え。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReindexStatus {
    /// 取り込みの待ち行列に積んだ（202）。終わるとファイルを読む・社内文書の検索で読める。
    Queued,
    /// 取り込みを待っている・取り込み中（積まない・200）。
    InProgress,
    /// いまの内容がすでに取り込まれている（積まない・200）。
    Indexed,
}

/// `POST /files/{id}/reindex` の応答。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReindexResponse {
    pub status: ReindexStatus,
    /// 取り込みに積んだ内容のバージョン（`queued` のときだけ）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
}

/// 取り込み直しを断った理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SearchIngestRefusedCode {
    /// この環境では社内文書の検索が使えない（503）。
    SearchDisabled,
    /// ファイルが無い・見えない（404・存在を秘匿する）。
    NotFound,
    /// ファイルを見られるが、編集できない（403）。
    Forbidden,
    /// 取り込めない種類・大きすぎる・取り込まない場所・本文が無かった（400・取り込み直しても読めない）。
    NotIndexable,
}

/// 取り込み直しを断ったときの本文（`message` は画面にそのまま出せる日本語）。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SearchIngestRefusedBody {
    pub status: u16,
    pub title: String,
    pub code: SearchIngestRefusedCode,
    pub message: String,
}

/// 断るか、ほかの誤り（[`ApiError`]）。
pub enum Refusal {
    Refused(SearchIngestRefusedCode, String),
    Api(ApiError),
}

impl From<ApiError> for Refusal {
    fn from(e: ApiError) -> Self {
        Refusal::Api(e)
    }
}

impl From<rag::RagError> for Refusal {
    fn from(e: rag::RagError) -> Self {
        Refusal::Api(e.into())
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        let (code, message) = match self {
            Refusal::Refused(code, message) => (code, message),
            Refusal::Api(e) => return e.into_response(),
        };
        let status = match code {
            SearchIngestRefusedCode::SearchDisabled => StatusCode::SERVICE_UNAVAILABLE,
            SearchIngestRefusedCode::NotFound => StatusCode::NOT_FOUND,
            SearchIngestRefusedCode::Forbidden => StatusCode::FORBIDDEN,
            SearchIngestRefusedCode::NotIndexable => StatusCode::BAD_REQUEST,
        };
        let body = SearchIngestRefusedBody {
            status: status.as_u16(),
            title: status.canonical_reason().unwrap_or("Error").to_string(),
            code,
            message,
        };
        (status, Json(body)).into_response()
    }
}

fn refused(code: SearchIngestRefusedCode, message: &str) -> Refusal {
    Refusal::Refused(code, message.to_string())
}

/// 取り込み直しても読めない理由の文言。
fn not_indexable_message(why: rag::NotIndexable) -> String {
    match why {
        rag::NotIndexable::Unsupported => UNSUPPORTED.to_string(),
        rag::NotIndexable::TooLarge { limit_bytes } => format!(
            "ファイルが大きすぎるため、社内文書の検索に取り込めません（上限 {} MB）。",
            limit_bytes / 1024 / 1024
        ),
        rag::NotIndexable::Excluded => EXCLUDED.to_string(),
        rag::NotIndexable::Empty => EMPTY.to_string(),
    }
}

/// ファイルを社内文書の検索に取り込み直す（ファイルを編集できる人）。
#[utoipa::path(
    post,
    path = "/files/{id}/reindex",
    params(("id" = Uuid, Path, description = "ファイル ID")),
    responses(
        (status = 202, description = "取り込みの待ち行列に積んだ", body = ReindexResponse),
        (status = 200, description = "取り込み中・取り込み済み（積まない）", body = ReindexResponse),
        (status = 400, description = "取り込み直しても読めない（`not_indexable`）", body = SearchIngestRefusedBody),
        (status = 401, description = "未認証"),
        (status = 403, description = "編集できない（`forbidden`）", body = SearchIngestRefusedBody),
        (status = 404, description = "ファイルが無い・見えない（`not_found`）", body = SearchIngestRefusedBody),
        (status = 503, description = "社内文書の検索が使えない環境（`search_disabled`）", body = SearchIngestRefusedBody),
    ),
    security(("session" = [])),
)]
pub async fn reindex_file(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<ReindexResponse>), Refusal> {
    let Some(search) = state.search.as_ref() else {
        return Err(refused(
            SearchIngestRefusedCode::SearchDisabled,
            SEARCH_DISABLED,
        ));
    };
    let node = match state
        .storage
        .authorize_reindex(&ctx, id, trace.as_deref())
        .await
    {
        Ok(node) => node,
        Err(storage::StorageError::NotFound) => {
            return Err(refused(SearchIngestRefusedCode::NotFound, NOT_FOUND))
        }
        Err(storage::StorageError::Forbidden) => {
            return Err(refused(SearchIngestRefusedCode::Forbidden, FORBIDDEN))
        }
        Err(other) => return Err(ApiError::from(other).into()),
    };
    let answer = |status| {
        (
            StatusCode::OK,
            Json(ReindexResponse {
                status,
                version: None,
            }),
        )
    };
    Ok(
        match search
            .request_reindex(&ctx, &node, trace.as_deref())
            .await?
        {
            rag::ReindexOutcome::Queued { version } => (
                StatusCode::ACCEPTED,
                Json(ReindexResponse {
                    status: ReindexStatus::Queued,
                    version: Some(version),
                }),
            ),
            rag::ReindexOutcome::InProgress => answer(ReindexStatus::InProgress),
            rag::ReindexOutcome::AlreadyIndexed => answer(ReindexStatus::Indexed),
            rag::ReindexOutcome::NotIndexable(why) => {
                return Err(Refusal::Refused(
                    SearchIngestRefusedCode::NotIndexable,
                    not_indexable_message(why),
                ))
            }
        },
    )
}

/// `POST /org-admin/search/backfill` の応答。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StartBackfillResponse {
    /// 新しく始めたか（`false` は、同じ組織で動いているものを返した）。
    pub started: bool,
    pub status: rag::SearchIngestStatus,
}

/// 社内文書の検索の取り込みの様子（待っている件数と、まとめての取り込みの進み具合）。
#[utoipa::path(
    get,
    path = "/org-admin/search/backfill",
    responses(
        (status = 200, description = "取り込みの様子（使えない環境では `enabled: false`）", body = rag::SearchIngestStatus),
        (status = 403, description = "組織管理者ではない"),
    ),
    security(("session" = [])),
)]
pub async fn get_backfill(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
) -> Result<Json<rag::SearchIngestStatus>, ApiError> {
    let Some(search) = state.search.as_ref() else {
        return Ok(Json(rag::SearchIngestStatus::disabled()));
    };
    Ok(Json(search.ingest_status(&ctx).await?))
}

/// 取り込みの記録が無いファイルを、まとめて取り込み始める（組織管理者・監査に残す）。
#[utoipa::path(
    post,
    path = "/org-admin/search/backfill",
    responses(
        (status = 202, description = "始めた・動いているものを返した", body = StartBackfillResponse),
        (status = 403, description = "組織管理者ではない"),
        (status = 503, description = "社内文書の検索が使えない環境（`search_disabled`）", body = SearchIngestRefusedBody),
    ),
    security(("session" = [])),
)]
pub async fn start_backfill(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
) -> Result<(StatusCode, Json<StartBackfillResponse>), Refusal> {
    let Some(search) = state.search.as_ref() else {
        return Err(refused(
            SearchIngestRefusedCode::SearchDisabled,
            SEARCH_DISABLED,
        ));
    };
    let (_, started) = search.start_backfill(&ctx, trace.as_deref()).await?;
    let status = search.ingest_status(&ctx).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(StartBackfillResponse { started, status }),
    ))
}

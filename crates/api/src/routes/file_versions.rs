//! ファイルのバージョン履歴 API（Task 1.7 / 11.8）。
//!
//! 一覧・特定版ダウンロード・非破壊復元に加え、AI 提案バージョン（`is_proposal`）の
//! 「採用」（通常の新バージョンへ昇格・このとき初めて RAG 再索引が流れる）を提供する。
//! DTO（[`FileVersionResponse`]）もここが正本（`routes::files` から re-export しない）。

use axum::{
    extract::{Path, State},
    Json,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use storage::FileVersion;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

use super::files::{DownloadUrlResponse, NodeResponse};

/// ファイルの内容版 1 件（履歴一覧の要素）。
#[derive(Debug, Serialize, ToSchema)]
pub struct FileVersionResponse {
    pub version: i64,
    pub blob_sha256: String,
    pub size_bytes: i64,
    pub content_type: String,
    /// この版を作成したユーザー id。
    pub author: String,
    /// 作成者の表示名（ディレクトリ解決済み・Task 11P.10）。未解決時は `null`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author_name: Option<String>,
    pub created_at: DateTime<Utc>,
    /// AI 提案バージョンか（Task 11.8）。true は current 未反映・editor の採用待ち。
    pub is_proposal: bool,
    /// 提案の実行主体（提案バージョンのみ）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposed_by: Option<String>,
}

impl From<FileVersion> for FileVersionResponse {
    fn from(v: FileVersion) -> Self {
        FileVersionResponse {
            version: v.version,
            blob_sha256: v.blob_sha256,
            size_bytes: v.size_bytes,
            content_type: v.content_type,
            author: v.author,
            author_name: None,
            created_at: v.created_at,
            is_proposal: v.is_proposal,
            proposed_by: v.proposed_by,
        }
    }
}

/// 版履歴の 1 ページ（新しい順・keyset ページング）。
#[derive(Debug, Serialize, ToSchema)]
pub struct FileVersionsResponse {
    pub items: Vec<FileVersionResponse>,
    /// 続きがあれば次回 `cursor` に渡す値（末尾なら `null`）。
    pub next_cursor: Option<String>,
}

/// 版履歴を新しい順に 1 ページ返す（Task 1.7・keyset ページング）。
#[utoipa::path(
    get,
    path = "/files/{id}/versions",
    params(
        ("id" = Uuid, Path, description = "ファイル ID"),
        crate::routes::folders::PageQuery,
    ),
    responses(
        (status = 200, description = "版履歴（新しい順・1 ページ）", body = FileVersionsResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "ファイルが無い"),
    ),
    security(("session" = [])),
)]
pub async fn list_versions(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    axum::extract::Query(q): axum::extract::Query<crate::routes::folders::PageQuery>,
) -> Result<Json<FileVersionsResponse>, ApiError> {
    let (versions, next_cursor) = state
        .storage
        .list_versions(
            &ctx,
            id,
            q.cursor.as_deref(),
            q.limit.unwrap_or(50),
            trace.as_deref(),
        )
        .await?;
    let mut items: Vec<FileVersionResponse> = versions.into_iter().map(Into::into).collect();
    // 版 author を表示名で補完（ディレクトリ・テナントスコープ。未登録 subject は null）。
    let ids: Vec<String> = items.iter().map(|v| v.author.clone()).collect();
    if let Ok(names) = state.directory.resolve_display_names(&ctx, &ids).await {
        for v in &mut items {
            v.author_name = names.get(&v.author).cloned();
        }
    }
    Ok(Json(FileVersionsResponse { items, next_cursor }))
}

/// 特定版の presigned ダウンロード URL を発行する（Task 1.7）。
#[utoipa::path(
    get,
    path = "/files/{id}/versions/{version}/download-url",
    params(
        ("id" = Uuid, Path, description = "ファイル ID"),
        ("version" = i64, Path, description = "版番号"),
    ),
    responses(
        (status = 200, description = "ダウンロード URL", body = DownloadUrlResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "ファイル/版が無い"),
    ),
    security(("session" = [])),
)]
pub async fn version_download_url(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, version)): Path<(Uuid, i64)>,
) -> Result<Json<DownloadUrlResponse>, ApiError> {
    let ticket = state
        .storage
        .issue_version_download_url(&ctx, id, version, trace.as_deref())
        .await?;
    Ok(Json(DownloadUrlResponse {
        url: ticket.url,
        expires_in_secs: ticket.expires_in_secs,
    }))
}

/// 過去版を新しい版として復元する（Task 1.7・履歴を壊さない）。
#[utoipa::path(
    post,
    path = "/files/{id}/versions/{version}/restore",
    params(
        ("id" = Uuid, Path, description = "ファイル ID"),
        ("version" = i64, Path, description = "復元元の版番号"),
    ),
    responses(
        (status = 200, description = "復元後のファイル（新版）", body = NodeResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "ファイル/版が無い"),
    ),
    security(("session" = [])),
)]
pub async fn restore_version(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, version)): Path<(Uuid, i64)>,
) -> Result<Json<NodeResponse>, ApiError> {
    let node = state
        .storage
        .restore_version(&ctx, id, version, trace.as_deref())
        .await?;
    Ok(Json(node.into()))
}

/// AI 提案バージョンを採用し、通常の新バージョンへ昇格する（Task 11.8・editor 権限）。
///
/// 採用時に初めて書込イベント（RAG 再索引）が流れる。対象が提案バージョンでなければ 404。
#[utoipa::path(
    post,
    path = "/files/{id}/versions/{version}/adopt",
    tag = "files",
    params(
        ("id" = Uuid, Path, description = "ファイル ID"),
        ("version" = i64, Path, description = "採用する提案バージョン番号"),
    ),
    responses(
        (status = 200, description = "採用後のファイル（新版）", body = NodeResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "ファイル/提案バージョンが無い"),
    ),
    security(("session" = [])),
)]
pub async fn adopt_version(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, version)): Path<(Uuid, i64)>,
) -> Result<Json<NodeResponse>, ApiError> {
    let node = state
        .storage
        .adopt_proposal_version(&ctx, id, version, trace.as_deref())
        .await?;
    Ok(Json(node.into()))
}

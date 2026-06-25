//! ファイル CRUD API（Task 1.4）。
//!
//! バイトは presigned URL でクライアント↔MinIO 直転送し、本ハンドラ群はメタ・認可・
//! 監査・content-addressing を担う StorageService 経由でのみ動作する（PIT-6・単一チョークポイント）。
//! 二相アップロード: `POST /files`（declare）→ `upload_url` へ直 PUT → `POST /files/{id}/finalize`。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use storage::{FileVersion, Node};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// ストレージノード（ファイル**または**フォルダ）のメタ表現（API レスポンス）。
///
/// `kind` で種別を判別し、フォルダでは `size_bytes`/`content_type` が `null` になる。
/// ファイル CRUD・フォルダ操作・共有された一覧（file/folder 混在）で共通に使う。
#[derive(Debug, Serialize, ToSchema)]
pub struct NodeResponse {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub parent_id: Option<Uuid>,
    pub size_bytes: Option<i64>,
    pub content_type: Option<String>,
    pub version: i64,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<Node> for NodeResponse {
    fn from(n: Node) -> Self {
        NodeResponse {
            id: n.id,
            name: n.name,
            kind: n.kind.as_str().to_string(),
            parent_id: n.parent_id,
            size_bytes: n.size_bytes,
            content_type: n.content_type,
            version: n.version,
            created_by: n.created_by,
            created_at: n.created_at,
            updated_at: n.updated_at,
        }
    }
}

/// アップロード宣言リクエスト（declare）。
///
/// `target_node_id` を指定すると**既存ファイルの内容更新（新版アップロード）**になり、
/// `parent_id`/`name` は無視して既存ノードのものを引き継ぐ。未指定なら**新規ファイル作成**で
/// `name` が必須。
#[derive(Debug, Deserialize, ToSchema)]
pub struct UploadRequest {
    /// 配置先フォルダ。未指定は org ルート直下（新規作成時のみ）。
    pub parent_id: Option<Uuid>,
    /// ファイル名（新規作成時は必須。内容更新時は無視される）。
    pub name: Option<String>,
    pub content_type: String,
    /// 内容のバイト数（クライアント申告。finalize で実体と照合）。
    pub size: i64,
    /// 内容の sha256（hex 小文字 64 桁。finalize で server-side 再ハッシュと照合）。
    pub sha256: String,
    /// 内容更新の対象ファイル ID。指定時は既存ファイルの新版アップロードになる。
    pub target_node_id: Option<Uuid>,
}

/// アップロード宣言レスポンス（declare）。
///
/// クライアントは `upload_url` へバイトを直接 PUT し、`upload_id` で finalize する。
/// 重複排除は finalize 時（＝所持証明の後）に行う。
#[derive(Debug, Serialize, ToSchema)]
pub struct UploadTicketResponse {
    pub upload_id: Uuid,
    /// presigned PUT URL。クライアントはここへ直接バイトを PUT する。
    pub upload_url: String,
}

/// リネーム/移動リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateFileRequest {
    /// 新しい名前（指定時にリネーム）。
    pub name: Option<String>,
    /// 移動先フォルダ。`null` 明示は「ルートへ移動」、省略は「移動しない」。
    #[serde(default, deserialize_with = "double_option")]
    pub parent_id: Option<Option<Uuid>>,
}

/// ダウンロード URL レスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct DownloadUrlResponse {
    pub url: String,
    pub expires_in_secs: u64,
}

/// ファイルの内容版 1 件（履歴一覧の要素）。
#[derive(Debug, Serialize, ToSchema)]
pub struct FileVersionResponse {
    pub version: i64,
    pub blob_sha256: String,
    pub size_bytes: i64,
    pub content_type: String,
    /// この版を作成したユーザー id。
    pub author: String,
    pub created_at: DateTime<Utc>,
}

impl From<FileVersion> for FileVersionResponse {
    fn from(v: FileVersion) -> Self {
        FileVersionResponse {
            version: v.version,
            blob_sha256: v.blob_sha256,
            size_bytes: v.size_bytes,
            content_type: v.content_type,
            author: v.author,
            created_at: v.created_at,
        }
    }
}

/// `null`（ルートへ移動）と省略（移動しない）を区別するための二重 Option デシリアライザ。
pub fn double_option<'de, D>(deserializer: D) -> Result<Option<Option<Uuid>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde::Deserialize::deserialize(deserializer).map(Some)
}

/// declare: メタを申告し、staging への presigned PUT URL を得る。
#[utoipa::path(
    post,
    path = "/files",
    request_body = UploadRequest,
    responses(
        (status = 200, description = "アップロードチケット", body = UploadTicketResponse),
        (status = 400, description = "不正なリクエスト"),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "親フォルダが無い"),
    ),
    security(("session" = [])),
)]
pub async fn begin_upload(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<UploadRequest>,
) -> Result<Json<UploadTicketResponse>, ApiError> {
    let ticket = state
        .storage
        .begin_upload(
            &ctx,
            req.parent_id,
            req.name.as_deref().unwrap_or(""),
            &req.content_type,
            &req.sha256,
            req.size,
            req.target_node_id,
            trace.as_deref(),
        )
        .await?;
    Ok(Json(UploadTicketResponse {
        upload_id: ticket.upload_id,
        upload_url: ticket.upload_url,
    }))
}

/// finalize: 直 PUT 後に内容を検証し、ノード化する。
#[utoipa::path(
    post,
    path = "/files/{upload_id}/finalize",
    params(("upload_id" = Uuid, Path, description = "declare で得た upload_id")),
    responses(
        (status = 200, description = "確定したファイル", body = NodeResponse),
        (status = 400, description = "整合性エラー（ハッシュ不一致・未アップロード）"),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "upload_id が無い"),
        (status = 409, description = "同名衝突"),
    ),
    security(("session" = [])),
)]
pub async fn finalize_upload(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(upload_id): Path<Uuid>,
) -> Result<Json<NodeResponse>, ApiError> {
    let node = state
        .storage
        .finalize_upload(&ctx, upload_id, trace.as_deref())
        .await?;
    Ok(Json(node.into()))
}

/// presigned GET URL を発行する（短 TTL）。
#[utoipa::path(
    get,
    path = "/files/{id}/download-url",
    params(("id" = Uuid, Path, description = "ファイル ID")),
    responses(
        (status = 200, description = "ダウンロード URL", body = DownloadUrlResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "ファイルが無い"),
    ),
    security(("session" = [])),
)]
pub async fn download_url(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<DownloadUrlResponse>, ApiError> {
    let ticket = state
        .storage
        .issue_download_url(&ctx, id, trace.as_deref())
        .await?;
    Ok(Json(DownloadUrlResponse {
        url: ticket.url,
        expires_in_secs: ticket.expires_in_secs,
    }))
}

/// ファイルメタを取得する。
#[utoipa::path(
    get,
    path = "/files/{id}",
    params(("id" = Uuid, Path, description = "ファイル ID")),
    responses(
        (status = 200, description = "ファイルメタ", body = NodeResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "ファイルが無い"),
    ),
    security(("session" = [])),
)]
pub async fn get_file(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<NodeResponse>, ApiError> {
    let node = state
        .storage
        .get_metadata(&ctx, id, trace.as_deref())
        .await?;
    Ok(Json(node.into()))
}

/// リネーム・移動（指定フィールドのみ反映）。
#[utoipa::path(
    patch,
    path = "/files/{id}",
    params(("id" = Uuid, Path, description = "ファイル ID")),
    request_body = UpdateFileRequest,
    responses(
        (status = 200, description = "更新後のファイル", body = NodeResponse),
        (status = 400, description = "不正なリクエスト"),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "ファイルが無い"),
        (status = 409, description = "同名衝突"),
    ),
    security(("session" = [])),
)]
pub async fn update_file(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateFileRequest>,
) -> Result<Json<NodeResponse>, ApiError> {
    // move と rename を 1 トランザクションで原子的に適用する（部分適用を防ぐ）。
    let node = state
        .storage
        .update_file(
            &ctx,
            id,
            req.name.as_deref(),
            req.parent_id,
            trace.as_deref(),
        )
        .await?;
    Ok(Json(node.into()))
}

/// 論理削除（ゴミ箱へ）。
#[utoipa::path(
    delete,
    path = "/files/{id}",
    params(("id" = Uuid, Path, description = "ファイル ID")),
    responses(
        (status = 204, description = "削除済み"),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "ファイルが無い"),
    ),
    security(("session" = [])),
)]
pub async fn delete_file(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    state
        .storage
        .soft_delete_file(&ctx, id, trace.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// ゴミ箱からの復元。
#[utoipa::path(
    post,
    path = "/files/{id}/restore",
    params(("id" = Uuid, Path, description = "ファイル ID")),
    responses(
        (status = 200, description = "復元したファイル", body = NodeResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "ファイルが無い"),
        (status = 409, description = "同名衝突"),
    ),
    security(("session" = [])),
)]
pub async fn restore_file(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<NodeResponse>, ApiError> {
    let node = state
        .storage
        .restore_file(&ctx, id, trace.as_deref())
        .await?;
    Ok(Json(node.into()))
}

/// 版履歴を新しい順に返す（Task 1.7）。
#[utoipa::path(
    get,
    path = "/files/{id}/versions",
    params(("id" = Uuid, Path, description = "ファイル ID")),
    responses(
        (status = 200, description = "版履歴（新しい順）", body = [FileVersionResponse]),
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
) -> Result<Json<Vec<FileVersionResponse>>, ApiError> {
    let versions = state
        .storage
        .list_versions(&ctx, id, trace.as_deref())
        .await?;
    Ok(Json(versions.into_iter().map(Into::into).collect()))
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

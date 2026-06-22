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
use storage::{model::UploadOutcome, Node};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// ファイル/フォルダのメタ表現（API レスポンス）。
#[derive(Debug, Serialize, ToSchema)]
pub struct FileResponse {
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

impl From<Node> for FileResponse {
    fn from(n: Node) -> Self {
        FileResponse {
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
#[derive(Debug, Deserialize, ToSchema)]
pub struct UploadRequest {
    /// 配置先フォルダ。未指定は org ルート直下。
    pub parent_id: Option<Uuid>,
    pub name: String,
    pub content_type: String,
    /// 内容のバイト数（クライアント申告。finalize で実体と照合）。
    pub size: i64,
    /// 内容の sha256（hex 小文字 64 桁。finalize で server-side 再ハッシュと照合）。
    pub sha256: String,
}

/// アップロード宣言レスポンス（declare）。
#[derive(Debug, Serialize, ToSchema)]
pub struct UploadTicket {
    /// `true` なら既存 blob と重複しアップロード不要（`file` にノードが入る）。
    pub upload_required: bool,
    /// dedup 済みの場合のノード（`upload_required=false`）。
    pub file: Option<FileResponse>,
    /// アップロードが必要な場合の finalize 用 ID（`upload_required=true`）。
    pub upload_id: Option<Uuid>,
    /// presigned PUT URL。クライアントはここへ直接バイトを PUT する。
    pub upload_url: Option<String>,
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

/// `null`（ルートへ移動）と省略（移動しない）を区別するための二重 Option デシリアライザ。
fn double_option<'de, D>(deserializer: D) -> Result<Option<Option<Uuid>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde::Deserialize::deserialize(deserializer).map(Some)
}

/// declare: メタを申告し、dedup 短絡 or presigned PUT URL を得る。
#[utoipa::path(
    post,
    path = "/files",
    request_body = UploadRequest,
    responses(
        (status = 200, description = "アップロードチケット", body = UploadTicket),
        (status = 400, description = "不正なリクエスト"),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
    ),
    security(("session" = [])),
)]
pub async fn begin_upload(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<UploadRequest>,
) -> Result<Json<UploadTicket>, ApiError> {
    let outcome = state
        .storage
        .begin_upload(
            &ctx,
            req.parent_id,
            &req.name,
            &req.content_type,
            &req.sha256,
            req.size,
            trace.as_deref(),
        )
        .await?;
    let ticket = match outcome {
        UploadOutcome::Deduplicated(node) => UploadTicket {
            upload_required: false,
            file: Some(node.into()),
            upload_id: None,
            upload_url: None,
        },
        UploadOutcome::NeedsUpload {
            upload_id,
            upload_url,
        } => UploadTicket {
            upload_required: true,
            file: None,
            upload_id: Some(upload_id),
            upload_url: Some(upload_url),
        },
    };
    Ok(Json(ticket))
}

/// finalize: 直 PUT 後に内容を検証し、ノード化する。
#[utoipa::path(
    post,
    path = "/files/{upload_id}/finalize",
    params(("upload_id" = Uuid, Path, description = "declare で得た upload_id")),
    responses(
        (status = 200, description = "確定したファイル", body = FileResponse),
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
) -> Result<Json<FileResponse>, ApiError> {
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
        (status = 200, description = "ファイルメタ", body = FileResponse),
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
) -> Result<Json<FileResponse>, ApiError> {
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
        (status = 200, description = "更新後のファイル", body = FileResponse),
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
) -> Result<Json<FileResponse>, ApiError> {
    if req.name.is_none() && req.parent_id.is_none() {
        return Err(ApiError::BadRequest(
            "name か parent_id のいずれかを指定してください".into(),
        ));
    }
    // 移動を先に行い（closure 整合）、続けてリネームする。
    let mut node = None;
    if let Some(new_parent) = req.parent_id {
        node = Some(
            state
                .storage
                .move_file(&ctx, id, new_parent, trace.as_deref())
                .await?,
        );
    }
    if let Some(new_name) = req.name {
        node = Some(
            state
                .storage
                .rename_file(&ctx, id, &new_name, trace.as_deref())
                .await?,
        );
    }
    Ok(Json(
        node.expect("move か rename のいずれかは実行済み").into(),
    ))
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
        (status = 200, description = "復元したファイル", body = FileResponse),
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
) -> Result<Json<FileResponse>, ApiError> {
    let node = state
        .storage
        .restore_file(&ctx, id, trace.as_deref())
        .await?;
    Ok(Json(node.into()))
}

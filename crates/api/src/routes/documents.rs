//! Word 文書（.docx）の作成 API（#332・下書き確定型の「ドライブに保存」/ 新規作成の共通経路）。
//!
//! `/notes`・`/slides` と同格の作成エンドポイント。本文 Markdown は blank.docx テンプレ＋
//! ingestion-worker `append_markdown`（`office.edit` と同経路・[`office::DocxComposer`]）で
//! .docx 化し、StorageService の内部書込（認可・監査・書込イベント→RAG 再索引つき）で保存する。
//! Collabora（office.enabled）には依存しない（worker のみ。markdown 省略なら worker も不要）。

use axum::extract::State;
use axum::routing::post;
use axum::Json;
use serde::Deserialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::ApiError;
use crate::extract::{AuthContextExt, TraceIdExt};
use crate::routes::collab::create_file_unique;
use crate::routes::files::NodeResponse;
use crate::server::RouteDecl;
use crate::state::AppState;

/// documents のルート宣言（office フラグ非依存・無条件配線）。
pub(crate) fn documents_route_decls() -> Vec<RouteDecl> {
    use crate::server::AccessPolicy::Session;
    vec![RouteDecl::new("/documents", &["POST"], Session, || {
        post(create_document)
    })]
}

/// Word 文書作成リクエスト（#332・「新規作成 > ドキュメント」/ 下書き確定の共通経路）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateDocumentRequest {
    /// 配置先フォルダ（None は org ルート直下）。
    pub parent_id: Option<Uuid>,
    /// ファイル名（`.docx` は自動付与）。
    pub name: String,
    /// 初期内容の Markdown（省略・空なら空ドキュメント＝blank.docx そのまま）。
    #[serde(default)]
    pub markdown: Option<String>,
}

/// Word 文書（.docx）を作成する。
///
/// 認可は StorageService の内部書込（親フォルダへの作成権限 ReBAC＋監査）に集約する
/// （単一チョークポイント・ハンドラ個別チェックなし）。同名衝突は Drive 風の連番リネーム。
#[utoipa::path(
    post, path = "/documents", request_body = CreateDocumentRequest,
    responses(
        (status = 200, description = "作成した Word 文書のノードメタ", body = NodeResponse),
        (status = 400, description = "名前または初期内容が不正"),
        (status = 401, description = "未認証"),
        (status = 403, description = "配置先への作成権限が無い"),
        (status = 503, description = "文書変換サービス（worker）に接続できない"),
    ),
    security(("session" = [])),
)]
pub async fn create_document(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<CreateDocumentRequest>,
) -> Result<Json<NodeResponse>, ApiError> {
    let name = req.name.trim();
    if name.is_empty() {
        return Err(ApiError::BadRequest("文書名を指定してください".into()));
    }
    let file_name = if name.to_ascii_lowercase().ends_with(".docx") {
        name.to_string()
    } else {
        format!("{name}.docx")
    };
    // 本文 md も下書きツールと同じ正規形へ寄せる（生 HTML はコードブロックへ縮退・Task 11P.6）。
    let markdown = req
        .markdown
        .as_deref()
        .map(collab::note::normalize_markdown)
        .unwrap_or_default();
    // 変換（worker 呼び出し）の前に配置先の作成権限を確認する（未権限ユーザーに無駄な変換を
    // させない・fail-fast）。認可の正本は write_file_internal 側にも残る（多層防御）。
    state
        .storage
        .authorize_create(&ctx, req.parent_id, trace.0.as_deref())
        .await?;
    let bytes = state
        .docx_composer
        .compose(&ctx.tenant_id, &file_name, &markdown)
        .await
        .map_err(to_api_error)?;
    let node = create_file_unique(
        &state,
        &ctx,
        req.parent_id,
        &file_name,
        &bytes,
        office::DOCX_CONTENT_TYPE,
        trace.0.as_deref(),
    )
    .await?;
    Ok(Json(NodeResponse::from(node)))
}

/// compose のエラーを HTTP へ写す（422=入力不正→400 / worker 不達→503・理由は隠さない範囲で）。
fn to_api_error(err: office::OfficeError) -> ApiError {
    match err {
        office::OfficeError::Invalid(msg) => ApiError::BadRequest(msg),
        office::OfficeError::Worker(msg) => {
            ApiError::ServiceUnavailable(format!("document compose: {msg}"))
        }
        other => ApiError::Internal(format!("document compose: {other}")),
    }
}

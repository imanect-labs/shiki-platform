//! Office ファイル（.docx / .xlsx）の作成 API（#332・#381）。
//!
//! `/notes`・`/slides` と同格の作成エンドポイント。保存は StorageService の内部書込
//! （認可・監査・書込イベント→RAG 再索引つき）で行う。
//!
//! - `POST /documents`: Word 文書。空テンプレ（blank.docx）をそのまま実体化する。
//! - `POST /sheets`: Excel ブック。空テンプレ（blank.xlsx）をそのまま実体化する（#381）。
//!
//! **本文を持つ新規作成はここには無い**（#381）。md→docx の自前変換を新規作成の入口に残すと、
//! 「Collabora 経由（忠実）」と「append_markdown 経由（劣化）」の 2 経路が公開 API に併存し、
//! どちらで作ったかで結果が変わる。本文入りの作成は AI の `save_document` / `save_sheet`
//! （Collabora へ paste）に一本化した。`DocxComposer` はノートの docx エクスポート
//! （`POST /documents/export`）と `office.edit` のためだけに残る。

use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Json;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
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
///
/// エクスポートの md→docx 変換は worker 往復（最大 1 分）を含むため
/// **SessionLongRunning（300s）** に置く。既定の Session（30s）だと 30〜60s の変換が
/// API 側タイムアウトで切られ、worker 完了前に失敗する（/files finalize と同じ扱い）。
/// 作成系は変換を伴わない（空テンプレ書込のみ）が、Office 作成の入口として同じ扱いにする。
pub(crate) fn documents_route_decls() -> Vec<RouteDecl> {
    use crate::server::AccessPolicy::SessionLongRunning;
    let r = RouteDecl::new;
    vec![
        r("/documents", &["POST"], SessionLongRunning, || {
            post(create_document)
        }),
        r("/documents/export", &["POST"], SessionLongRunning, || {
            post(export_document)
        }),
        r("/sheets", &["POST"], SessionLongRunning, || {
            post(create_sheet)
        }),
    ]
}

/// Excel ブック作成リクエスト（#381・「新規作成 > スプレッドシート（Excel）」）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateSheetRequest {
    /// 配置先フォルダ（None は org ルート直下）。
    pub parent_id: Option<Uuid>,
    /// ファイル名（`.xlsx` は自動付与）。
    pub name: String,
}

/// Excel ブック（.xlsx）を空テンプレから作成する（#381）。
///
/// 認可は StorageService の内部書込に集約する（単一チョークポイント）。同名衝突は
/// Drive 風の連番リネーム。変換を伴わないため worker にも Collabora にも依存しない。
#[utoipa::path(
    post, path = "/sheets", request_body = CreateSheetRequest,
    responses(
        (status = 200, description = "作成した Excel ブックのノードメタ", body = NodeResponse),
        (status = 400, description = "名前が不正"),
        (status = 401, description = "未認証"),
        (status = 403, description = "配置先への作成権限が無い"),
    ),
    security(("session" = [])),
)]
pub async fn create_sheet(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<CreateSheetRequest>,
) -> Result<Json<NodeResponse>, ApiError> {
    create_blank(
        &state,
        &ctx,
        trace.0.as_deref(),
        req.parent_id,
        &req.name,
        office::OfficeKind::Spreadsheet,
    )
    .await
}

/// Word 文書作成リクエスト（#332・「新規作成 > ドキュメント」）。
///
/// 本文は受けない（#381）。作成後は Collabora Writer で書く。
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateDocumentRequest {
    /// 配置先フォルダ（None は org ルート直下）。
    pub parent_id: Option<Uuid>,
    /// ファイル名（`.docx` は自動付与）。
    pub name: String,
}

/// Word 文書（.docx）を空テンプレから作成する。
///
/// 認可は StorageService の内部書込（親フォルダへの作成権限 ReBAC＋監査）に集約する
/// （単一チョークポイント・ハンドラ個別チェックなし）。同名衝突は Drive 風の連番リネーム。
#[utoipa::path(
    post, path = "/documents", request_body = CreateDocumentRequest,
    responses(
        (status = 200, description = "作成した Word 文書のノードメタ", body = NodeResponse),
        (status = 400, description = "名前が不正"),
        (status = 401, description = "未認証"),
        (status = 403, description = "配置先への作成権限が無い"),
    ),
    security(("session" = [])),
)]
pub async fn create_document(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<CreateDocumentRequest>,
) -> Result<Json<NodeResponse>, ApiError> {
    create_blank(
        &state,
        &ctx,
        trace.0.as_deref(),
        req.parent_id,
        &req.name,
        office::OfficeKind::Document,
    )
    .await
}

/// 空テンプレを実体化する共通経路（Word/Excel・#381）。
async fn create_blank(
    state: &AppState,
    ctx: &authz::AuthContext,
    trace_id: Option<&str>,
    parent_id: Option<Uuid>,
    name: &str,
    kind: office::OfficeKind,
) -> Result<Json<NodeResponse>, ApiError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ApiError::BadRequest("ファイル名を指定してください".into()));
    }
    let node = create_file_unique(
        state,
        ctx,
        parent_id,
        &kind.file_name(name),
        office::blank_template(kind),
        kind.content_type(),
        trace_id,
    )
    .await?;
    Ok(Json(NodeResponse::from(node)))
}

/// Word 文書エクスポートリクエスト（#334・ノートの docx エクスポート）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct ExportDocumentRequest {
    /// ダウンロードファイル名（`.docx` は自動付与・Content-Disposition に載る）。
    pub name: String,
    /// 本文の Markdown（チャート等は画像 data URL の image 行として埋め込み済み）。
    pub markdown: String,
}

/// Markdown を .docx へ変換して bytes を返す（保存しない・#334）。
///
/// ノードへ一切アクセスしない純変換のため、認可はセッションのみ（confused-deputy 面なし）。
/// 本文はクライアント（ノートエディタの表示内容）が持ち込む。
#[utoipa::path(
    post, path = "/documents/export", request_body = ExportDocumentRequest,
    responses(
        (status = 200, description = ".docx バイナリ（attachment）", content_type = "application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
        (status = 400, description = "内容が不正（worker が拒否）"),
        (status = 401, description = "未認証"),
        (status = 503, description = "文書変換サービス（worker）に接続できない"),
    ),
    security(("session" = [])),
)]
pub async fn export_document(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Json(req): Json<ExportDocumentRequest>,
) -> Result<Response, ApiError> {
    let name = req.name.trim();
    if name.is_empty() {
        return Err(ApiError::BadRequest("ファイル名を指定してください".into()));
    }
    let file_name = if name.to_ascii_lowercase().ends_with(".docx") {
        name.to_string()
    } else {
        format!("{name}.docx")
    };
    let bytes = state
        .docx_composer
        .compose(&ctx.tenant_id, &file_name, &req.markdown)
        .await
        .map_err(to_api_error)?;
    // ファイル名は RFC 5987（filename*）で UTF-8 のままエンコードする（日本語名を壊さない）。
    let encoded_name = utf8_percent_encode(&file_name, NON_ALPHANUMERIC).to_string();
    let headers = [
        (header::CONTENT_TYPE, office::DOCX_CONTENT_TYPE.to_string()),
        (
            header::CONTENT_DISPOSITION,
            format!("attachment; filename*=UTF-8''{encoded_name}"),
        ),
    ];
    Ok((headers, bytes).into_response())
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

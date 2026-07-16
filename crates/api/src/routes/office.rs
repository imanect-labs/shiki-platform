//! Office 編集セッション発行（Task 11.6・design §4.8）。
//!
//! `/office/sessions` はブラウザ（cookie セッション）から呼ばれ、Collabora の編集
//! アクション URL と WOPI access_token を発行する。トークンは UX 用の入場券であり
//! 権限の根拠ではない（WOPI 側が毎呼び出しで ReBAC 再チェックする・PIT-11）。

use axum::extract::State;
use axum::routing::post;
use axum::Json;
use serde::{Deserialize, Serialize};
use storage::NodeKind;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::ApiError;
use crate::extract::{AuthContextExt, TraceIdExt};
use crate::server::RouteDecl;
use crate::state::AppState;

/// office のルート宣言（route_table から分離・同じ宣言的マップの一部）。
///
/// `office.enabled=false` の構成では `build_router` がこの宣言を配線しない
/// （`/office/` プレフィックスで判定・fail-closed）。
pub(crate) fn office_route_decls() -> Vec<RouteDecl> {
    use crate::server::AccessPolicy::Session;
    let r = RouteDecl::new;
    vec![r("/office/sessions", &["POST"], Session, || {
        post(create_office_session)
    })]
}

/// Office 編集セッションの発行リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateOfficeSessionRequest {
    /// 編集対象のファイル（docx/xlsx/pptx/odt/ods/odp）。
    pub file_id: Uuid,
}

/// Office 編集セッション（Collabora iframe の組み立て材料）。
#[derive(Debug, Serialize, ToSchema)]
pub struct OfficeSessionResponse {
    /// Collabora の編集アクション URL（discovery 由来・WOPISrc を付与して使う）。
    pub action_url: String,
    /// WOPI access_token（実行主体×ファイル×短寿命。form post で iframe へ注入する）。
    pub access_token: String,
    /// トークンの有効期間（ミリ秒）。
    pub access_token_ttl_ms: u64,
}

/// Office 編集セッションを発行する。
///
/// viewer 権限が要る（無ければ存在秘匿の 404）。未対応拡張子・Collabora 未設定も
/// 404。トークンにはテナント境界と file_id を焼き込み、他ファイルへ流用できない。
#[utoipa::path(
    post, path = "/office/sessions", request_body = CreateOfficeSessionRequest,
    responses(
        (status = 200, description = "編集セッション", body = OfficeSessionResponse),
        (status = 401, description = "未認証"),
        (status = 404, description = "ファイルが存在しない・読めない（存在秘匿）・未対応拡張子"),
        (status = 503, description = "Collabora discovery の取得失敗（一時障害）"),
    ),
    security(("session" = [])),
)]
pub async fn create_office_session(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<CreateOfficeSessionRequest>,
) -> Result<Json<OfficeSessionResponse>, ApiError> {
    // enabled=false ではルート自体が配線されないが、防御的に 404 とする（fail-closed）。
    let Some(office) = &state.office else {
        return Err(ApiError::NotFound);
    };
    // viewer check（StorageService 経由・監査つき）。権限なしは存在秘匿の 404。
    let node = state
        .storage
        .get_metadata(&ctx, req.file_id, trace.0.as_deref())
        .await
        .map_err(|e| match ApiError::from(e) {
            ApiError::Forbidden => ApiError::NotFound,
            other => other,
        })?;
    if node.kind != NodeKind::File {
        return Err(ApiError::NotFound);
    }
    let ext = node
        .name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    let action_url = match office.suite.editor_action_url(&ext).await {
        Ok(Some(url)) => url,
        // 未対応拡張子は「編集面が存在しない」＝404（ドライブ側はダウンロードへ誘導）。
        Ok(None) => return Err(ApiError::NotFound),
        // discovery 失敗は機能 off の fail-closed（Collabora 停止等の一時障害）。
        Err(office::OfficeError::Discovery(detail)) => {
            return Err(ApiError::ServiceUnavailable(format!(
                "office discovery: {detail}"
            )));
        }
        Err(e) => return Err(ApiError::Internal(format!("office: {e}"))),
    };
    let access_token = office::wopi::token::issue(&office.wopi.token_key, &ctx, req.file_id)
        .map_err(|e| ApiError::Internal(format!("office token: {e}")))?;
    Ok(Json(OfficeSessionResponse {
        action_url,
        access_token,
        access_token_ttl_ms: office::TOKEN_TTL.as_millis() as u64,
    }))
}

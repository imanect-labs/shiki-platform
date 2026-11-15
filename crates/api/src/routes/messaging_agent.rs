//! 職員間メッセージ API — パーソナルエージェント（名前・アイコン・自分だけに表示の回答・Task 14.8 / 14.20）。

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
    Json,
};
use messaging::{AgentProfileUpdate, AgentProfileView, PublishWarning};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::messaging::{error, service};
use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    server::{AccessPolicy, RouteDecl},
    state::AppState,
};

pub(crate) fn route_decls() -> Vec<RouteDecl> {
    vec![
        RouteDecl::new(
            "/messaging/agent",
            &["GET", "PUT"],
            AccessPolicy::Session,
            || get(get_profile).put(put_profile),
        ),
        RouteDecl::new(
            "/messaging/agent/icon",
            &["PUT", "DELETE"],
            AccessPolicy::Session,
            || put(put_icon).delete(delete_icon),
        ),
        RouteDecl::new(
            "/messaging/agents/{user_id}/icon",
            &["GET"],
            AccessPolicy::Session,
            || get(get_icon),
        ),
        RouteDecl::new(
            "/messaging/agent-replies/{id}/publish",
            &["POST"],
            AccessPolicy::Session,
            || post(publish_reply),
        ),
        RouteDecl::new(
            "/messaging/agent-replies/{id}",
            &["DELETE"],
            AccessPolicy::Session,
            || delete(dismiss_reply),
        ),
    ]
}

/// 自分のパーソナルエージェントの名前とアイコン。
#[utoipa::path(get, path = "/messaging/agent",
    responses((status = 200, description = "成功", body = AgentProfileView)),
    security(("session" = [])))]
pub async fn get_profile(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
) -> Result<Json<AgentProfileView>, ApiError> {
    Ok(Json(
        service(&state)?.agent_profile(&ctx).await.map_err(error)?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ProfileBody {
    /// 名前（空文字で既定の「シキ」に戻す）。
    pub name: String,
}

/// 名前を変える（見た目だけ・権限に影響しない・紛らわしい名前は 400）。
#[utoipa::path(put, path = "/messaging/agent", request_body = ProfileBody,
    responses((status = 200, description = "成功", body = AgentProfileView), (status = 400, description = "使えない名前")),
    security(("session" = [])))]
pub async fn put_profile(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(body): Json<ProfileBody>,
) -> Result<Json<AgentProfileView>, ApiError> {
    Ok(Json(
        service(&state)?
            .set_agent_profile(
                &ctx,
                AgentProfileUpdate {
                    name: Some(body.name),
                    icon: None,
                },
                trace.0.as_deref(),
            )
            .await
            .map_err(error)?,
    ))
}

/// アイコンを置く（本文が画像そのもの・PNG / JPEG / WebP・256 KB まで）。
#[utoipa::path(put, path = "/messaging/agent/icon", request_body(content = Vec<u8>, content_type = "image/png"),
    responses((status = 200, description = "成功", body = AgentProfileView), (status = 400, description = "画像ではない・大きすぎる")),
    security(("session" = [])))]
pub async fn put_icon(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<AgentProfileView>, ApiError> {
    if body.len() > messaging::agent_profile::MAX_ICON_BYTES {
        return Err(ApiError::BadRequest("アイコンは 256 KB までです".into()));
    }
    // 形式は申告（Content-Type）ではなく中身で決める（申告と違えば拒否）。
    let Some(mime) = messaging::agent_profile::icon_mime(&body) else {
        return Err(ApiError::BadRequest(
            "アイコンは PNG・JPEG・WebP の画像だけです".into(),
        ));
    };
    if let Some(declared) = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    {
        if !declared.starts_with(mime) && declared != "application/octet-stream" {
            return Err(ApiError::BadRequest(
                "画像の形式が申告と一致しません".into(),
            ));
        }
    }
    Ok(Json(
        service(&state)?
            .set_agent_profile(
                &ctx,
                AgentProfileUpdate {
                    name: None,
                    icon: Some(Some((body.to_vec(), mime.to_string()))),
                },
                trace.0.as_deref(),
            )
            .await
            .map_err(error)?,
    ))
}

/// アイコンを外す。
#[utoipa::path(delete, path = "/messaging/agent/icon",
    responses((status = 200, description = "成功", body = AgentProfileView)),
    security(("session" = [])))]
pub async fn delete_icon(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
) -> Result<Json<AgentProfileView>, ApiError> {
    Ok(Json(
        service(&state)?
            .set_agent_profile(
                &ctx,
                AgentProfileUpdate {
                    name: None,
                    icon: Some(None),
                },
                trace.0.as_deref(),
            )
            .await
            .map_err(error)?,
    ))
}

/// 利用者のエージェントのアイコン（組織の利用者なら見られる・プロフィール画像と同じ扱い）。
#[utoipa::path(get, path = "/messaging/agents/{user_id}/icon", params(("user_id" = String, Path)),
    responses((status = 200, description = "画像", content_type = "image/png"), (status = 404, description = "未設定")),
    security(("session" = [])))]
pub async fn get_icon(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Path(user): Path<String>,
) -> Result<Response, ApiError> {
    let (bytes, mime) = service(&state)?
        .agent_icon(&ctx, &user)
        .await
        .map_err(error)?;
    let mut resp = bytes.into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&mime).unwrap_or(HeaderValue::from_static("image/png")),
    );
    // 画像として以外に解釈させない（中身はマジックバイトで検証済み）。
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=300"),
    );
    Ok(resp)
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PublishBody {
    /// 「一部の参加者が読めない資料」の確認を済ませたか。
    #[serde(default)]
    pub confirmed: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PublishResult {
    /// 投稿したか（`false` なら `warning` を見せて確認を求める）。
    pub posted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<PublishWarning>,
}

/// 自分だけに表示中の回答をチャンネルに投稿する（参照資料を読めない参加者がいれば、まず確認を求める）。
#[utoipa::path(post, path = "/messaging/agent-replies/{id}/publish", params(("id" = Uuid, Path)), request_body = PublishBody,
    responses((status = 200, description = "投稿した・確認が要る", body = PublishResult), (status = 404, description = "無い")),
    security(("session" = [])))]
pub async fn publish_reply(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(body): Json<PublishBody>,
) -> Result<Json<PublishResult>, ApiError> {
    let warning = service(&state)?
        .publish_agent_reply(&ctx, id, body.confirmed, trace.0.as_deref())
        .await
        .map_err(error)?;
    Ok(Json(PublishResult {
        posted: warning.is_none(),
        warning,
    }))
}

/// 自分だけに表示中の回答を捨てる。
#[utoipa::path(delete, path = "/messaging/agent-replies/{id}", params(("id" = Uuid, Path)),
    responses((status = 204, description = "捨てた"), (status = 404, description = "無い")),
    security(("session" = [])))]
pub async fn dismiss_reply(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    service(&state)?
        .dismiss_agent_reply(&ctx, id)
        .await
        .map_err(error)?;
    Ok(StatusCode::NO_CONTENT)
}

//! 本人ボタン専用。自然言語や AI ツールから未解決・報告希望を確定しない。
use crate::{
    extract::AuthContextExt,
    routes::bug_reports::{error, service},
    server::{AccessPolicy, RouteDecl},
    state::AppState,
};
use axum::{
    extract::{Path, State},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json,
};
use shiki_bug_report::{HelpCase, HelpResolution};
use uuid::Uuid;

pub(crate) fn route_decls() -> Vec<RouteDecl> {
    vec![
        RouteDecl::new(
            "/threads/{id}/help-cases/{case_id}",
            &["GET"],
            AccessPolicy::Session,
            || get(detail),
        ),
        RouteDecl::new(
            "/threads/{id}/help-cases/{case_id}/resolution",
            &["POST"],
            AccessPolicy::Session,
            || post(resolve),
        ),
        RouteDecl::new(
            "/threads/{id}/help-cases/{case_id}/report-intent",
            &["POST"],
            AccessPolicy::Session,
            || post(intent),
        ),
    ]
}
#[utoipa::path(get,path="/threads/{id}/help-cases/{case_id}",params(("id"=Uuid,Path),("case_id"=Uuid,Path)),responses((status=200,body=HelpCase),(status=404,description="本人の有効な案内がない")),security(("session"=[])))]
pub async fn detail(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Path((thread, id)): Path<(Uuid, Uuid)>,
) -> Result<Json<HelpCase>, Response> {
    Ok(Json(
        service(&state)
            .map_err(IntoResponse::into_response)?
            .help_case(&ctx, thread, id)
            .await
            .map_err(error)?,
    ))
}
#[utoipa::path(post,path="/threads/{id}/help-cases/{case_id}/resolution",params(("id"=Uuid,Path),("case_id"=Uuid,Path)),request_body=HelpResolution,responses((status=200,body=HelpCase),(status=409,description="確認が競合")),security(("session"=[])))]
pub async fn resolve(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Path((thread, id)): Path<(Uuid, Uuid)>,
    Json(resolution): Json<HelpResolution>,
) -> Result<Json<HelpCase>, Response> {
    Ok(Json(
        service(&state)
            .map_err(IntoResponse::into_response)?
            .resolve_help_case(&ctx, thread, id, resolution)
            .await
            .map_err(error)?,
    ))
}
#[utoipa::path(post,path="/threads/{id}/help-cases/{case_id}/report-intent",params(("id"=Uuid,Path),("case_id"=Uuid,Path)),responses((status=200,body=HelpCase),(status=409,description="未解決の確認が必要")),security(("session"=[])))]
pub async fn intent(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Path((thread, id)): Path<(Uuid, Uuid)>,
) -> Result<Json<HelpCase>, Response> {
    Ok(Json(
        service(&state)
            .map_err(IntoResponse::into_response)?
            .help_report_intent(&ctx, thread, id)
            .await
            .map_err(error)?,
    ))
}

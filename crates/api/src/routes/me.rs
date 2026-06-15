//! `GET /me` — 認証＋認可の縦の最小貫通（docs/roadmap phase-0 Task 0.6）。
//!
//! JWT 検証（middleware）→ principal → AuthContext extractor → OpenFGA check →
//! ユーザー情報 JSON、を 1 リクエストで協調させる。

use authz::{FgaObject, Relation};
use axum::{extract::State, Json};
use serde::Serialize;
use utoipa::ToSchema;

use crate::{error::ApiError, extract::AuthContextExt, state::AppState};

#[derive(Debug, Serialize, ToSchema)]
pub struct MeResponse {
    pub id: String,
    pub email: Option<String>,
    pub dept: Option<String>,
    pub org: String,
}

/// 認証済みユーザー自身の情報を返す。
#[utoipa::path(
    get,
    path = "/me",
    responses(
        (status = 200, description = "ユーザー情報", body = MeResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
    ),
    security(("bearer" = [])),
)]
pub async fn get_me(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
) -> Result<Json<MeResponse>, ApiError> {
    // 認可: 自分が自 org の member か（単一チョークポイント経由の check）。
    let allowed = state
        .authz
        .check(
            &ctx.subject(),
            Relation::Member,
            &FgaObject::organization(&ctx.org),
        )
        .await?;
    if !allowed {
        return Err(ApiError::Forbidden);
    }

    Ok(Json(MeResponse {
        id: ctx.principal.id,
        email: ctx.principal.email,
        dept: ctx.principal.dept,
        org: ctx.org,
    }))
}

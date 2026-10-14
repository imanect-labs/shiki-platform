//! `GET /me` — 認証＋認可の縦の最小貫通（docs/roadmap phase-0 Task 0.6）。
//!
//! JWT 検証（middleware）→ principal → AuthContext extractor → OpenFGA check →
//! ユーザー情報 JSON、を 1 リクエストで協調させる。

use authz::{Consistency, Relation};
use axum::{extract::State, Json};
use serde::Serialize;
use utoipa::ToSchema;

use crate::{error::ApiError, extract::AuthContextExt, state::AppState};

#[derive(Debug, Serialize, ToSchema)]
pub struct MeResponse {
    pub id: String,
    pub email: Option<String>,
    /// IdP が宣言した所属ロール（多値）。階層込みの実効メンバーシップは OpenFGA が正本。
    pub roles: Vec<String>,
    pub org: String,
    /// テナント識別子（SaaS の隔離境界。オンプレは単一固定）。
    pub tenant_id: String,
    /// 組織管理者か（管理コンソールの入口を出すか・`organization#admin`）。
    /// 表示のための値で、管理 API 自体はルートのポリシーで毎回確かめる。
    pub is_admin: bool,
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
    security(("session" = [])),
)]
pub async fn get_me(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
) -> Result<Json<MeResponse>, ApiError> {
    // 認可: 自分が自 org の member か（単一チョークポイント経由の check）。
    // 管理者かどうかはメニューの表示にだけ使うので、並べて低遅延で引く（操作はゲートが強い整合性で再判定）。
    let org = ctx.ns().organization(&ctx.org);
    let subject = ctx.subject();
    let member = async {
        Ok::<_, ApiError>(
            state
                .authz
                .check(
                    &subject,
                    Relation::Member,
                    &org,
                    Consistency::MinimizeLatency,
                )
                .await?,
        )
    };
    let (allowed, is_admin) = tokio::try_join!(
        member,
        crate::middleware::org_admin::is_org_admin(&state, &ctx, Consistency::MinimizeLatency),
    )?;
    if !allowed {
        return Err(ApiError::Forbidden);
    }

    Ok(Json(MeResponse {
        is_admin,
        id: ctx.principal.id,
        email: ctx.principal.email,
        roles: ctx.principal.roles,
        org: ctx.org,
        tenant_id: ctx.tenant_id,
    }))
}

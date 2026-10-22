//! 管理コンソールの構造化データ設定（`/org-admin/data/settings`・組織管理者のみ・Task 15.3）。
//!
//! 件数の上限（上限付き正確カウント・data-platform §5.4）をテナント単位で変える。
//! 管理者チェックはルート表のポリシー（`AccessPolicy::OrgAdmin`）が一律に掛ける。変更は監査に残す。

use axum::{extract::State, routing::get, Json};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    server::{AccessPolicy, RouteDecl},
    state::AppState,
};

pub(crate) fn org_admin_data_route_decls() -> Vec<RouteDecl> {
    vec![RouteDecl::new(
        "/org-admin/data/settings",
        &["GET", "PUT"],
        AccessPolicy::OrgAdmin,
        || get(get_data_settings).put(update_data_settings),
    )]
}

/// 構造化データ設定の更新。
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateDataSettingsRequest {
    /// 件数の上限（1〜10,000,000。`null` で既定 10,000 へ戻す）。
    pub count_cap: Option<u64>,
}

/// テナントの構造化データ設定。
#[utoipa::path(
    get,
    path = "/org-admin/data/settings",
    responses(
        (status = 200, description = "構造化データ設定", body = data::DataTenantSettings),
        (status = 403, description = "組織管理者ではない"),
    ),
    security(("session" = [])),
)]
pub async fn get_data_settings(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
) -> Result<Json<data::DataTenantSettings>, ApiError> {
    Ok(Json(state.data.tenant_settings(&ctx).await?))
}

/// テナントの構造化データ設定を更新する（範囲外は 400・監査に残す）。
#[utoipa::path(
    put,
    path = "/org-admin/data/settings",
    request_body = UpdateDataSettingsRequest,
    responses(
        (status = 200, description = "更新後の設定", body = data::DataTenantSettings),
        (status = 400, description = "範囲外"),
        (status = 403, description = "組織管理者ではない"),
    ),
    security(("session" = [])),
)]
pub async fn update_data_settings(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<UpdateDataSettingsRequest>,
) -> Result<Json<data::DataTenantSettings>, ApiError> {
    Ok(Json(
        state
            .data
            .update_tenant_count_cap(&ctx, req.count_cap, trace.as_deref())
            .await?,
    ))
}

//! 管理コンソールの構造化データ設定（`/org-admin/data/settings`・組織管理者のみ・Task 15.3）。
//!
//! 件数の上限（上限付き正確カウント・data-platform §5.4）をテナント単位で変える。
//! テーブルごとのクエリ最適化の報告と固定計画（`/org-admin/data/tables/{id}/optimizer`・Task 15.12）:
//! 走査効率などの値を含まない運用の数値と、複合索引・索引の昇格の候補（列の参照名）を出し、機微な
//! テーブルを統計に依らない固定計画へ切り替える（query-optimizer.md §7.3・§8）。推定値・統計値・
//! 予測コスト・計画名は返さない（PIT-75）。
//! オプティマイザの環境の状態（`/org-admin/data/optimizer`）: 起動時の較正の前後の単価と、検証用の操作で
//! 見た予測と実測の一致の度合い（相関・平均対数誤差）の前後。環境（DB サーバ）の数値だけで、テナントの
//! データ・統計は含まない（運用者向け・query-optimizer.md §5.2・§7.3）。
//! 管理者チェックはルート表のポリシー（`AccessPolicy::OrgAdmin`）が一律に掛ける。変更は監査に残す。

use axum::{
    extract::{Path, State},
    routing::get,
    Json,
};
use serde::Deserialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    server::{AccessPolicy, RouteDecl},
    state::AppState,
};

pub(crate) fn org_admin_data_route_decls() -> Vec<RouteDecl> {
    vec![
        RouteDecl::new(
            "/org-admin/data/settings",
            &["GET", "PUT"],
            AccessPolicy::OrgAdmin,
            || get(get_data_settings).put(update_data_settings),
        ),
        RouteDecl::new(
            "/org-admin/data/optimizer",
            &["GET"],
            AccessPolicy::OrgAdmin,
            || get(get_optimizer_status),
        ),
        RouteDecl::new(
            "/org-admin/data/tables/{id}/optimizer",
            &["GET", "PUT"],
            AccessPolicy::OrgAdmin,
            || get(get_table_optimizer).put(update_table_optimizer),
        ),
    ]
}

/// テーブルのクエリ最適化の設定の更新。
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateTableOptimizerRequest {
    /// 統計に依らない固定計画で読む（機微なテーブル向け・応答時間から見えない行の分布を推せないようにする）。
    pub fixed_plan: bool,
}

/// オプティマイザの環境の状態（単価と起動時の較正の前後・テナントのデータを含まない）。
#[utoipa::path(
    get,
    path = "/org-admin/data/optimizer",
    responses(
        (status = 200, description = "単価と較正の前後", body = data::OptimizerStatus),
        (status = 403, description = "組織管理者ではない"),
    ),
    security(("session" = [])),
)]
pub async fn get_optimizer_status(
    State(state): State<AppState>,
) -> Result<Json<data::OptimizerStatus>, ApiError> {
    Ok(Json(state.data.optimizer_status()))
}

/// テーブルのクエリ最適化の報告（値・推定値を含まない運用の数値と索引の候補）。
#[utoipa::path(
    get,
    path = "/org-admin/data/tables/{id}/optimizer",
    params(("id" = Uuid, Path, description = "テーブル ID")),
    responses(
        (status = 200, description = "クエリ最適化の報告", body = data::TableOptimizerReport),
        (status = 403, description = "組織管理者ではない"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn get_table_optimizer(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Path(id): Path<Uuid>,
) -> Result<Json<data::TableOptimizerReport>, ApiError> {
    Ok(Json(state.data.table_optimizer_report(&ctx, id).await?))
}

/// テーブルの固定計画を切り替える（監査に残す）。
#[utoipa::path(
    put,
    path = "/org-admin/data/tables/{id}/optimizer",
    params(("id" = Uuid, Path, description = "テーブル ID")),
    request_body = UpdateTableOptimizerRequest,
    responses(
        (status = 200, description = "更新後の報告", body = data::TableOptimizerReport),
        (status = 403, description = "組織管理者ではない"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn update_table_optimizer(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateTableOptimizerRequest>,
) -> Result<Json<data::TableOptimizerReport>, ApiError> {
    Ok(Json(
        state
            .data
            .set_table_fixed_plan(&ctx, id, req.fixed_plan, trace.as_deref())
            .await?,
    ))
}

/// 構造化データ設定の更新。
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateDataSettingsRequest {
    /// 件数の上限（1〜10,000,000。`null` で既定 10,000 へ戻す）。
    pub count_cap: Option<u64>,
    /// テーブルあたりの索引列数の上限（1〜1,000。`null` で既定 20 へ戻す。**省略すると変えない**）。
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<u32>)]
    #[allow(clippy::option_option)] // 省略（変えない）と null（既定へ戻す）を区別する。
    pub max_indexed_fields: Option<Option<u32>>,
    /// 1 ビューのルックアップ・ロールアップ列の上限（省略は変えない・`null` は既定 12 へ戻す・Task 15.5）。
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<u32>)]
    #[allow(clippy::option_option)]
    pub max_view_lookup_columns: Option<Option<u32>>,
}

/// 「省略」と「null」を区別する（省略 = `None`・null = `Some(None)`）。
#[allow(clippy::option_option)]
fn present<'de, D>(d: D) -> Result<Option<Option<u32>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<u32>::deserialize(d).map(Some)
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
    let mut settings = state
        .data
        .update_tenant_count_cap(&ctx, req.count_cap, trace.as_deref())
        .await?;
    if let Some(cap) = req.max_indexed_fields {
        settings = state
            .data
            .update_tenant_index_cap(&ctx, cap, trace.as_deref())
            .await?;
    }
    if let Some(cap) = req.max_view_lookup_columns {
        settings = state
            .data
            .update_tenant_view_lookup_cap(&ctx, cap, trace.as_deref())
            .await?;
    }
    Ok(Json(settings))
}

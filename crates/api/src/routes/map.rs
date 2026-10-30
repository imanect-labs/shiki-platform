//! `GET /map/config` — 地図の背景タイルの設定（回答の ```ui の map を描く web 向け）。
//!
//! 背景タイルはブラウザが提供元から直接読む（外部接続必須機能・requirements NFR-2）。
//! 返すのはスタイルの URL と Mapbox の**公開**トークン（`pk.*`）だけで、秘密は持たない。
//! 地図が無効な構成（オンプレ・エアギャップ）では `provider: null` を返し、web は静止画を描く。

use axum::{extract::State, routing::get, Json};
use serde::Serialize;
use utoipa::ToSchema;

use crate::{
    config::MapProvider,
    error::ApiError,
    extract::AuthContextExt,
    server::{AccessPolicy, RouteDecl},
    state::AppState,
};

pub(crate) fn map_route_decls() -> Vec<RouteDecl> {
    vec![RouteDecl::new(
        "/map/config",
        &["GET"],
        AccessPolicy::Session,
        || get(map_config),
    )]
}

/// 背景タイルの提供元。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MapProviderKind {
    Mapbox,
    Style,
}

impl From<MapProvider> for MapProviderKind {
    fn from(p: MapProvider) -> Self {
        match p {
            MapProvider::Mapbox => Self::Mapbox,
            MapProvider::Style => Self::Style,
        }
    }
}

/// `GET /map/config` のレスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct MapConfigResponse {
    /// 提供元（`null` なら地図の背景を出さない）。
    pub provider: Option<MapProviderKind>,
    /// ライト用のスタイル JSON の URL。
    pub style_url_light: Option<String>,
    /// ダーク用のスタイル JSON の URL。
    pub style_url_dark: Option<String>,
    /// Mapbox の公開トークン（スタイル内の `mapbox://` の URL を書き換えるときに付ける）。
    pub mapbox_token: Option<String>,
}

/// 地図の背景タイルの設定。
#[utoipa::path(
    get,
    path = "/map/config",
    responses(
        (status = 200, description = "地図の設定（無効なら provider が null）", body = MapConfigResponse),
        (status = 401, description = "未認証"),
    ),
    security(("session" = [])),
)]
pub async fn map_config(
    State(state): State<AppState>,
    AuthContextExt(_ctx): AuthContextExt,
) -> Result<Json<MapConfigResponse>, ApiError> {
    // 起動時に検証済み（AppConfig::validate）なので、ここで失敗するのは設定の取り違えだけ。
    let client = state
        .config
        .map
        .client()
        .map_err(|e| ApiError::Internal(format!("map config: {e}")))?;
    Ok(Json(match client {
        Some(c) => MapConfigResponse {
            provider: Some(c.provider.into()),
            style_url_light: Some(c.style_url_light),
            style_url_dark: Some(c.style_url_dark),
            mapbox_token: c.mapbox_token,
        },
        None => MapConfigResponse {
            provider: None,
            style_url_light: None,
            style_url_dark: None,
            mapbox_token: None,
        },
    }))
}

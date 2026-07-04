//! ユーザーディレクトリ検索（共有ダイアログの相手検索）。Task 1.10 / #20。
//!
//! 共有相手（個人）を email / 表示名で検索する。`DirectoryStore` 経由で呼び出し元の
//! `tenant_id`（＋ org）に必ず絞り込むため、別テナントのユーザーは結果に出ない
//! （SaaS 隔離境界＝`tenant_id`）。全件取得を避けるため keyset カーソルでページングする。

use axum::{
    extract::{Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use storage::{DirectoryRole, DirectoryUser, DEFAULT_SEARCH_LIMIT};
use utoipa::{IntoParams, ToSchema};

use crate::{error::ApiError, extract::AuthContextExt, state::AppState};

/// 検索クエリ（部分一致語・カーソル・件数）。
#[derive(Debug, Deserialize, IntoParams)]
pub struct DirectorySearchQuery {
    /// email / 表示名の部分一致語。空なら同テナントの先頭ページ。
    #[serde(default)]
    pub q: String,
    /// 前回応答の `next_cursor`。続きから取得する（省略で先頭）。
    pub cursor: Option<String>,
    /// 1 ページの最大件数（1..=50。既定 20）。
    pub limit: Option<usize>,
}

/// 検索結果の 1 ユーザー（共有相手候補）。
#[derive(Debug, Serialize, ToSchema)]
pub struct DirectoryUserResponse {
    /// 共有 tuple の `user:<id>` に使う識別子（OIDC `sub`）。
    pub id: String,
    pub email: String,
    pub display_name: String,
}

impl From<DirectoryUser> for DirectoryUserResponse {
    fn from(u: DirectoryUser) -> Self {
        Self {
            id: u.id,
            email: u.email,
            display_name: u.display_name,
        }
    }
}

/// 検索の 1 ページ。
#[derive(Debug, Serialize, ToSchema)]
pub struct DirectorySearchResponse {
    pub items: Vec<DirectoryUserResponse>,
    /// 続きがあれば次回 `cursor` に渡す値（末尾なら `null`）。
    pub next_cursor: Option<String>,
}

/// 同テナント（＋ org）のユーザーを検索する（自分自身は除外）。
#[utoipa::path(
    get,
    path = "/directory/users",
    params(DirectorySearchQuery),
    responses(
        (status = 200, description = "検索結果（同テナントのみ）", body = DirectorySearchResponse),
        (status = 401, description = "未認証"),
    ),
    security(("session" = [])),
)]
pub async fn search_users(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Query(q): Query<DirectorySearchQuery>,
) -> Result<Json<DirectorySearchResponse>, ApiError> {
    let page = state
        .directory
        .search(
            &ctx,
            &q.q,
            q.cursor.as_deref(),
            q.limit.unwrap_or(DEFAULT_SEARCH_LIMIT),
        )
        .await?;
    Ok(Json(DirectorySearchResponse {
        items: page.items.into_iter().map(Into::into).collect(),
        next_cursor: page.next_cursor,
    }))
}

/// 検索結果の 1 ロール/部署（共有相手候補）。#76。
#[derive(Debug, Serialize, ToSchema)]
pub struct DirectoryRoleResponse {
    /// 共有 tuple の `role:<id>#member` に使う識別子（Keycloak role/group 由来）。
    pub id: String,
    pub display_name: String,
}

impl From<DirectoryRole> for DirectoryRoleResponse {
    fn from(r: DirectoryRole) -> Self {
        Self {
            id: r.id,
            display_name: r.display_name,
        }
    }
}

/// ロール検索の 1 ページ。
#[derive(Debug, Serialize, ToSchema)]
pub struct DirectoryRoleSearchResponse {
    pub items: Vec<DirectoryRoleResponse>,
    pub next_cursor: Option<String>,
}

/// ロール検索クエリ（ユーザー検索と `q` の意味が異なるため DTO を分ける）。
#[derive(Debug, Deserialize, IntoParams)]
pub struct DirectoryRoleSearchQuery {
    /// role id / 表示名の部分一致語。空なら同テナントの先頭ページ。
    #[serde(default)]
    pub q: String,
    /// 前回応答の `next_cursor`。続きから取得する（省略で先頭）。
    pub cursor: Option<String>,
    /// 1 ページの最大件数（1..=50。既定 20）。
    pub limit: Option<usize>,
}

/// 同テナント（＋ org）のロール/部署を検索する（共有ダイアログのオートコンプリート・#76）。
#[utoipa::path(
    get,
    path = "/directory/roles",
    params(DirectoryRoleSearchQuery),
    responses(
        (status = 200, description = "検索結果（同テナントのみ）", body = DirectoryRoleSearchResponse),
        (status = 401, description = "未認証"),
    ),
    security(("session" = [])),
)]
pub async fn search_roles(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Query(q): Query<DirectoryRoleSearchQuery>,
) -> Result<Json<DirectoryRoleSearchResponse>, ApiError> {
    let page = state
        .directory
        .search_roles(
            &ctx,
            &q.q,
            q.cursor.as_deref(),
            q.limit.unwrap_or(DEFAULT_SEARCH_LIMIT),
        )
        .await?;
    Ok(Json(DirectoryRoleSearchResponse {
        items: page.items.into_iter().map(Into::into).collect(),
        next_cursor: page.next_cursor,
    }))
}

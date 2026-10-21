//! 回答への評価（👍 / 👎）のルート（#751）。
//!
//! FR-16 の既定どおりメタデータだけ（評価・👎 の理由カテゴリ）。本文・自由記述は受け付けない。
//! 認可はスレッドの viewer（`ChatStore` が判定する）。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

use super::chat::chat_store;

/// 評価。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackRating {
    Up,
    Down,
}

/// 👎 の理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackCategory {
    /// 内容が不正確。
    Inaccurate,
    /// 社内の情報が見つからない。
    NotFound,
    /// 指示どおりでない。
    NotFollowed,
    /// 遅い。
    Slow,
    /// やり方がわからなかった（ヘルプの穴の検出に使う・FR-16）。
    HowTo,
    /// その他。
    Other,
}

impl FeedbackCategory {
    /// 保存値（`message_feedback.category`）。
    pub fn as_str(self) -> &'static str {
        match self {
            FeedbackCategory::Inaccurate => "inaccurate",
            FeedbackCategory::NotFound => "not_found",
            FeedbackCategory::NotFollowed => "not_followed",
            FeedbackCategory::Slow => "slow",
            FeedbackCategory::HowTo => "how_to",
            FeedbackCategory::Other => "other",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        [
            FeedbackCategory::Inaccurate,
            FeedbackCategory::NotFound,
            FeedbackCategory::NotFollowed,
            FeedbackCategory::Slow,
            FeedbackCategory::HowTo,
            FeedbackCategory::Other,
        ]
        .into_iter()
        .find(|c| c.as_str() == s)
    }
}

/// `PUT .../feedback` の本文。
#[derive(Debug, Deserialize, ToSchema)]
pub struct SetFeedbackRequest {
    pub rating: FeedbackRating,
    /// 👎 の理由（👍 では指定しない）。
    #[serde(default)]
    pub category: Option<FeedbackCategory>,
}

/// 自分の評価 1 件。
#[derive(Debug, Serialize, ToSchema)]
pub struct FeedbackItem {
    pub message_id: Uuid,
    pub rating: FeedbackRating,
    pub category: Option<FeedbackCategory>,
}

/// `GET /threads/{id}/feedback` のレスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct ThreadFeedbackResponse {
    pub items: Vec<FeedbackItem>,
}

/// このスレッドで自分が付けた評価（再訪時にボタンの状態を戻す）。
#[utoipa::path(
    get, path = "/threads/{id}/feedback",
    params(("id" = Uuid, Path, description = "スレッド ID")),
    responses(
        (status = 200, description = "自分の評価", body = ThreadFeedbackResponse),
        (status = 403, description = "認可されていない"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn get_thread_feedback(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<ThreadFeedbackResponse>, ApiError> {
    let rows = chat_store(&state)?
        .my_feedback(&ctx, id, trace.as_deref())
        .await?;
    let items = rows
        .into_iter()
        .map(|f| FeedbackItem {
            message_id: f.message_id,
            rating: if f.rating > 0 {
                FeedbackRating::Up
            } else {
                FeedbackRating::Down
            },
            category: f.category.as_deref().and_then(FeedbackCategory::parse),
        })
        .collect();
    Ok(Json(ThreadFeedbackResponse { items }))
}

/// 回答に評価を付ける（付け直しは上書き）。
#[utoipa::path(
    put, path = "/threads/{id}/messages/{message_id}/feedback", request_body = SetFeedbackRequest,
    params(
        ("id" = Uuid, Path, description = "スレッド ID"),
        ("message_id" = Uuid, Path, description = "評価する回答（assistant メッセージ）の ID"),
    ),
    responses(
        (status = 204, description = "保存した"),
        (status = 400, description = "👍 に理由を付けた等"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "このスレッドの回答ではない"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn set_message_feedback(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, message_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<SetFeedbackRequest>,
) -> Result<StatusCode, ApiError> {
    let rating = match req.rating {
        FeedbackRating::Up => 1,
        FeedbackRating::Down => -1,
    };
    chat_store(&state)?
        .set_feedback(
            &ctx,
            id,
            message_id,
            rating,
            req.category.map(FeedbackCategory::as_str),
            trace.as_deref(),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 自分の評価を取り消す。
#[utoipa::path(
    delete, path = "/threads/{id}/messages/{message_id}/feedback",
    params(
        ("id" = Uuid, Path, description = "スレッド ID"),
        ("message_id" = Uuid, Path, description = "回答の ID"),
    ),
    responses(
        (status = 204, description = "取り消した（無ければ何もしない）"),
        (status = 403, description = "認可されていない"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn clear_message_feedback(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, message_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    chat_store(&state)?
        .clear_feedback(&ctx, id, message_id, trace.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_match_the_store_vocabulary() {
        let all = [
            FeedbackCategory::Inaccurate,
            FeedbackCategory::NotFound,
            FeedbackCategory::NotFollowed,
            FeedbackCategory::Slow,
            FeedbackCategory::HowTo,
            FeedbackCategory::Other,
        ];
        let names: Vec<&str> = all.iter().map(|c| c.as_str()).collect();
        assert_eq!(names, chat::FEEDBACK_CATEGORIES);
        for c in all {
            assert_eq!(FeedbackCategory::parse(c.as_str()), Some(c));
        }
    }
}

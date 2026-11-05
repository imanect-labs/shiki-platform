//! 回答の画像の代理取得ルート（#1044・PIT-77）。
//!
//! 回答の Markdown 画像をブラウザから外部へ直接読ませない。shiki-server が代わりに取得して返す。
//! - 認可はスレッドの viewer（`ChatStore::answer_image_source` が判定し、監査も残る）。
//! - **出所に無い URL は取得しない**（404）。出所はその回答で取得した Web ページの本文と Web の引用の
//!   抜粋で、判定はサーバ側で行う（`chat::answer_images`）。フロントの判定は見た目だけ。
//! - 取得するのは出典に書かれていた字句の URL（回答側の書き分けを通信に効かせない）。
//! - 取得は `web_fetch` と同じ関門（URL 検証・解決後 IP の検証とアドレス固定）を通る。
//!
//! 失敗の理由はログにだけ残す。クライアントには 404 / 502 だけを返し、フロントは「外部の画像」の
//! リンクに落とす。

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use utoipa::IntoParams;
use uuid::Uuid;

use sandbox_client::net_guard::SystemResolver;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

use super::chat::chat_store;
use super::chat_images_fetch::{fetch_image, FetchedImage, ImageFetchError};
use agent_core::tools::public_fetch::public_only;

/// 画像の URL（回答の Markdown に書かれていたもの）。
#[derive(Debug, Deserialize, IntoParams)]
pub struct AnswerImageQuery {
    /// 画像の URL。出所の判定は正規化した値で行い、取得には出典の字句を使う。
    pub u: String,
}

/// 受け付ける URL の長さの上限（出所側の上限と揃える）。
const MAX_URL_LEN: usize = 2048;

/// 回答に描く Web の画像を、shiki-server が代わりに取得して返す。
#[utoipa::path(
    get, path = "/threads/{id}/messages/{message_id}/image",
    params(
        ("id" = Uuid, Path, description = "スレッド ID"),
        ("message_id" = Uuid, Path, description = "回答のメッセージ ID"),
        AnswerImageQuery,
    ),
    responses(
        (status = 200, description = "画像（png / jpeg / gif / webp / avif）", content_type = "image/*"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "出所に無い・取得できない形式"),
        (status = 502, description = "取得先が失敗した"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn get_answer_image(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((thread_id, message_id)): Path<(Uuid, Uuid)>,
    Query(q): Query<AnswerImageQuery>,
) -> Result<Response, ApiError> {
    if q.u.is_empty() || q.u.len() > MAX_URL_LEN {
        return Err(ApiError::NotFound);
    }
    let Some(source) = chat_store(&state)?
        .answer_image_source(&ctx, thread_id, message_id, &q.u, trace.as_deref())
        .await?
    else {
        return Err(ApiError::NotFound);
    };

    match fetch_image(&SystemResolver, &source, public_only).await {
        Ok(image) => Ok(image_response(image)),
        Err(e) => {
            tracing::info!(target: "answer_image", %thread_id, %message_id, reason = e.reason(), "画像を代理取得できませんでした");
            Ok(failure_status(&e).into_response())
        }
    }
}

/// 取得の失敗をクライアントへの状態に写す（理由は返さない）。取得先の失敗は 502、
/// 取得しない・できない形式は 404。フロントはどちらも「外部の画像」のリンクに落とす。
fn failure_status(e: &ImageFetchError) -> StatusCode {
    match e {
        ImageFetchError::Upstream(_) | ImageFetchError::TooManyRedirects => StatusCode::BAD_GATEWAY,
        ImageFetchError::Rejected(_) | ImageFetchError::NotImage | ImageFetchError::TooLarge => {
            StatusCode::NOT_FOUND
        }
    }
}

/// 取得した画像の応答。型は中身から判定したもの。
fn image_response(image: FetchedImage) -> Response {
    let headers = [
        (header::CONTENT_TYPE, HeaderValue::from_static(image.mime)),
        // 判定した型以外として解釈させない・取得した中身にスクリプトや取得を許さない。
        (
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ),
        (
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("default-src 'none'; sandbox"),
        ),
        (
            header::CONTENT_DISPOSITION,
            HeaderValue::from_static("inline"),
        ),
        // 会話に紐づく取得なので共有キャッシュに載せない。ブラウザには 1 時間持たせる。
        (
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, max-age=3600"),
        ),
        (
            header::HeaderName::from_static("cross-origin-resource-policy"),
            HeaderValue::from_static("same-origin"),
        ),
    ];
    (headers, image.bytes).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_failures_are_502_and_refusals_are_404() {
        assert_eq!(
            failure_status(&ImageFetchError::Upstream("x".into())),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            failure_status(&ImageFetchError::TooManyRedirects),
            StatusCode::BAD_GATEWAY
        );
        for e in [
            ImageFetchError::Rejected("x".into()),
            ImageFetchError::NotImage,
            ImageFetchError::TooLarge,
        ] {
            assert_eq!(failure_status(&e), StatusCode::NOT_FOUND, "{e:?}");
        }
    }

    #[test]
    fn image_response_is_locked_down() {
        let res = image_response(FetchedImage {
            bytes: vec![0x89, b'P', b'N', b'G'],
            mime: "image/png",
        });
        assert_eq!(res.status(), StatusCode::OK);
        let h = res.headers();
        assert_eq!(h[header::CONTENT_TYPE], "image/png");
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(
            h[header::CONTENT_SECURITY_POLICY],
            "default-src 'none'; sandbox"
        );
        assert_eq!(h[header::CACHE_CONTROL], "private, max-age=3600");
        assert_eq!(h["cross-origin-resource-policy"], "same-origin");
    }
}

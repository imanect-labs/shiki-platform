//! `trace_id` extractor（監査ログ ↔ OTel トレース突合の継ぎ目）。
//!
//! [`telemetry::observe`](crate::telemetry::observe) がリクエスト span から解決した
//! trace_id を request extension に載せ、ハンドラはこの extractor で受け取って
//! StorageService の監査記録へ渡す（design §4.9: 早期に種を蒔く）。

use std::convert::Infallible;

use axum::{extract::FromRequestParts, http::request::Parts};

/// request extension で trace_id を運ぶ newtype（`observe` が挿入）。
#[derive(Debug, Clone)]
pub struct TraceId(pub String);

/// trace_id を取り出す extractor。未設定（OTel 無効時）は `None`。
pub struct TraceIdExt(pub Option<String>);

impl<S> FromRequestParts<S> for TraceIdExt
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(TraceIdExt(
            parts.extensions.get::<TraceId>().map(|t| t.0.clone()),
        ))
    }
}

impl TraceIdExt {
    /// `service` へ渡すための `Option<&str>`。
    pub fn as_deref(&self) -> Option<&str> {
        self.0.as_deref()
    }
}

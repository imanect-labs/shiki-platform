//! shiki-api — axum HTTP サーバ（API / SSE / OpenAPI）。
//!
//! Phase 0 では設定ローダ・ヘルスチェック・OIDC 認証・OpenFGA 認可・`GET /me`・
//! OTel 計装の縦の最小貫通を提供する。

pub mod config;
pub mod error;
pub mod extract;
pub mod health;
pub mod middleware;
pub mod openapi;
pub mod routes;
pub mod server;
pub mod state;
pub mod telemetry;

pub use server::build_router;
pub use state::AppState;

//! shiki-api — axum HTTP サーバ（API / SSE / OpenAPI）。
//!
//! Phase 0 では設定ローダ・ヘルスチェック・OIDC 認証・OpenFGA 認可・`GET /me`・
//! OTel 計装の縦の最小貫通を提供する。

// #[cfg(test)] のユニットテストは本番コードのみ厳格化する pedantic/安全系 lint を許容する。
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::print_stdout,
        clippy::print_stderr,
        clippy::pedantic,
        clippy::cognitive_complexity
    )
)]

pub mod config;
pub mod error;
pub mod extract;
pub mod health;
pub mod keycloak_admin;
pub mod middleware;
pub mod oidc;
pub mod openapi;
pub mod routes;
pub mod server;
pub mod session;
pub mod state;
pub mod telemetry;
pub mod workflow_runtime;

pub use server::build_router;
pub use state::AppState;

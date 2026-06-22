//! ヘルスチェック（docs/roadmap phase-0 Task 0.3）。
//!
//! - `/healthz`: liveness。依存に触れず常に 200。
//! - `/readyz`: readiness。Postgres 疎通を確認し、断時は 503。

use axum::{extract::State, http::StatusCode};

use crate::state::AppState;

/// liveness プローブ。
pub async fn healthz() -> StatusCode {
    StatusCode::OK
}

/// readiness プローブ。Postgres に `SELECT 1` を投げて疎通確認する。
pub async fn readyz(State(state): State<AppState>) -> StatusCode {
    match sqlx::query("SELECT 1").execute(&state.db).await {
        Ok(_) => StatusCode::OK,
        Err(err) => {
            tracing::warn!(error = %err, "readyz: Postgres 疎通に失敗");
            StatusCode::SERVICE_UNAVAILABLE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn healthz_always_ok() {
        // liveness は依存に触れず常に 200。
        assert_eq!(healthz().await, StatusCode::OK);
    }
}

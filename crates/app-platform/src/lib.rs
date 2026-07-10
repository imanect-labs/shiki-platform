//! ミニアプリ／業務アプリ基盤（Phase 9 Task 9.1 / 9.13a）。
//!
//! コードベース・ミニアプリ（B）のマニフェスト・語彙照合検証・汎用レジストリ（不変 publish）。
//! マニフェストは `artifact(kind=mini_app_code)` として A（宣言的）と同一の version＋ReBAC＋
//! 監査枠に乗る。要求スコープ/ツールは閉じた語彙（[`authz::CapabilityScope`] /
//! [`agent_core::ToolName`]）へ照合し、実在しない権限名を拒否する（ハルシネーション境界）。

mod manifest;
mod registry;
mod store;
mod validate;

pub use manifest::{
    Budget, CronEntry, FrontendBundle, ManifestTable, MiniAppManifest, ServerSpec, TrustTier,
};
pub use registry::{NewRegistryEntry, Registry, RegistryEntry};
pub use store::{manifest_digest, MiniAppCodeStore};
pub use validate::validate_manifest;

/// ミニアプリ基盤のエラー。
#[derive(Debug, thiserror::Error)]
pub enum AppPlatformError {
    #[error("対象が見つかりません")]
    NotFound,
    #[error("権限がありません")]
    Forbidden,
    #[error("不正な入力: {0}")]
    Invalid(String),
    #[error("競合しています: {0}")]
    Conflict(String),
    #[error("内部エラー: {0}")]
    Internal(String),
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn map_db(e: sqlx::Error) -> AppPlatformError {
    AppPlatformError::Internal(format!("db: {e}"))
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn map_artifact(e: artifact::ArtifactError) -> AppPlatformError {
    use artifact::ArtifactError as AE;
    match e {
        AE::NotFound => AppPlatformError::NotFound,
        AE::Forbidden => AppPlatformError::Forbidden,
        AE::Invalid(m) => AppPlatformError::Invalid(m),
        AE::Conflict(m) => AppPlatformError::Conflict(m),
        AE::Internal(m) => AppPlatformError::Internal(format!("artifact: {m}")),
    }
}

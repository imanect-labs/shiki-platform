//! ミニアプリ／業務アプリ基盤（Phase 9 Task 9.1 / 9.13a）。
//!
//! コードベース・ミニアプリ（B）のマニフェスト・語彙照合検証・汎用レジストリ（不変 publish）。
//! マニフェストは `artifact(kind=mini_app_code)` として A（宣言的）と同一の version＋ReBAC＋
//! 監査枠に乗る。要求スコープ/ツールは閉じた語彙（[`authz::CapabilityScope`] /
//! [`agent_core::ToolName`]）へ照合し、実在しない権限名を拒否する（ハルシネーション境界）。

mod bundle;
mod egress_guard;
mod functions;
mod install;
mod install_ops;
mod manifest;
mod registry;
mod sign;
mod store;
mod trusted_key;
mod validate;

pub use bundle::{BundleStore, MAX_BUNDLE_BYTES};
pub use functions::{
    egress_allowed, FunctionActor, FunctionInvocation, FunctionOutcome, FunctionRunner,
};
pub use install::{InstallRequest, InstallService, Installed};
pub use install_ops::next_cron_run_after;
pub use manifest::{
    Budget, CronEntry, FrontendBundle, ManifestTable, MiniAppManifest, ServerSpec, TrustTier,
};
pub use registry::{NewRegistryEntry, Registry, RegistryEntry};
pub use sign::{sign_manifest, verify_manifest_signature};
pub use store::{manifest_digest, MiniAppCodeStore};
pub use trusted_key::{TrustedKey, TrustedKeyStore};
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

impl From<data::DataError> for AppPlatformError {
    fn from(e: data::DataError) -> Self {
        use data::DataError;
        match e {
            DataError::NotFound => AppPlatformError::NotFound,
            DataError::Forbidden => AppPlatformError::Forbidden,
            DataError::Invalid(m) => AppPlatformError::Invalid(m),
            DataError::Conflict(m) => AppPlatformError::Conflict(m),
            DataError::Internal(m) => AppPlatformError::Internal(m),
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use artifact::ArtifactError as AE;

    #[test]
    fn artifact_errors_map_by_kind() {
        assert!(matches!(
            map_artifact(AE::NotFound),
            AppPlatformError::NotFound
        ));
        assert!(matches!(
            map_artifact(AE::Forbidden),
            AppPlatformError::Forbidden
        ));
        assert!(matches!(
            map_artifact(AE::Invalid("x".into())),
            AppPlatformError::Invalid(_)
        ));
        assert!(matches!(
            map_artifact(AE::Conflict("x".into())),
            AppPlatformError::Conflict(_)
        ));
        // Internal は原文脈を残してラップする。
        let mapped = map_artifact(AE::Internal("boom".into()));
        assert!(matches!(&mapped, AppPlatformError::Internal(m) if m.contains("boom")));
    }

    #[test]
    fn db_error_maps_to_internal() {
        let mapped = map_db(sqlx::Error::RowNotFound);
        assert!(matches!(mapped, AppPlatformError::Internal(_)));
        // ユーザー可読メッセージが付く。
        assert!(!mapped.to_string().is_empty());
    }
}

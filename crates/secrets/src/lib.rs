//! シークレット管理（Task 10.9・miniapp-platform.md §5）。
//!
//! 不変条件:
//! - **write-only / use-only**: 登録・ローテーション・削除・参照名一覧はできるが、
//!   **平文を読み返す API は存在しない**（[`SecretStore`] に read 系がない）。利用は
//!   実行時解決（[`SecretStore::resolve`]）のみで、解決イベントは毎回監査に残る。
//! - **envelope encryption**: 平文は DEK（データ暗号鍵）で AES-256-GCM 暗号化し、DEK は
//!   マスターキー（[`KeyProvider`]）で包んで保存する。平文も DEK 平文も DB に残らない。
//! - **宛先束縛**: 登録時に添付可能ホストを宣言し、http.request 実行時に fail-closed 強制
//!   （[`binding`]・PIT-36）。
//! - **ReBAC**: `secret:<tenant>|<id>` に owner / can_use。`can_use` を持つ実行主体のみ解決可。
//! - **レダクト**: 解決した平文の集合を [`redact`] でログ・run 履歴・エラーからマスクする
//!   （記録時実施・engine.md §11.3）。

mod binding;
mod crypto;
mod key_provider;
mod redact;
mod store;

pub use binding::{host_allowed, DestinationBinding};
pub use crypto::{CryptoError, KeyGuard};
pub use key_provider::{KeyProvider, LocalKeyFileProvider, WrappedKey};
pub use redact::Redactor;
pub use store::{NewSecret, ResolvedSecret, SecretMeta, SecretStore};

/// シークレット操作のエラー。
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("対象が見つかりません")]
    NotFound,
    #[error("権限がありません")]
    Forbidden,
    #[error("不正な入力: {0}")]
    Invalid(String),
    #[error("宛先が許可されていません: {0}")]
    DestinationDenied(String),
    #[error("暗号処理に失敗しました: {0}")]
    Crypto(#[from] CryptoError),
    #[error("競合しています: {0}")]
    Conflict(String),
    #[error("内部エラー: {0}")]
    Internal(String),
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn map_db(e: sqlx::Error) -> SecretError {
    SecretError::Internal(format!("db: {e}"))
}

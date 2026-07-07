//! 共有可能アーティファクト共通基盤（Task 6.1・Phase 10 Stage A で前倒し）。
//!
//! prompt template / UI スペック / ミニアプリ / **ワークフロー IR** / skill / script を
//! 統一的に扱う「バージョン付き・ReBAC 共有可能な JSON 本文」の共通枠。
//!
//! 不変条件:
//! - **不変バージョン追記方式**: 本文の変更は常に新バージョンの追記。過去バージョンは
//!   不変で取得できる（[`ArtifactStore::get_version`]）。
//! - **権限の正本は OpenFGA**: `artifact:<tenant>|<id>` に owner/editor/viewer
//!   （thread と同型の非階層共有）。全公開メソッドは `&AuthContext` を取り、
//!   内部で check ＋監査を行う（アンビエント権限の排除・design §1/§4）。
//! - **kind は閉じた集合**（[`ArtifactKind`]）: Task 6.7/6.10/10.1/10.11 が同じテーブル・
//!   同じ共有 API・同じ監査経路に乗る。

mod model;
mod share;
mod store;

pub use model::{Artifact, ArtifactKind, ArtifactRole, ArtifactVersion, VersionMeta};
pub use store::{ArtifactStore, NewArtifact};

/// アーティファクト操作のエラー。
#[derive(Debug, thiserror::Error)]
pub enum ArtifactError {
    #[error("対象が見つかりません")]
    NotFound,
    #[error("権限がありません")]
    Forbidden,
    #[error("不正な入力: {0}")]
    Invalid(String),
    /// 名前重複・バージョン競合（楽観ロック不一致）。
    #[error("競合しています: {0}")]
    Conflict(String),
    #[error("内部エラー: {0}")]
    Internal(String),
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn map_db(e: sqlx::Error) -> ArtifactError {
    ArtifactError::Internal(format!("db: {e}"))
}

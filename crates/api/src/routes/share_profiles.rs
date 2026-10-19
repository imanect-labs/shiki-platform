//! 共有一覧（アーティファクト・スレッド）の相手表示名の補完（#741）。
//!
//! ノード共有（`StorageService::list_shares`）と同じ [`storage::DirectoryStore`] の一括解決を使う
//! （テナント＋org スコープ・`= ANY($ids)` の 1 クエリ）。解決失敗は非致命で、名前なし
//! （UI の短縮 ID 表示）にフォールバックする。

use authz::AuthContext;
use storage::{ShareProfiles, ShareTarget};

use crate::state::AppState;

/// 共有相手群の表示名（user は email も）を引く。失敗時は空（全員 `None`）。
pub(crate) async fn resolve<'a>(
    state: &AppState,
    ctx: &AuthContext,
    targets: impl IntoIterator<Item = &'a ShareTarget>,
) -> ShareProfiles {
    state
        .directory
        .resolve_share_profiles(ctx, targets)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "共有相手の表示名解決に失敗（ID 表示へフォールバック）");
            ShareProfiles::default()
        })
}

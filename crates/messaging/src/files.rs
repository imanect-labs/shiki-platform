//! `file_ref` の解決（design §4.14 本文・Task 14.6）。
//!
//! 発言は node_id だけを持ち、表示のたびに **StorageService 経由で閲覧者の ReBAC を評価**する。
//! 読めないファイルは名前・サイズも返さない（「参照できない添付」）。共有後に文書の権限を絞れば、
//! 過去の発言からも見えなくなる。

use async_trait::async_trait;
use authz::AuthContext;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

/// 閲覧者が読めるファイルのメタデータ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMeta {
    pub name: String,
    pub size_bytes: Option<i64>,
    pub content_type: Option<String>,
}

/// ファイルの読取権限とメタデータの窓口（本番は [`StorageFiles`]・テストは差し替え）。
#[async_trait]
pub trait FileAccess: Send + Sync {
    /// `ids` のうち閲覧者が読めるもののメタデータ。読めない・無いものは含めない（fail-closed）。
    async fn readable(&self, ctx: &AuthContext, ids: &[Uuid]) -> HashMap<Uuid, FileMeta>;
}

/// StorageService（単一チョークポイント）経由の実装。
pub struct StorageFiles(pub Arc<storage::StorageService>);

#[async_trait]
impl FileAccess for StorageFiles {
    async fn readable(&self, ctx: &AuthContext, ids: &[Uuid]) -> HashMap<Uuid, FileMeta> {
        // まとめて 1 回で判定する（添付ごとの往復・監査を作らない）。失敗は「読めない」に倒す。
        match self
            .0
            .readable_files_metadata(ctx, ids, "messaging.file_ref")
            .await
        {
            Ok(nodes) => nodes
                .into_iter()
                .map(|n| {
                    (
                        n.id,
                        FileMeta {
                            name: n.name,
                            size_bytes: n.size_bytes,
                            content_type: n.content_type,
                        },
                    )
                })
                .collect(),
            Err(e) => {
                tracing::warn!(error = %e, "messaging: 添付の権限を評価できません（参照できないとして扱う）");
                HashMap::new()
            }
        }
    }
}

/// 何も読めない実装（ストレージを持たない構成・テスト）。
pub struct NoFiles;

#[async_trait]
impl FileAccess for NoFiles {
    async fn readable(&self, _ctx: &AuthContext, _ids: &[Uuid]) -> HashMap<Uuid, FileMeta> {
        HashMap::new()
    }
}

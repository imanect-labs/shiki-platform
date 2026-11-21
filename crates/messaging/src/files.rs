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

/// `readable_files_metadata` が 1 回で扱う件数（呼び出し側で分ける上限）。
const READ_BATCH: usize = 2_000;

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
        // 一括判定の上限ごとに分けて判定する（添付ごとの往復・監査を作らない）。失敗した塊は「読めない」に倒す。
        let mut out = HashMap::new();
        for chunk in ids.chunks(READ_BATCH) {
            match self
                .0
                .readable_files_metadata(ctx, chunk, "messaging.file_ref")
                .await
            {
                Ok(nodes) => out.extend(nodes.into_iter().map(|n| {
                    (
                        n.id,
                        FileMeta {
                            name: n.name,
                            size_bytes: n.size_bytes,
                            content_type: n.content_type,
                        },
                    )
                })),
                Err(e) => {
                    tracing::warn!(error = %e, "messaging: 添付の権限を評価できません（参照できないとして扱う）");
                }
            }
        }
        out
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

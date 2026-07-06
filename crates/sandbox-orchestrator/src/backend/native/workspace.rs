//! ホスト側 `/workspace` の実体ディレクトリに対するファイル操作（gVisor は host bind mount で共有）。
//!
//! ゲストパス（`/workspace/...`）を `validate::normalize_workspace_path` で正規化してからホストパスへ
//! 写像し、シンボリックリンク経由の脱出も canonicalize で再確認する（PIT-23）。

use std::path::{Path, PathBuf};

use sandbox_client::{DirEntry, SandboxError};

use crate::validate::{self, MAX_DIR_ENTRIES};

/// per-sandbox のホスト側ワークスペース（`<state>/workspace`）。
#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    /// ディレクトリを作成して開く。
    pub fn create(root: &Path) -> Result<Workspace, SandboxError> {
        std::fs::create_dir_all(root)
            .map_err(|e| SandboxError::Internal(format!("create workspace: {e}")))?;
        let root = std::fs::canonicalize(root)
            .map_err(|e| SandboxError::Internal(format!("canonicalize workspace: {e}")))?;
        Ok(Workspace { root })
    }

    /// ゲスト側マウントパス用のルート（`config.json` の bind source）。
    #[must_use]
    pub fn host_root(&self) -> &Path {
        &self.root
    }

    /// ゲストパスをホストパスへ写像する（正規化＋ルート閉じ込め）。
    fn resolve(&self, guest_path: &str) -> Result<PathBuf, SandboxError> {
        let norm = validate::normalize_workspace_path(guest_path)
            .map_err(|e| SandboxError::Invalid(e.to_string()))?;
        // norm は "/workspace/..." 形式。接頭辞を剥がしてルート配下へ。
        let rel = norm.strip_prefix("/workspace").unwrap_or("");
        let rel = rel.strip_prefix('/').unwrap_or(rel);
        let joined = if rel.is_empty() {
            self.root.clone()
        } else {
            self.root.join(rel)
        };
        Ok(joined)
    }

    /// 書込先が確実にルート配下か（シンボリックリンク脱出を canonicalize で弾く）。
    ///
    /// 対象自体は未作成のことがあるため、存在する最も近い祖先を canonicalize して判定する。
    fn guard_under_root(&self, path: &Path) -> Result<(), SandboxError> {
        let mut probe = path;
        loop {
            match probe.canonicalize() {
                Ok(real) => {
                    if real == self.root || real.starts_with(&self.root) {
                        return Ok(());
                    }
                    return Err(SandboxError::Invalid("path escapes workspace".into()));
                }
                Err(_) => match probe.parent() {
                    Some(p) => probe = p,
                    None => return Ok(()),
                },
            }
        }
    }

    /// ファイルを書き込む（親ディレクトリは作成）。
    pub async fn put(&self, guest_path: &str, bytes: Vec<u8>) -> Result<(), SandboxError> {
        validate::check_file_size(bytes.len()).map_err(|e| SandboxError::Invalid(e.to_string()))?;
        let path = self.resolve(guest_path)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| SandboxError::Internal(format!("mkdir: {e}")))?;
        }
        self.guard_under_root(&path)?;
        tokio::fs::write(&path, bytes)
            .await
            .map_err(|e| SandboxError::Internal(format!("write: {e}")))
    }

    /// ファイルを読み出す。
    pub async fn get(&self, guest_path: &str) -> Result<Vec<u8>, SandboxError> {
        let path = self.resolve(guest_path)?;
        self.guard_under_root(&path)?;
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|_| SandboxError::NotFound(guest_path.to_string()))?;
        validate::check_file_size(bytes.len()).map_err(|e| SandboxError::Invalid(e.to_string()))?;
        Ok(bytes)
    }

    /// ディレクトリ一覧（成果物差分検出用・上限で打ち切り）。
    pub async fn list(&self, guest_path: &str) -> Result<Vec<DirEntry>, SandboxError> {
        let path = self.resolve(guest_path)?;
        self.guard_under_root(&path)?;
        let mut rd = tokio::fs::read_dir(&path)
            .await
            .map_err(|_| SandboxError::NotFound(guest_path.to_string()))?;
        let mut entries = Vec::new();
        while let Some(ent) = rd
            .next_entry()
            .await
            .map_err(|e| SandboxError::Internal(format!("readdir: {e}")))?
        {
            let Ok(meta) = ent.metadata().await else {
                continue;
            };
            entries.push(DirEntry {
                name: ent.file_name().to_string_lossy().into_owned(),
                is_dir: meta.is_dir(),
                size: meta.len(),
            });
            if entries.len() >= MAX_DIR_ENTRIES {
                break;
            }
        }
        Ok(entries)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn tmp_ws() -> Workspace {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!("ws-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        Workspace::create(&base).expect("create ws")
    }

    #[tokio::test]
    async fn put_get_roundtrip() {
        let ws = tmp_ws();
        ws.put("/workspace/out/x.txt", b"hello".to_vec())
            .await
            .unwrap();
        let got = ws.get("out/x.txt").await.unwrap();
        assert_eq!(got, b"hello");
    }

    #[tokio::test]
    async fn list_shows_entries() {
        let ws = tmp_ws();
        ws.put("a.txt", b"1".to_vec()).await.unwrap();
        ws.put("b.txt", b"22".to_vec()).await.unwrap();
        let mut names: Vec<String> = ws
            .list("/workspace")
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        names.sort();
        assert_eq!(names, vec!["a.txt".to_string(), "b.txt".to_string()]);
    }

    #[tokio::test]
    async fn rejects_traversal() {
        let ws = tmp_ws();
        assert!(ws.put("../escape.txt", b"x".to_vec()).await.is_err());
        assert!(ws.get("/etc/passwd").await.is_err());
    }

    #[tokio::test]
    async fn missing_file_is_not_found() {
        let ws = tmp_ws();
        assert!(matches!(
            ws.get("nope.txt").await,
            Err(SandboxError::NotFound(_))
        ));
    }
}

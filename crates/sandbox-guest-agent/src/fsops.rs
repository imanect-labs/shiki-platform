//! ゲスト内ファイル操作（`/workspace` 配下限定）。ホスト由来パスは正規化してルート外を弾く。

use base64::Engine;
use shiki_sandbox_agent_proto::{DirEntryDto, Event};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;
const WORKSPACE: &str = "/workspace";

/// ゲストパスを `/workspace` 配下に閉じ込めて実パスへ（`..`/絶対を弾く）。
fn resolve(path: &str) -> Result<std::path::PathBuf, String> {
    if path.contains('\0') {
        return Err("path contains NUL".into());
    }
    let rel = path.strip_prefix('/').unwrap_or(path);
    let rel = rel.strip_prefix("workspace/").unwrap_or(rel);
    let rel = if rel == "workspace" { "" } else { rel };
    let mut out = std::path::PathBuf::from(WORKSPACE);
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => return Err("path escapes workspace".into()),
            s => out.push(s),
        }
    }
    Ok(out)
}

/// base64 の内容をファイルに書く。
pub(crate) fn write_file(path: &str, b64: &str) -> Event {
    let bytes = match B64.decode(b64.as_bytes()) {
        Ok(b) => b,
        Err(e) => {
            return Event::Err {
                msg: format!("base64: {e}"),
            }
        }
    };
    let dest = match resolve(path) {
        Ok(p) => p,
        Err(e) => return Event::Err { msg: e },
    };
    if let Some(parent) = dest.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return Event::Err {
                msg: format!("mkdir: {e}"),
            };
        }
    }
    match std::fs::write(&dest, bytes) {
        Ok(()) => Event::Ok,
        Err(e) => Event::Err {
            msg: format!("write: {e}"),
        },
    }
}

/// ファイルを読み base64 で返す。
pub(crate) fn read_file(path: &str) -> Event {
    let src = match resolve(path) {
        Ok(p) => p,
        Err(e) => return Event::Err { msg: e },
    };
    match std::fs::read(&src) {
        Ok(bytes) => Event::File {
            b64: B64.encode(&bytes),
        },
        Err(e) => Event::Err {
            msg: format!("read: {e}"),
        },
    }
}

/// ディレクトリを一覧する。
pub(crate) fn list_dir(path: &str) -> Event {
    let dir = match resolve(path) {
        Ok(p) => p,
        Err(e) => return Event::Err { msg: e },
    };
    let rd = match std::fs::read_dir(&dir) {
        Ok(r) => r,
        Err(e) => {
            return Event::Err {
                msg: format!("readdir: {e}"),
            }
        }
    };
    let mut entries = Vec::new();
    for ent in rd.flatten() {
        let Ok(meta) = ent.metadata() else { continue };
        entries.push(DirEntryDto {
            name: ent.file_name().to_string_lossy().into_owned(),
            is_dir: meta.is_dir(),
            size: meta.len(),
        });
        if entries.len() >= 256 {
            break;
        }
    }
    Event::Dir { entries }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn resolve_confines_to_workspace() {
        assert_eq!(
            resolve("a.txt").unwrap(),
            std::path::Path::new("/workspace/a.txt")
        );
        assert_eq!(
            resolve("/workspace/x/y").unwrap(),
            std::path::Path::new("/workspace/x/y")
        );
        assert!(resolve("../etc/passwd").is_err());
        assert!(resolve("/workspace/../etc").is_err());
        assert!(resolve("a\0b").is_err());
    }
}

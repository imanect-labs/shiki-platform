//! ゲストコマンドパッケージ（software）の解決（Task 4.12 software・PIT-23/33）。
//!
//! `SandboxSpec.software` の名前を、orchestrator にステージ済みのパッケージディレクトリ
//! （`SANDBOX__SOFTWARE_DIR` 配下・`scripts/build-sandbox-commands.sh` が生成する
//! `<name>/package.tar`）へ写像する。名前はクライアント由来＝敵対的として扱い、
//! パス文字を含む名前・未知の名前は fail-closed で拒否する（実行時ダウンロードはしない）。

use std::path::Path;

use sandbox_client::SandboxError;
use secure_exec_client::wire;

/// 1 spec あたりの software 上限（ゲスト側 mount 数と起動コストの暴走防止）。
const MAX_SOFTWARE: usize = 16;

/// software 名の検証。パッケージはディレクトリ名として解決するため、パス区切り・`..`・
/// 隠しファイル・非 ASCII を拒否する（`coreutils` / `curl` / `ripgrep` 等の小文字英数のみ）。
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with(['.', '-'])
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// spec の software 名をステージ済みパッケージ（`dir` 記述子）へ解決する。
///
/// - `names` が空なら空を返す（パッケージ無しで VM を作る・追加コスト無し）。
/// - `software_dir` 未設定でパッケージが要求されたら Unimplemented（このデプロイには同梱が無い）。
/// - パッケージは `<software_dir>/<name>/package.tar` が存在するもののみ有効
///   （sidecar は tar 投影のみサポート・ディレクトリ投影は不可）。
pub fn resolve_software(
    software_dir: Option<&Path>,
    names: &[String],
) -> Result<Vec<wire::PackageDescriptor>, SandboxError> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    if names.len() > MAX_SOFTWARE {
        return Err(SandboxError::Invalid(format!(
            "software が多すぎます（最大 {MAX_SOFTWARE} 個）"
        )));
    }
    let Some(dir) = software_dir else {
        return Err(SandboxError::Unimplemented(
            "guest command packages are not installed on this orchestrator".into(),
        ));
    };
    let mut packages = Vec::with_capacity(names.len());
    for name in names {
        if !valid_name(name) {
            return Err(SandboxError::Invalid(format!(
                "software 名が不正です: {name:?}"
            )));
        }
        let pkg_dir = dir.join(name);
        if !pkg_dir.join("package.tar").is_file() {
            return Err(SandboxError::Invalid(format!(
                "software が見つかりません: {name}"
            )));
        }
        packages.push(wire::PackageDescriptor {
            dir: Some(pkg_dir.to_string_lossy().into_owned()),
            tar: None,
        });
    }
    Ok(packages)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn stage(dir: &Path, name: &str) {
        let pkg = dir.join(name);
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("package.tar"), b"tar").unwrap();
    }

    #[test]
    fn empty_names_resolve_to_no_packages() {
        assert!(resolve_software(None, &[]).unwrap().is_empty());
    }

    #[test]
    fn missing_software_dir_is_unimplemented() {
        let err = resolve_software(None, &["coreutils".into()]).unwrap_err();
        assert!(matches!(err, SandboxError::Unimplemented(_)));
    }

    #[test]
    fn resolves_staged_package() {
        let tmp = std::env::temp_dir().join(format!("sw-{}", uuid::Uuid::new_v4()));
        stage(&tmp, "coreutils");
        let pkgs = resolve_software(Some(&tmp), &["coreutils".into()]).unwrap();
        assert_eq!(pkgs.len(), 1);
        assert!(pkgs[0].dir.as_deref().unwrap().ends_with("coreutils"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn rejects_unknown_and_hostile_names() {
        let tmp = std::env::temp_dir().join(format!("sw-{}", uuid::Uuid::new_v4()));
        stage(&tmp, "coreutils");
        // 未知パッケージ（ステージ無し）。
        assert!(matches!(
            resolve_software(Some(&tmp), &["curl".into()]).unwrap_err(),
            SandboxError::Invalid(_)
        ));
        // パストラバーサル/パス区切り/隠し名/大文字（PIT-23）。
        for bad in ["../etc", "a/b", ".hidden", "-flag", "CoreUtils", ""] {
            assert!(
                matches!(
                    resolve_software(Some(&tmp), &[bad.to_string()]).unwrap_err(),
                    SandboxError::Invalid(_)
                ),
                "should reject {bad:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn rejects_too_many_packages() {
        let names: Vec<String> = (0..17).map(|i| format!("pkg{i}")).collect();
        let tmp = std::env::temp_dir().join(format!("sw-{}", uuid::Uuid::new_v4()));
        assert!(matches!(
            resolve_software(Some(&tmp), &names).unwrap_err(),
            SandboxError::Invalid(_)
        ));
    }
}

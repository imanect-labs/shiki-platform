//! PIT-23 の中核: サンドボックス/クライアント由来の全入力の単一検証点。
//!
//! ゲスト（信頼できない側）が供給するパス・サイズ・コマンドを orchestrator（特権側）が解釈する前に
//! ここで弾く。パストラバーサル・巨大ペイロード・不正文字を止める。プロパティテスト＋fuzz の対象。

/// 検証エラー（呼び出し側 InvalidArgument に写像）。
#[derive(Debug, PartialEq, Eq)]
pub struct ValidateError(pub String);

impl std::fmt::Display for ValidateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// ゲスト仮想FS のルート。全パスはこの下に閉じ込める。
pub const WORKSPACE_ROOT: &str = "/workspace";

/// 1 ファイルの最大バイト数（put/get）。
pub const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
/// list_dir が返す最大エントリ数（成果物回収の暴走防止）。
pub const MAX_DIR_ENTRIES: usize = 20;
/// stdout/stderr の累積上限（超過は打ち切り）。
pub const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
/// シェルコマンド文字列の最大長。
pub const MAX_SHELL_LEN: usize = 64 * 1024;
/// Python コードの最大長。
pub const MAX_CODE_LEN: usize = 1024 * 1024;

fn err(msg: impl Into<String>) -> ValidateError {
    ValidateError(msg.into())
}

/// ゲストパスを正規化・検証する。`/workspace` 配下の正規パスだけを許す。
///
/// - NUL・制御文字を拒否
/// - 長さ上限
/// - `..` によるトラバーサルを解決後に拒否（`/workspace` を出ようとしたら拒否）
/// - 絶対/相対いずれも `/workspace` 起点に解決
pub fn normalize_workspace_path(input: &str) -> Result<String, ValidateError> {
    if input.is_empty() {
        return Err(err("empty path"));
    }
    if input.len() > 4096 {
        return Err(err("path too long"));
    }
    if input.contains('\0') {
        return Err(err("path contains NUL"));
    }
    if input.chars().any(char::is_control) {
        return Err(err("path contains control character"));
    }

    // 相対パスは /workspace 起点。絶対パスは /workspace 配下のみ許可。
    let rooted = if let Some(rest) = input.strip_prefix('/') {
        if rest == "workspace" || rest.starts_with("workspace/") {
            input.to_string()
        } else {
            return Err(err("absolute path must be under /workspace"));
        }
    } else {
        format!("{WORKSPACE_ROOT}/{input}")
    };

    // セグメント単位で `.`/`..` を解決する（記号リンクは扱わない＝ゲスト仮想FS 側の責務）。
    let mut segments: Vec<&str> = Vec::new();
    for seg in rooted.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                // /workspace の上には出さない。
                if segments.len() <= 1 {
                    return Err(err("path escapes /workspace"));
                }
                segments.pop();
            }
            s => segments.push(s),
        }
    }
    // segments[0] は "workspace" のはず。
    if segments.first() != Some(&"workspace") {
        return Err(err("path escapes /workspace"));
    }
    Ok(format!("/{}", segments.join("/")))
}

/// put_file のバイト列サイズを検証する。
pub fn check_file_size(len: usize) -> Result<(), ValidateError> {
    if len > MAX_FILE_BYTES {
        return Err(err(format!(
            "file too large: {len} bytes (max {MAX_FILE_BYTES})"
        )));
    }
    Ok(())
}

/// Python コード長を検証する。
pub fn check_code(code: &str) -> Result<(), ValidateError> {
    if code.len() > MAX_CODE_LEN {
        return Err(err("python code too long"));
    }
    Ok(())
}

/// シェルコマンド長を検証する。
pub fn check_shell(cmd: &str) -> Result<(), ValidateError> {
    if cmd.is_empty() {
        return Err(err("empty shell command"));
    }
    if cmd.len() > MAX_SHELL_LEN {
        return Err(err("shell command too long"));
    }
    if cmd.contains('\0') {
        return Err(err("shell command contains NUL"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_relative() {
        assert_eq!(
            normalize_workspace_path("main.py").expect("ok"),
            "/workspace/main.py"
        );
        assert_eq!(
            normalize_workspace_path("a/./b.txt").expect("ok"),
            "/workspace/a/b.txt"
        );
    }

    #[test]
    fn accepts_workspace_absolute() {
        assert_eq!(
            normalize_workspace_path("/workspace/out/x.png").expect("ok"),
            "/workspace/out/x.png"
        );
    }

    #[test]
    fn rejects_traversal() {
        assert!(normalize_workspace_path("../etc/passwd").is_err());
        assert!(normalize_workspace_path("/workspace/../etc/passwd").is_err());
        assert!(normalize_workspace_path("a/../../b").is_err());
        assert!(normalize_workspace_path("/etc/passwd").is_err());
    }

    #[test]
    fn rejects_bad_chars() {
        assert!(normalize_workspace_path("a\0b").is_err());
        assert!(normalize_workspace_path("a\nb").is_err());
        assert!(normalize_workspace_path("").is_err());
    }

    #[test]
    fn size_and_shell_guards() {
        assert!(check_file_size(MAX_FILE_BYTES).is_ok());
        assert!(check_file_size(MAX_FILE_BYTES + 1).is_err());
        assert!(check_shell("ls -la").is_ok());
        assert!(check_shell("").is_err());
        assert!(check_shell("a\0b").is_err());
    }
}

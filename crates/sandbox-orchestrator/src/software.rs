//! ゲストコマンドスイート（software）の解決とマウント記述子生成（Task 4.12 software・PIT-23/33）。
//!
//! ゲストコマンド（ls/cat/grep/curl 等の wasm バイナリ）は、sidecar が認識する **コマンドルート**
//! `/__secure_exec/commands/0` に `host_dir` プラグインで read-only マウントする。sidecar はこの
//! ディレクトリを `$PATH` に載せ、内部の wasm を kernel 管理 stdio で実行する（＝出力が
//! `ProcessOutputEvent` に surface する経路。package.tar 投影ではこの stdio 配線が働かない）。
//!
//! `SandboxSpec.software` はクライアント由来＝敵対的として扱い（PIT-23）、名前検証で不正
//! （パス区切り・`..` 等）を弾く。名前は「意図/監査」用で、実際にマウントされるのは
//! `commands_dir` のコマンド集合全体（アルファは per-command 粒度を持たない・ポストアルファで細分化）。
//! `commands_dir` 未設定で software 要求が来たら fail-closed（このデプロイに同梱が無い・実行時 DL 禁止）。

use std::path::Path;

use sandbox_client::SandboxError;
use secure_exec_client::wire;

/// sidecar が `$PATH` に載せるコマンドルート（upstream 既定の番号付きパス）。
const COMMAND_MOUNT_PATH: &str = "/__secure_exec/commands/0";

/// 1 spec あたりの software 名の上限（暴走ガード）。
const MAX_SOFTWARE: usize = 32;

/// software 名の検証。監査/意図表現用のため、パス区切り・`..`・隠し名・非 ASCII を拒否する。
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with(['.', '-'])
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// spec の software 要求を、コマンドルートの `host_dir` マウント記述子へ解決する。
///
/// - `names` が空なら `None`（コマンド無しで VM を作る・追加コスト無し）。
/// - `commands_dir` 未設定で要求ありなら `Unimplemented`（同梱無し）。
/// - 名前が不正なら `Invalid`（fail-closed・PIT-23）。
pub fn resolve_command_mount(
    commands_dir: Option<&Path>,
    names: &[String],
) -> Result<Option<wire::MountDescriptor>, SandboxError> {
    if names.is_empty() {
        return Ok(None);
    }
    if names.len() > MAX_SOFTWARE {
        return Err(SandboxError::Invalid(format!(
            "software が多すぎます（最大 {MAX_SOFTWARE} 個）"
        )));
    }
    for name in names {
        if !valid_name(name) {
            return Err(SandboxError::Invalid(format!(
                "software 名が不正です: {name:?}"
            )));
        }
    }
    let Some(dir) = commands_dir else {
        return Err(SandboxError::Unimplemented(
            "guest command suite is not installed on this orchestrator".into(),
        ));
    };
    if !dir.is_dir() {
        return Err(SandboxError::Unimplemented(
            "guest command directory does not exist".into(),
        ));
    }
    let host_path = dir.to_string_lossy().into_owned();
    let config = serde_json::json!({ "hostPath": host_path, "readOnly": true }).to_string();
    Ok(Some(wire::MountDescriptor {
        guest_path: COMMAND_MOUNT_PATH.to_string(),
        read_only: true,
        plugin: wire::MountPluginDescriptor {
            id: "host_dir".to_string(),
            config,
        },
    }))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn empty_names_resolve_to_no_mount() {
        assert!(resolve_command_mount(None, &[]).unwrap().is_none());
    }

    #[test]
    fn missing_commands_dir_is_unimplemented() {
        let err = resolve_command_mount(None, &["coreutils".into()]).unwrap_err();
        assert!(matches!(err, SandboxError::Unimplemented(_)));
    }

    #[test]
    fn resolves_host_dir_mount() {
        let tmp = std::env::temp_dir();
        let mount = resolve_command_mount(Some(&tmp), &["coreutils".into()])
            .unwrap()
            .expect("mount");
        assert_eq!(mount.guest_path, COMMAND_MOUNT_PATH);
        assert_eq!(mount.plugin.id, "host_dir");
        assert!(mount.read_only);
        assert!(mount.plugin.config.contains("hostPath"));
    }

    #[test]
    fn rejects_hostile_names() {
        let tmp = std::env::temp_dir();
        for bad in ["../etc", "a/b", ".hidden", "-flag", "CoreUtils", ""] {
            assert!(
                matches!(
                    resolve_command_mount(Some(&tmp), &[bad.to_string()]).unwrap_err(),
                    SandboxError::Invalid(_)
                ),
                "should reject {bad:?}"
            );
        }
    }

    #[test]
    fn rejects_too_many() {
        let names: Vec<String> = (0..33).map(|i| format!("pkg{i}")).collect();
        assert!(matches!(
            resolve_command_mount(Some(&std::env::temp_dir()), &names).unwrap_err(),
            SandboxError::Invalid(_)
        ));
    }
}

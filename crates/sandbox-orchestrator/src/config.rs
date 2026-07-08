//! `SandboxSpec` → secure-exec `CreateVmConfig` の写像（limits・egress permissions・DNS）。
//!
//! egress は静的＋動的 allowlist を `PatternPermissionScope::Rules` に落とす。allowlist 空＝default Deny で
//! 全遮断。deny_overlay（管理者）は Deny ルールとして allow より優先で置く。完全一致/明示ワイルドカードのみ
//! 生成し、部分文字列マッチは作らない（PIT-36 の教訓）。

use sandbox_client::{Egress, EgressRule, SandboxLimits, SandboxSpec};
use secure_exec_vm_config::{
    CreateVmConfig, FsPermissionScope, JsRuntimeLimitsConfig, PatternPermissionRule,
    PatternPermissionRuleSet, PatternPermissionScope, PermissionMode, PermissionsPolicy,
    PythonLimitsConfig, ResourceLimitsConfig, RootFilesystemConfig, RootFilesystemMode,
    VmDnsConfig, VmLimitsConfig,
};

/// orchestrator のランタイム設定（DNS リゾルバ等）。
#[derive(Debug, Clone)]
pub struct OrchestratorEnv {
    /// egress 許可時に guest へ渡す DNS リゾルバ（allowlist 空なら DNS も渋る）。
    pub egress_resolvers: Vec<String>,
    /// ゲストコマンド（wasm バイナリ）のフラットなディレクトリ。`/__secure_exec/commands/0` に
    /// host_dir マウントされ `$PATH` に載る。未設定なら software 要求を Unimplemented で拒否する
    /// （実行時ダウンロード禁止・PIT-33）。
    pub commands_dir: Option<std::path::PathBuf>,
}

impl Default for OrchestratorEnv {
    fn default() -> Self {
        OrchestratorEnv {
            egress_resolvers: vec!["1.1.1.1".to_string(), "8.8.8.8".to_string()],
            commands_dir: None,
        }
    }
}

/// spec のリソース上限を secure-exec の VmLimitsConfig に落とす。
pub fn map_limits(l: &SandboxLimits) -> VmLimitsConfig {
    let resources = ResourceLimitsConfig {
        cpu_count: Some(1),
        max_processes: Some(u64::from(l.max_processes)),
        max_open_fds: Some(u64::from(l.max_open_fds)),
        max_filesystem_bytes: Some(l.max_fs_bytes),
        max_wasm_memory_bytes: Some(l.memory_mb.saturating_mul(1024 * 1024)),
        ..Default::default()
    };
    let js_runtime = JsRuntimeLimitsConfig {
        v8_heap_limit_mb: Some(l.memory_mb),
        wall_clock_limit_ms: Some(l.wall_clock_ms),
        cpu_time_limit_ms: Some(l.wall_clock_ms),
        captured_output_limit_bytes: Some(l.max_output_bytes),
        ..Default::default()
    };
    let python = PythonLimitsConfig {
        execution_timeout_ms: Some(l.exec_timeout_ms),
        max_old_space_mb: Some(l.memory_mb),
        output_buffer_max_bytes: Some(l.max_output_bytes),
        ..Default::default()
    };
    VmLimitsConfig {
        resources: Some(resources),
        js_runtime: Some(js_runtime),
        python: Some(python),
        ..Default::default()
    }
}

/// 1 つの egress ルールを sidecar のパターン群に展開する（完全一致/明示ワイルドカードのみ）。
fn rule_patterns(rule: &EgressRule) -> Vec<String> {
    if rule.port == 0 {
        // 全ポート: ホスト名そのものと host:* を許す。
        vec![
            rule.host_pattern.clone(),
            format!("{}:*", rule.host_pattern),
        ]
    } else {
        vec![format!("{}:{}", rule.host_pattern, rule.port)]
    }
}

/// egress ポリシを network permission scope に落とす。allowlist 空＝default Deny 全遮断。
pub fn map_egress(egress: &Egress) -> PatternPermissionScope {
    let mut rules: Vec<PatternPermissionRule> = Vec::new();

    // 管理者拒否リストを先頭に（Deny を allow より優先）。
    if !egress.deny_overlay.is_empty() {
        let patterns = egress.deny_overlay.iter().flat_map(rule_patterns).collect();
        rules.push(PatternPermissionRule {
            mode: PermissionMode::Deny,
            operations: Vec::new(),
            patterns,
        });
    }

    // 静的＋動的 allow（run 限定の dynamic_allow も同格で許可）。
    let allow_patterns: Vec<String> = egress
        .static_allow
        .iter()
        .chain(egress.dynamic_allow.iter())
        .flat_map(rule_patterns)
        .collect();
    if !allow_patterns.is_empty() {
        rules.push(PatternPermissionRule {
            mode: PermissionMode::Allow,
            operations: Vec::new(),
            patterns: allow_patterns,
        });
    }

    PatternPermissionScope::Rules(PatternPermissionRuleSet {
        default: Some(PermissionMode::Deny),
        rules,
    })
}

/// spec 全体を CreateVmConfig に落とす。root fs は Ephemeral（まっさら短命）。
pub fn spec_to_vm_config(spec: &SandboxSpec, env: &OrchestratorEnv) -> CreateVmConfig {
    let has_egress = !spec.egress.static_allow.is_empty() || !spec.egress.dynamic_allow.is_empty();
    // ゲスト内（ephemeral 仮想FS）は full 許可。隔離境界は VM そのもの（プロセス分離＋wasm）であり、
    // intra-guest の fs/process を絞る必要はない。network だけ egress ルールで制御し、binding（listen）は
    // 既定 deny（サーバ待ち受け不可）に寄せる。
    let allow = PatternPermissionScope::Mode(PermissionMode::Allow);
    let permissions = PermissionsPolicy {
        fs: Some(FsPermissionScope::Mode(PermissionMode::Allow)),
        network: Some(map_egress(&spec.egress)),
        child_process: Some(allow.clone()),
        process: Some(allow.clone()),
        env: Some(allow),
        binding: None,
    };
    // DNS: allowlist が空なら解決経路ごと閉じる（名前解決自体が egress）。
    let dns = if has_egress {
        Some(VmDnsConfig {
            name_servers: env.egress_resolvers.clone(),
            overrides: std::collections::BTreeMap::new(),
        })
    } else {
        None
    };
    CreateVmConfig {
        // cwd は未指定にする。sidecar の既定ゲスト cwd が /workspace（bootstrap 済み）。
        // 絶対パスを渡すとホスト側パスとして mkdir され Permission denied になる。
        cwd: None,
        root_filesystem: RootFilesystemConfig {
            mode: RootFilesystemMode::Ephemeral,
            ..Default::default()
        },
        permissions: Some(permissions),
        limits: Some(map_limits(&spec.limits)),
        dns,
        ..Default::default()
    }
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;
    use sandbox_client::SandboxLimits;

    #[test]
    fn empty_egress_is_default_deny() {
        let scope = map_egress(&Egress::blocked());
        match scope {
            PatternPermissionScope::Rules(rs) => {
                assert_eq!(rs.default, Some(PermissionMode::Deny));
                assert!(rs.rules.is_empty(), "no allow rules when blocked");
            }
            PatternPermissionScope::Mode(_) => panic!("expected rules"),
        }
    }

    #[test]
    fn allow_rules_generated() {
        let egress = Egress {
            static_allow: vec![EgressRule {
                host_pattern: "api.example.com".into(),
                port: 443,
            }],
            ..Egress::blocked()
        };
        let PatternPermissionScope::Rules(rs) = map_egress(&egress) else {
            panic!("expected rules");
        };
        assert_eq!(rs.default, Some(PermissionMode::Deny));
        let allow = rs
            .rules
            .iter()
            .find(|r| r.mode == PermissionMode::Allow)
            .expect("allow rule");
        assert!(allow.patterns.contains(&"api.example.com:443".to_string()));
    }

    #[test]
    fn deny_overlay_precedes_allow() {
        let egress = Egress {
            static_allow: vec![EgressRule {
                host_pattern: "ok.example.com".into(),
                port: 0,
            }],
            deny_overlay: vec![EgressRule {
                host_pattern: "evil.example.com".into(),
                port: 0,
            }],
            ..Egress::blocked()
        };
        let PatternPermissionScope::Rules(rs) = map_egress(&egress) else {
            panic!("expected rules");
        };
        assert_eq!(rs.rules.first().map(|r| r.mode), Some(PermissionMode::Deny));
    }

    #[test]
    fn limits_mapped() {
        let l = SandboxLimits::constrained();
        let cfg = map_limits(&l);
        let res = cfg.resources.expect("resources");
        assert_eq!(res.max_processes, Some(u64::from(l.max_processes)));
        let py = cfg.python.expect("python");
        assert_eq!(py.execution_timeout_ms, Some(l.exec_timeout_ms));
    }

    #[test]
    fn blocked_spec_has_no_dns() {
        let spec = SandboxSpec::code_interpreter(
            sandbox_client::SandboxBackend::Wasm,
            "t".into(),
            "o".into(),
            "u:1".into(),
        );
        let cfg = spec_to_vm_config(&spec, &OrchestratorEnv::default());
        assert!(cfg.dns.is_none(), "code_interpreter must have no resolver");
    }
}

//! egress allowlist の照合（gVisor/FC ティアはこの判定を **プロキシ内で自前実行**する）。
//!
//! 意味論は `config.rs`（wasm ティアの `map_egress`）と一致させる: deny_overlay 先頭・完全一致 or
//! 明示ワイルドカード（`*.example.com`）のみ・部分文字列マッチは作らない（PIT-36 の教訓）。

use sandbox_client::Egress;

/// 1 接続（host, port）に対する判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    /// allow ルールに一致せず default-deny。
    DenyNoRule,
    /// 管理者 deny_overlay に一致（allow より優先）。
    DenyOverlay,
    /// ホスト名が取れない（非 TLS/非 HTTP など）→ 遮断。
    DenyNoHostname,
}

impl Decision {
    /// 監査ログ用の短い理由文字列。
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            Decision::Allow => "allow",
            Decision::DenyNoRule => "deny_no_rule",
            Decision::DenyOverlay => "deny_overlay",
            Decision::DenyNoHostname => "deny_no_hostname",
        }
    }

    #[must_use]
    pub fn is_allow(self) -> bool {
        matches!(self, Decision::Allow)
    }
}

/// ホストパターンが具体ホストに一致するか（大文字小文字無視・完全一致 or `*.suffix`）。
fn host_matches(pattern: &str, host: &str) -> bool {
    let pattern = pattern.trim_end_matches('.');
    let host = host.trim_end_matches('.');
    if let Some(suffix) = pattern.strip_prefix("*.") {
        // `*.example.com` は `a.example.com` に一致するが `example.com` 自体には一致しない。
        let tail = format!(".{suffix}");
        host.len() > tail.len()
            && host
                .to_ascii_lowercase()
                .ends_with(&tail.to_ascii_lowercase())
    } else {
        pattern.eq_ignore_ascii_case(host)
    }
}

fn rule_matches(rule: &sandbox_client::EgressRule, host: &str, port: u16) -> bool {
    (rule.port == 0 || rule.port == port) && host_matches(&rule.host_pattern, host)
}

/// egress ポリシに対する (host, port) の判定。deny_overlay を最優先、次に静的＋動的 allow。
#[must_use]
pub fn evaluate(egress: &Egress, host: &str, port: u16) -> Decision {
    if egress
        .deny_overlay
        .iter()
        .any(|r| rule_matches(r, host, port))
    {
        return Decision::DenyOverlay;
    }
    let allowed = egress
        .static_allow
        .iter()
        .chain(egress.dynamic_allow.iter())
        .any(|r| rule_matches(r, host, port));
    if allowed {
        Decision::Allow
    } else {
        Decision::DenyNoRule
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sandbox_client::EgressRule;

    fn rule(host: &str, port: u16) -> EgressRule {
        EgressRule {
            host_pattern: host.to_string(),
            port,
        }
    }

    #[test]
    fn empty_is_default_deny() {
        assert_eq!(
            evaluate(&Egress::blocked(), "example.com", 443),
            Decision::DenyNoRule
        );
    }

    #[test]
    fn exact_host_and_port() {
        let e = Egress {
            static_allow: vec![rule("api.example.com", 443)],
            ..Egress::blocked()
        };
        assert!(evaluate(&e, "api.example.com", 443).is_allow());
        assert_eq!(evaluate(&e, "api.example.com", 80), Decision::DenyNoRule);
        assert_eq!(evaluate(&e, "evil.example.com", 443), Decision::DenyNoRule);
    }

    #[test]
    fn any_port_rule() {
        let e = Egress {
            static_allow: vec![rule("h.example.com", 0)],
            ..Egress::blocked()
        };
        assert!(evaluate(&e, "h.example.com", 443).is_allow());
        assert!(evaluate(&e, "h.example.com", 8443).is_allow());
    }

    #[test]
    fn wildcard_matches_subdomain_only() {
        let e = Egress {
            static_allow: vec![rule("*.example.com", 443)],
            ..Egress::blocked()
        };
        assert!(evaluate(&e, "a.example.com", 443).is_allow());
        assert!(evaluate(&e, "a.b.example.com", 443).is_allow());
        // ベースドメインそのものには一致しない。
        assert_eq!(evaluate(&e, "example.com", 443), Decision::DenyNoRule);
        // 部分文字列一致はしない。
        assert_eq!(evaluate(&e, "notexample.com", 443), Decision::DenyNoRule);
    }

    #[test]
    fn deny_overlay_beats_allow() {
        let e = Egress {
            static_allow: vec![rule("*.example.com", 0)],
            deny_overlay: vec![rule("secret.example.com", 0)],
            ..Egress::blocked()
        };
        assert!(evaluate(&e, "ok.example.com", 443).is_allow());
        assert_eq!(
            evaluate(&e, "secret.example.com", 443),
            Decision::DenyOverlay
        );
    }

    #[test]
    fn case_insensitive() {
        let e = Egress {
            static_allow: vec![rule("API.Example.COM", 443)],
            ..Egress::blocked()
        };
        assert!(evaluate(&e, "api.example.com", 443).is_allow());
    }

    #[test]
    fn dynamic_allow_honored() {
        let e = Egress {
            dynamic_allow: vec![rule("fetch.example.com", 443)],
            ..Egress::blocked()
        };
        assert!(evaluate(&e, "fetch.example.com", 443).is_allow());
    }
}

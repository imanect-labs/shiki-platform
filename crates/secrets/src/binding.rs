//! 宛先束縛（PIT-36）。シークレットを添付できるホストを制限する。
//!
//! ホスト照合は **完全一致 or 明示サフィックス（`*.example.com`）のみ**。部分文字列マッチは
//! 禁止する（`api.slack.com.evil.com` のような近似ホストを弾く）。リダイレクト追従時も
//! 追従先ホストを本判定に通す（http.request ノード側で再検証・PIT-36 ①②）。

/// シークレットの宛先束縛（許可ホスト集合）。
#[derive(Debug, Clone)]
pub struct DestinationBinding {
    allowed_hosts: Vec<String>,
}

impl DestinationBinding {
    pub fn new(allowed_hosts: Vec<String>) -> Self {
        DestinationBinding { allowed_hosts }
    }

    /// `host` が許可集合に含まれるか（完全一致 or `*.suffix`）。
    pub fn allows(&self, host: &str) -> bool {
        self.allowed_hosts.iter().any(|pat| host_allowed(pat, host))
    }

    pub fn hosts(&self) -> &[String] {
        &self.allowed_hosts
    }
}

/// 1 パターンと host の照合（完全一致 or `*.suffix`・部分一致は不可）。
pub fn host_allowed(pattern: &str, host: &str) -> bool {
    let pattern = pattern.trim().to_ascii_lowercase();
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if pattern.is_empty() || host.is_empty() {
        return false;
    }
    if let Some(suffix) = pattern.strip_prefix("*.") {
        // `*.example.com` は「example.com のサブドメイン」を許す（example.com 自体は含まない）。
        // 近似ホスト（api.slack.com.evil.com）は suffix 一致しても "." 直前境界で弾く。
        return host.len() > suffix.len()
            && host.ends_with(suffix)
            && host.as_bytes()[host.len() - suffix.len() - 1] == b'.';
    }
    pattern == host
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match() {
        assert!(host_allowed("api.slack.com", "api.slack.com"));
        assert!(host_allowed("api.slack.com", "API.SLACK.COM")); // 大文字小文字非依存
        assert!(!host_allowed("api.slack.com", "slack.com"));
    }

    #[test]
    fn rejects_near_host_suffix_attack() {
        // 近似ホスト（PIT-36）: 部分文字列/接頭が一致しても弾く。
        assert!(!host_allowed("api.slack.com", "api.slack.com.evil.com"));
        assert!(!host_allowed("api.slack.com", "evil-api.slack.com"));
        assert!(!host_allowed("api.slack.com", "notapi.slack.com"));
    }

    #[test]
    fn wildcard_suffix() {
        assert!(host_allowed("*.slack.com", "api.slack.com"));
        assert!(host_allowed("*.slack.com", "hooks.slack.com"));
        // ワイルドカードはドメイン自体を含まない。
        assert!(!host_allowed("*.slack.com", "slack.com"));
        // 近似（境界が "." でない）は弾く。
        assert!(!host_allowed("*.slack.com", "evilslack.com"));
        assert!(!host_allowed("*.slack.com", "api.slack.com.evil.com"));
    }

    #[test]
    fn binding_allows_any() {
        let b = DestinationBinding::new(vec!["api.slack.com".into(), "*.example.com".into()]);
        assert!(b.allows("api.slack.com"));
        assert!(b.allows("x.example.com"));
        assert!(!b.allows("evil.com"));
    }

    #[test]
    fn empty_denies() {
        assert!(!host_allowed("", "api.slack.com"));
        assert!(!host_allowed("api.slack.com", ""));
        let b = DestinationBinding::new(vec![]);
        assert!(!b.allows("api.slack.com"));
    }
}

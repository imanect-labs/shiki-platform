//! 管理者が内部のネットワークの送り先として許したホストの表（D-135）。
//!
//! 外への送信は、名前解決の後のアドレスに内部の IP が 1 つでもあれば断る（[`crate::net_guard`]・PIT-48 の ②）。
//! 例外は、管理者がデプロイ設定（ワークフローの `workflow.http_internal_hosts`・ミニアプリの関数の
//! `gateway.function_http_internal_hosts`）に書いたホストだけで、
//! そのホスト（ポートを書けばそのポートだけ）には内部の IP へも送る（オンプレの社内システムを呼ぶ使い方）。
//! 書き方の読み方と照らし方は、この 1 か所に置く（送り口ごとに写さない）。
//!
//! - 書き方: `host`・`host:port`・`[IPv6]`・`[IPv6]:port`・括弧の無い IPv6。名前は URL と同じ規則で読む（大文字・
//!   小文字と末尾の `.` を区別しない。IP は 10 進・8 進・16 進の IPv4 も同じ IP にそろう）。
//! - ワイルドカード・スキーム・パス・利用者名は書けない。読めない項目があれば、起動の確かめ（[`InternalHosts::parse`]）が
//!   管理者向けの文で断る（黙って外すと、社内の送り先を書いたつもりで、実行のときに断られる理由が分からない）。
//! - 送れる先の許可（公開の SaaS も書く場所）とは分ける。許可に書いたホストまで内部へつながると、DNS を持つ人が
//!   解決先を内部へ向けるだけで内部の任意のポートへ届く。ここに書くのは、DNS を管理者が管理するホストだけにする。

/// 1 つの項目（そろえた名前か IP と、書いたときはポート）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    /// [`canonical_host`] でそろえた名前か IP。
    host: String,
    /// 書いたときは、そのポートにだけ送れる。
    port: Option<u16>,
}

/// 内部の送り先として許したホストの表（空なら内部の IP へは送らない）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InternalHosts {
    entries: Vec<Entry>,
}

impl InternalHosts {
    /// 設定 `setting`（誤りの文に出す設定の名前）の項目から組む。読めない項目があれば、その文を返す。
    pub fn parse(setting: &str, entries: &[String]) -> Result<Self, String> {
        entries
            .iter()
            .map(|e| parse_entry(setting, e))
            .collect::<Result<Vec<_>, _>>()
            .map(|entries| InternalHosts { entries })
    }

    /// 読めない項目を除いて組む（起動の確かめ [`InternalHosts::parse`] を通した後の組み立て）。
    ///
    /// 読めない項目（ワイルドカードなど）は、内部の宛先を許さない。
    #[must_use]
    pub fn lenient(entries: &[String]) -> Self {
        InternalHosts {
            entries: entries
                .iter()
                .filter_map(|e| parse_entry("", e).ok())
                .collect(),
        }
    }

    /// このホストとポートへ、内部の IP への送信を許すか（ポートを書いた項目はそのポートだけ）。
    #[must_use]
    pub fn allows(&self, host: &url::Host<&str>, port: u16) -> bool {
        let host = canonical_host(host);
        self.entries
            .iter()
            .any(|e| e.host == host && e.port.is_none_or(|p| p == port))
    }

    /// このホストのこのポートを、ポートを書いた項目で許したか（ポートを決めて許す送り口が、決まったポートの外を
    /// 開けるかを見る）。
    #[must_use]
    pub fn names_port(&self, host: &url::Host<&str>, port: u16) -> bool {
        let host = canonical_host(host);
        self.entries
            .iter()
            .any(|e| e.host == host && e.port == Some(port))
    }

    /// 1 つも書いていないか。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// 照らすホストの形（名前は小文字で末尾の `.` を外す。IP は 1 つの書き方にそろえる）。
fn canonical_host<S: AsRef<str>>(host: &url::Host<S>) -> String {
    match host {
        url::Host::Domain(d) => d.as_ref().trim_end_matches('.').to_ascii_lowercase(),
        url::Host::Ipv4(ip) => ip.to_string(),
        url::Host::Ipv6(ip) => ip.to_string(),
    }
}

/// 1 つの項目を読む（誤りの文は管理者向け）。
fn parse_entry(setting: &str, entry: &str) -> Result<Entry, String> {
    let unreadable = || {
        format!(
            "{setting} の「{entry}」を読めません。ホスト名か IP を、ワイルドカードを使わずに書いて\
             ください（ポートを限るなら host:port。例: erp.corp.example、10.0.0.5:8443、[fd00::5]:8443）。"
        )
    };
    let entry_t = entry.trim();
    if entry_t.is_empty() || entry_t.contains('*') || entry_t.contains('/') || entry_t.contains('@')
    {
        return Err(unreadable());
    }
    let (host, port) = if let Some(rest) = entry_t.strip_prefix('[') {
        let (inner, after) = rest.split_once(']').ok_or_else(unreadable)?;
        let port = match after {
            "" => None,
            p => Some(p.strip_prefix(':').ok_or_else(unreadable)?),
        };
        (format!("[{inner}]"), port)
    } else {
        match entry_t.matches(':').count() {
            0 => (entry_t.to_string(), None),
            1 => {
                let (h, p) = entry_t.split_once(':').ok_or_else(unreadable)?;
                (h.to_string(), Some(p))
            }
            // 括弧の無い IPv6（ポートは書けない）。
            _ => (format!("[{entry_t}]"), None),
        }
    };
    let port = port
        .map(|p| {
            p.parse::<u16>()
                .ok()
                .filter(|p| *p > 0)
                .ok_or_else(unreadable)
        })
        .transpose()?;
    let canonical = canonical_host(&url::Host::parse(&host).map_err(|_| unreadable())?);
    if canonical.is_empty() {
        return Err(unreadable());
    }
    Ok(Entry {
        host: canonical,
        port,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SETTING: &str = "workflow.http_internal_hosts";

    /// 書き方: 名前は大文字・小文字と末尾の . を問わず、IP は書き方によらず同じ IP にそろえる。読めない項目
    /// （ワイルドカード・スキーム・パス・ポートの誤り）は、設定の名前を添えた管理者向けの文で断る。
    #[test]
    fn entries_are_read_like_urls_and_unreadable_ones_are_reported() {
        let read = |e: &str| parse_entry(SETTING, e).map(|h| (h.host, h.port));
        assert_eq!(
            read(" ERP.Corp.Example. "),
            Ok(("erp.corp.example".into(), None))
        );
        assert_eq!(
            read("erp.corp.example:8443"),
            Ok(("erp.corp.example".into(), Some(8443)))
        );
        assert_eq!(read("0x0a.0.0.5"), Ok(("10.0.0.5".into(), None)));
        assert_eq!(read("10.0.0.5:80"), Ok(("10.0.0.5".into(), Some(80))));
        assert_eq!(read("[FD00:0::5]:8443"), Ok(("fd00::5".into(), Some(8443))));
        assert_eq!(read("fd00::5"), Ok(("fd00::5".into(), None)));
        for bad in [
            "",
            "*.corp.example",
            "erp.*.example",
            "https://erp.corp.example",
            "erp.corp.example/api",
            "erp.corp.example:0",
            "erp.corp.example:http",
            "[fd00::5",
            "[fd00::5]8443",
            "user@erp.corp.example",
        ] {
            let err = read(bad).unwrap_err();
            assert!(
                err.starts_with(&format!("{SETTING} の「{bad}」を読めません。")),
                "{bad}: {err}"
            );
        }
        let ok = ["erp.corp.example".to_string(), "10.0.0.5:8443".to_string()];
        assert!(InternalHosts::parse(SETTING, &ok).is_ok());
        let bad = ["erp.corp.example".to_string(), "*.corp.example".to_string()];
        assert!(InternalHosts::parse(SETTING, &bad)
            .unwrap_err()
            .starts_with(&format!("{SETTING} の「*.corp.example」を読めません。")));
        // 読めない項目は、組み立てでは除く（ほかの項目は効く）。
        let lenient = InternalHosts::lenient(&bad);
        assert!(lenient.allows(&url::Host::Domain("erp.corp.example"), 443));
        assert!(!lenient.allows(&url::Host::Domain("x.corp.example"), 443));
    }

    /// ポートを書いた項目はそのポートだけ。書かない項目はどのポートも許すが、ポートを名指ししたことにはならない。
    #[test]
    fn ports_are_matched_only_when_written() {
        let hosts = InternalHosts::parse(
            SETTING,
            &["erp.corp.example:8443".to_string(), "10.0.0.5".to_string()],
        )
        .unwrap();
        let erp = url::Host::Domain("ERP.corp.example.");
        assert!(hosts.allows(&erp, 8443));
        assert!(!hosts.allows(&erp, 443));
        assert!(hosts.names_port(&erp, 8443));
        assert!(!hosts.names_port(&erp, 443));
        let ip = url::Host::Ipv4("10.0.0.5".parse().unwrap());
        assert!(hosts.allows(&ip, 22));
        assert!(!hosts.names_port(&ip, 22));
        assert!(!hosts.is_empty());
        assert!(InternalHosts::default().is_empty());
    }
}

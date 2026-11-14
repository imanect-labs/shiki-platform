//! エージェントブラウザの cookie jar（docs/agent/browser.md §5・PIT-80）。
//!
//! jar はエンジンではなく `BrowserSession`（run 単位）が持つ。エンジンを切り替えても jar は 1 つで、
//! 値はモデル・ツール結果・ログに出さない（[`CookieJar::host_summary`] は件数だけを返す）。
//!
//! RFC 6265bis のうち、エージェントの取得に効く規則を実装する:
//! - Domain 属性は要求ホストを含み、公開サフィックス（`co.jp` 等）であってはならない。
//!   省略時は host-only（そのホストにだけ送る）。
//! - Secure は https からしか設定できず、https にしか送らない。http からは同名の Secure cookie を上書きできない。
//! - `__Secure-` / `__Host-` 接頭辞の条件を検証する。
//! - SameSite は**送信時に強制する**（未指定は Lax 扱い・`None` は Secure 必須）。
//!   `cookie_store` クレートは SameSite を強制しないので自前で持つ（browser.md §5.2）。
//! - Partitioned（CHIPS）はトップレベルのサイトごとに分ける。

use cookie::{Cookie, SameSite};
use time::OffsetDateTime;
use url::{Host, Url};

/// jar 全体の上限（溢れたら最も古いものから捨てる）。
const MAX_COOKIES: usize = 300;
/// 1 サイト（登録可能ドメイン＝eTLD+1）あたりの上限。ドメインの文字列ごとに数えると、1 つのサイトが
/// Domain 属性を変えて jar を埋め、他サイトの cookie（ログイン中のセッション）を追い出せる。
const MAX_PER_SITE: usize = 50;
/// 名前＋値の上限（RFC 6265bis §5.6 の 4096 バイト）。
const MAX_NAME_VALUE_BYTES: usize = 4096;

/// 保存した cookie（属性を落とさずに持つ）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCookie {
    pub name: String,
    pub value: String,
    /// 小文字・先頭のドットなし。
    pub domain: String,
    /// true ならそのホストにだけ送る（Domain 属性なし）。
    pub host_only: bool,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: SameSiteMode,
    /// Partitioned のときのトップレベルサイト（スキーム付きの登録可能ドメイン）。
    pub partition: Option<String>,
    /// None はセッション cookie（run の終わりで jar ごと消える）。
    pub expires: Option<OffsetDateTime>,
    /// 作成順（同じ長さのパスの並び順に使う）。
    pub creation: u64,
}

/// 送信時に効く SameSite（未指定は Lax 扱い）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SameSiteMode {
    Strict,
    Lax,
    None,
}

/// 要求の文脈（SameSite と Partitioned の判定に使う）。
#[derive(Debug, Clone)]
pub struct RequestContext {
    /// 要求を起こしたページの URL。None はアドレスバーと同じ扱い（ユーザーが書いた URL・
    /// モデルの navigate）で、同一サイトとみなす。
    pub initiator: Option<Url>,
    /// トップレベルの遷移か（D はサブリソースを取らないので常に true）。
    pub top_level: bool,
    /// 安全なメソッド（GET / HEAD）か。
    pub safe_method: bool,
    /// リダイレクトの連鎖の途中で別サイトを通った。以後は同一サイトとみなさない
    /// （連鎖に別サイトが入った時点で Strict を外す・RFC 6265bis）。
    pub cross_site_redirect: bool,
}

/// [`CookieJar::store`] が受け取らなかった理由（観測用。値は含めない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejected {
    Unparsable,
    TooLarge,
    DomainMismatch,
    PublicSuffix,
    SecureFromInsecure,
    PrefixViolation,
    SameSiteNoneWithoutSecure,
    OverwritesSecure,
    /// Partitioned は Secure 必須（CHIPS）。属性を外して普通の cookie にはしない。
    PartitionedWithoutSecure,
}

/// run 単位の cookie jar。
#[derive(Debug, Default, Clone)]
pub struct CookieJar {
    cookies: Vec<StoredCookie>,
    seq: u64,
}

impl CookieJar {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 応答の `Set-Cookie` を 1 つ取り込む。`top_level` は Partitioned のキーに使うトップレベルの URL。
    ///
    /// # Errors
    /// 規則に反して取り込まなかった理由（値は含めない）。
    pub fn store(
        &mut self,
        url: &Url,
        top_level: &Url,
        set_cookie: &str,
        now: OffsetDateTime,
    ) -> Result<(), Rejected> {
        let parsed = Cookie::parse(set_cookie).map_err(|_| Rejected::Unparsable)?;
        if parsed.name().len() + parsed.value().len() > MAX_NAME_VALUE_BYTES {
            return Err(Rejected::TooLarge);
        }
        let Some(host) = host_of(url) else {
            return Err(Rejected::DomainMismatch);
        };
        let secure_origin = url.scheme() == "https";
        let secure = parsed.secure().unwrap_or(false);
        if secure && !secure_origin {
            return Err(Rejected::SecureFromInsecure);
        }

        let (domain, host_only) = resolve_domain(&host, url, parsed.domain())?;
        let path = match parsed.path() {
            Some(p) if p.starts_with('/') => p.to_string(),
            _ => default_path(url),
        };
        check_prefix(parsed.name(), secure, secure_origin, host_only, &path)?;

        let same_site = match parsed.same_site() {
            Some(SameSite::Strict) => SameSiteMode::Strict,
            Some(SameSite::None) => {
                if !secure {
                    return Err(Rejected::SameSiteNoneWithoutSecure);
                }
                SameSiteMode::None
            }
            // 未指定は Lax 扱い（Lax-by-default）。
            Some(SameSite::Lax) | None => SameSiteMode::Lax,
        };
        let partition = if parsed.partitioned().unwrap_or(false) {
            if !secure {
                return Err(Rejected::PartitionedWithoutSecure);
            }
            Some(site_key(top_level))
        } else {
            None
        };
        let expires = expiry(&parsed, now);

        // http からは同名の Secure cookie を上書き・削除できない（6265bis §5.7 step 16）。
        if !secure_origin
            && self.cookies.iter().any(|c| {
                c.secure
                    && c.name == parsed.name()
                    && (domain_match(&c.domain, &domain) || domain_match(&domain, &c.domain))
                    && path_match(&path, &c.path)
            })
        {
            return Err(Rejected::OverwritesSecure);
        }

        let creation = self
            .position(
                parsed.name(),
                &domain,
                host_only,
                &path,
                partition.as_deref(),
            )
            .map(|i| self.cookies.remove(i).creation);
        if expires.is_some_and(|e| e <= now) {
            // 期限切れの指定は削除の意味（既存があれば上で取り除いた）。
            return Ok(());
        }
        self.seq += 1;
        self.cookies.push(StoredCookie {
            name: parsed.name().to_string(),
            value: parsed.value().to_string(),
            domain,
            host_only,
            path,
            secure,
            http_only: parsed.http_only().unwrap_or(false),
            same_site,
            partition,
            expires,
            creation: creation.unwrap_or(self.seq),
        });
        // 期限切れを先に捨ててから上限を判定する（期限切れが有効な cookie を押し出さないように）。
        self.cookies.retain(|c| c.expires.is_none_or(|e| e > now));
        self.evict();
        Ok(())
    }

    /// `url` への要求に付ける `Cookie` ヘッダの値。送るものが無ければ None。
    #[must_use]
    pub fn header_for(
        &self,
        url: &Url,
        ctx: &RequestContext,
        now: OffsetDateTime,
    ) -> Option<String> {
        let host = host_of(url)?;
        let secure_target = url.scheme() == "https";
        let same_site =
            !ctx.cross_site_redirect && ctx.initiator.as_ref().is_none_or(|i| same_site(i, url));
        let top_site = site_key(
            ctx.initiator
                .as_ref()
                .filter(|_| !ctx.top_level)
                .unwrap_or(url),
        );
        let mut picked: Vec<&StoredCookie> = self
            .cookies
            .iter()
            .filter(|c| c.expires.is_none_or(|e| e > now))
            .filter(|c| {
                if c.host_only {
                    c.domain == host
                } else {
                    domain_match(&host, &c.domain)
                }
            })
            .filter(|c| path_match(url.path(), &c.path))
            .filter(|c| !c.secure || secure_target)
            .filter(|c| c.partition.as_ref().is_none_or(|p| *p == top_site))
            .filter(|c| {
                same_site
                    || match c.same_site {
                        SameSiteMode::Strict => false,
                        SameSiteMode::Lax => ctx.top_level && ctx.safe_method,
                        SameSiteMode::None => true,
                    }
            })
            .collect();
        if picked.is_empty() {
            return None;
        }
        // 長いパスを先に、同じ長さなら古いものを先に（6265bis §5.8.3）。
        picked.sort_by(|a, b| {
            b.path
                .len()
                .cmp(&a.path.len())
                .then(a.creation.cmp(&b.creation))
        });
        Some(
            picked
                .iter()
                .map(|c| format!("{}={}", c.name, c.value))
                .collect::<Vec<_>>()
                .join("; "),
        )
    }

    /// 監査用の要約（ホストごとの件数。名前と値は出さない）。
    #[must_use]
    pub fn host_summary(&self) -> Vec<(String, usize)> {
        let mut out: Vec<(String, usize)> = Vec::new();
        for c in &self.cookies {
            match out.iter_mut().find(|(d, _)| *d == c.domain) {
                Some((_, n)) => *n += 1,
                None => out.push((c.domain.clone(), 1)),
            }
        }
        out.sort();
        out
    }

    /// 件数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.cookies.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cookies.is_empty()
    }

    /// 全量を書き出す（エンジン切替の鏡写し・browser.md §5.2）。
    #[must_use]
    pub fn export(&self) -> Vec<StoredCookie> {
        self.cookies.clone()
    }

    /// 全量で置き換える（エンジン切替で戻るとき。部分の差分は取らない・PIT-80）。
    pub fn replace_all(&mut self, cookies: Vec<StoredCookie>) {
        self.seq = cookies.iter().map(|c| c.creation).max().unwrap_or(0);
        self.cookies = cookies;
        self.evict();
    }

    fn position(
        &self,
        name: &str,
        domain: &str,
        host_only: bool,
        path: &str,
        partition: Option<&str>,
    ) -> Option<usize> {
        self.cookies.iter().position(|c| {
            c.name == name
                && c.domain == domain
                && c.host_only == host_only
                && c.path == path
                && c.partition.as_deref() == partition
        })
    }

    fn evict(&mut self) {
        // サイトごとの上限 → 全体の上限の順に、古いものから捨てる。
        let mut sites: Vec<String> = self
            .cookies
            .iter()
            .map(|c| registrable(&c.domain))
            .collect();
        sites.sort();
        sites.dedup();
        for site in sites {
            while self
                .cookies
                .iter()
                .filter(|c| registrable(&c.domain) == site)
                .count()
                > MAX_PER_SITE
            {
                match self.oldest(|c| registrable(&c.domain) == site) {
                    Some(i) => {
                        self.cookies.remove(i);
                    }
                    None => break,
                }
            }
        }
        while self.cookies.len() > MAX_COOKIES {
            match self.oldest(|_| true) {
                Some(i) => {
                    self.cookies.remove(i);
                }
                None => break,
            }
        }
    }

    fn oldest(&self, pred: impl Fn(&StoredCookie) -> bool) -> Option<usize> {
        self.cookies
            .iter()
            .enumerate()
            .filter(|(_, c)| pred(c))
            .min_by_key(|(_, c)| c.creation)
            .map(|(i, _)| i)
    }
}

/// URL のホスト（小文字・末尾のドットなし）。IP は文字列表記。
fn host_of(url: &Url) -> Option<String> {
    match url.host()? {
        Host::Domain(d) => Some(d.trim_end_matches('.').to_ascii_lowercase()),
        Host::Ipv4(ip) => Some(ip.to_string()),
        Host::Ipv6(ip) => Some(format!("[{ip}]")),
    }
}

fn is_ip(url: &Url) -> bool {
    matches!(url.host(), Some(Host::Ipv4(_) | Host::Ipv6(_)))
}

/// Domain 属性を検証して（保存するドメイン, host-only か）を返す。
fn resolve_domain(host: &str, url: &Url, attr: Option<&str>) -> Result<(String, bool), Rejected> {
    let attr = attr
        .map(|d| {
            d.trim_start_matches('.')
                .trim_end_matches('.')
                .to_ascii_lowercase()
        })
        .filter(|d| !d.is_empty());
    let Some(d) = attr else {
        return Ok((host.to_string(), true));
    };
    if is_ip(url) {
        // IP には Domain 属性で広げられない。同じ値なら host-only として扱う。
        return if d == host.trim_matches(['[', ']']) || d == host {
            Ok((host.to_string(), true))
        } else {
            Err(Rejected::DomainMismatch)
        };
    }
    if !domain_match(host, &d) {
        return Err(Rejected::DomainMismatch);
    }
    if is_public_suffix(&d) {
        // 公開サフィックスそのものを Domain にできるのは、要求ホストが同じ値のときだけ（host-only に倒す）。
        return if d == host {
            Ok((host.to_string(), true))
        } else {
            Err(Rejected::PublicSuffix)
        };
    }
    Ok((d, false))
}

/// ドメインの登録可能ドメイン（eTLD+1）。判定できなければそのまま。
fn registrable(domain: &str) -> String {
    psl::domain_str(domain).unwrap_or(domain).to_string()
}

fn is_public_suffix(domain: &str) -> bool {
    psl::suffix_str(domain).is_some_and(|s| s == domain)
}

/// `host` が `domain` に一致するか、そのサブドメインか（文字列の後方一致ではなくラベル境界で比べる）。
fn domain_match(host: &str, domain: &str) -> bool {
    host == domain
        || (host.len() > domain.len()
            && host.ends_with(domain)
            && host.as_bytes()[host.len() - domain.len() - 1] == b'.')
}

/// 6265bis §5.1.4 の path-match。
fn path_match(request_path: &str, cookie_path: &str) -> bool {
    let request_path = if request_path.is_empty() {
        "/"
    } else {
        request_path
    };
    request_path == cookie_path
        || (request_path.starts_with(cookie_path)
            && (cookie_path.ends_with('/')
                || request_path.as_bytes().get(cookie_path.len()) == Some(&b'/')))
}

/// 6265bis §5.1.4 の default-path。
fn default_path(url: &Url) -> String {
    let p = url.path();
    match p.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => p[..i].to_string(),
    }
}

fn check_prefix(
    name: &str,
    secure: bool,
    secure_origin: bool,
    host_only: bool,
    path: &str,
) -> Result<(), Rejected> {
    let lower = name.to_ascii_lowercase();
    if lower.starts_with("__secure-") && !(secure && secure_origin) {
        return Err(Rejected::PrefixViolation);
    }
    if lower.starts_with("__host-") && !(secure && secure_origin && host_only && path == "/") {
        return Err(Rejected::PrefixViolation);
    }
    Ok(())
}

fn expiry(parsed: &Cookie<'_>, now: OffsetDateTime) -> Option<OffsetDateTime> {
    // Max-Age が Expires より優先される。
    if let Some(age) = parsed.max_age() {
        return Some(now.saturating_add(age));
    }
    parsed.expires_datetime()
}

/// スキーム付きの登録可能ドメイン（schemeful same-site の単位）。IP はアドレスそのもの。
pub(crate) fn site_key(url: &Url) -> String {
    let host = host_of(url).unwrap_or_default();
    let registrable = if is_ip(url) {
        host
    } else {
        psl::domain_str(&host).map_or(host.clone(), str::to_string)
    };
    format!("{}://{registrable}", url.scheme())
}

/// 2 つの URL が同一サイトか（スキームと登録可能ドメインが一致）。
pub(crate) fn same_site(a: &Url, b: &Url) -> bool {
    site_key(a) == site_key(b)
}

#[cfg(test)]
#[path = "cookie_jar_tests.rs"]
mod tests;

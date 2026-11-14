use super::*;

fn u(s: &str) -> Url {
    Url::parse(s).unwrap()
}

fn now() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap()
}

fn nav() -> RequestContext {
    RequestContext {
        initiator: None,
        top_level: true,
        safe_method: true,
    }
}

fn from(initiator: &str, safe_method: bool) -> RequestContext {
    RequestContext {
        initiator: Some(u(initiator)),
        top_level: true,
        safe_method,
    }
}

fn set(jar: &mut CookieJar, url: &str, header: &str) -> Result<(), Rejected> {
    let url = u(url);
    jar.store(&url, &url, header, now())
}

#[test]
fn host_only_cookie_is_not_sent_to_subdomains() {
    let mut jar = CookieJar::new();
    set(&mut jar, "https://a.example.co.jp/login", "sid=1; Path=/").unwrap();
    assert_eq!(
        jar.header_for(&u("https://a.example.co.jp/x"), &nav(), now()),
        Some("sid=1".into())
    );
    assert_eq!(
        jar.header_for(&u("https://evil.a.example.co.jp/"), &nav(), now()),
        None
    );
    assert_eq!(
        jar.header_for(&u("https://example.co.jp/"), &nav(), now()),
        None
    );
}

#[test]
fn domain_cookie_reaches_subdomains_but_not_lookalikes() {
    let mut jar = CookieJar::new();
    set(
        &mut jar,
        "https://www.example.com/",
        "d=1; Domain=example.com; Path=/",
    )
    .unwrap();
    assert!(jar
        .header_for(&u("https://api.example.com/"), &nav(), now())
        .is_some());
    assert_eq!(
        jar.header_for(&u("https://badexample.com/"), &nav(), now()),
        None
    );
}

#[test]
fn rejects_public_suffix_and_foreign_domains() {
    let mut jar = CookieJar::new();
    assert_eq!(
        set(&mut jar, "https://shop.example.co.jp/", "x=1; Domain=co.jp"),
        Err(Rejected::PublicSuffix)
    );
    assert_eq!(
        set(
            &mut jar,
            "https://shop.example.co.jp/",
            "x=1; Domain=other.co.jp"
        ),
        Err(Rejected::DomainMismatch)
    );
    assert!(jar.is_empty());
}

#[test]
fn secure_rules() {
    let mut jar = CookieJar::new();
    assert_eq!(
        set(&mut jar, "http://example.com/", "s=1; Secure"),
        Err(Rejected::SecureFromInsecure)
    );
    set(&mut jar, "https://example.com/", "s=1; Secure; Path=/").unwrap();
    assert_eq!(
        jar.header_for(&u("http://example.com/"), &nav(), now()),
        None
    );
    assert_eq!(
        jar.header_for(&u("https://example.com/"), &nav(), now()),
        Some("s=1".into())
    );
    // http から同名の Secure cookie は上書きできない。
    assert_eq!(
        set(&mut jar, "http://example.com/", "s=evil; Path=/"),
        Err(Rejected::OverwritesSecure)
    );
}

#[test]
fn cookie_prefixes() {
    let mut jar = CookieJar::new();
    assert_eq!(
        set(&mut jar, "https://example.com/", "__Secure-a=1"),
        Err(Rejected::PrefixViolation)
    );
    assert_eq!(
        set(
            &mut jar,
            "https://example.com/",
            "__Host-a=1; Secure; Path=/; Domain=example.com"
        ),
        Err(Rejected::PrefixViolation)
    );
    assert_eq!(
        set(
            &mut jar,
            "https://example.com/x/",
            "__Host-a=1; Secure; Path=/x"
        ),
        Err(Rejected::PrefixViolation)
    );
    set(
        &mut jar,
        "https://example.com/",
        "__Host-a=1; Secure; Path=/",
    )
    .unwrap();
    set(&mut jar, "https://example.com/", "__Secure-b=2; Secure").unwrap();
    assert_eq!(jar.len(), 2);
}

#[test]
fn same_site_is_enforced_on_cross_site_requests() {
    let mut jar = CookieJar::new();
    set(
        &mut jar,
        "https://bank.example/",
        "strict=1; SameSite=Strict; Path=/",
    )
    .unwrap();
    set(&mut jar, "https://bank.example/", "lax=1; Path=/").unwrap(); // 未指定は Lax
    set(
        &mut jar,
        "https://bank.example/",
        "none=1; SameSite=None; Secure; Path=/",
    )
    .unwrap();
    let target = u("https://bank.example/transfer");

    // アドレスバー相当（ユーザーが書いた URL）は同一サイト扱いで全部付く。
    let all = jar.header_for(&target, &nav(), now()).unwrap();
    assert!(all.contains("strict=1") && all.contains("lax=1") && all.contains("none=1"));

    // 別サイトのリンク（トップレベル GET）: Strict は付かない、Lax は付く。
    let link = jar
        .header_for(&target, &from("https://attacker.example/", true), now())
        .unwrap();
    assert!(!link.contains("strict"), "{link}");
    assert!(link.contains("lax=1") && link.contains("none=1"), "{link}");

    // 別サイトからの POST: Lax も付かない（D 経由だけの CSRF を作らない・PIT-80）。
    let post = jar
        .header_for(&target, &from("https://attacker.example/", false), now())
        .unwrap();
    assert_eq!(post, "none=1");

    // 同じサイトのサブドメインからは全部付く（schemeful same-site）。
    let same = jar
        .header_for(&target, &from("https://www.bank.example/", false), now())
        .unwrap();
    assert!(same.contains("strict=1"));
    // スキームが違えば別サイト。
    let http = jar
        .header_for(&target, &from("http://bank.example/", false), now())
        .unwrap();
    assert!(!http.contains("strict") && !http.contains("lax"));
}

#[test]
fn same_site_none_requires_secure() {
    let mut jar = CookieJar::new();
    assert_eq!(
        set(&mut jar, "https://example.com/", "n=1; SameSite=None"),
        Err(Rejected::SameSiteNoneWithoutSecure)
    );
}

#[test]
fn expiry_and_deletion() {
    let mut jar = CookieJar::new();
    set(&mut jar, "https://example.com/", "a=1; Path=/; Max-Age=60").unwrap();
    assert!(jar
        .header_for(&u("https://example.com/"), &nav(), now())
        .is_some());
    let later = now() + time::Duration::seconds(61);
    assert_eq!(
        jar.header_for(&u("https://example.com/"), &nav(), later),
        None
    );
    // Max-Age=0 は削除（ログアウト）。
    set(&mut jar, "https://example.com/", "a=; Path=/; Max-Age=0").unwrap();
    assert!(jar.is_empty());
}

#[test]
fn path_match_and_ordering() {
    let mut jar = CookieJar::new();
    set(&mut jar, "https://example.com/", "a=root; Path=/").unwrap();
    set(&mut jar, "https://example.com/", "a=docs; Path=/docs").unwrap();
    assert_eq!(
        jar.header_for(&u("https://example.com/docs/x"), &nav(), now()),
        Some("a=docs; a=root".into())
    );
    assert_eq!(
        jar.header_for(&u("https://example.com/docsx"), &nav(), now()),
        Some("a=root".into())
    );
    // default-path はディレクトリ部分。
    set(&mut jar, "https://example.com/app/page", "b=1").unwrap();
    assert!(jar
        .header_for(&u("https://example.com/app/other"), &nav(), now())
        .unwrap()
        .contains("b=1"));
    assert!(!jar
        .header_for(&u("https://example.com/"), &nav(), now())
        .unwrap()
        .contains("b=1"));
}

#[test]
fn replacing_keeps_creation_order_and_summary_hides_values() {
    let mut jar = CookieJar::new();
    set(&mut jar, "https://example.com/", "a=1; Path=/").unwrap();
    set(&mut jar, "https://example.com/", "b=1; Path=/").unwrap();
    set(&mut jar, "https://example.com/", "a=2; Path=/").unwrap();
    assert_eq!(
        jar.header_for(&u("https://example.com/"), &nav(), now()),
        Some("a=2; b=1".into())
    );
    assert_eq!(jar.host_summary(), vec![("example.com".to_string(), 2)]);
}

#[test]
fn partitioned_cookies_follow_top_level_site() {
    let mut jar = CookieJar::new();
    let embed = u("https://widget.example/");
    jar.store(
        &embed,
        &u("https://shop.example/"),
        "p=1; Secure; Partitioned; SameSite=None; Path=/",
        now(),
    )
    .unwrap();
    let in_shop = RequestContext {
        initiator: Some(u("https://shop.example/")),
        top_level: false,
        safe_method: true,
    };
    let in_other = RequestContext {
        initiator: Some(u("https://other.example/")),
        top_level: false,
        safe_method: true,
    };
    assert!(jar.header_for(&embed, &in_shop, now()).is_some());
    assert_eq!(jar.header_for(&embed, &in_other, now()), None);
}

#[test]
fn export_and_replace_all_round_trip() {
    let mut jar = CookieJar::new();
    set(&mut jar, "https://a.example/", "x=1; Path=/").unwrap();
    set(&mut jar, "https://b.example/", "y=1; Path=/").unwrap();
    let snapshot = jar.export();
    let mut other = CookieJar::new();
    other.replace_all(snapshot);
    assert_eq!(other.host_summary(), jar.host_summary());
    // 置き換えた後に足したものは既存より新しい。
    set(&mut other, "https://a.example/", "z=1; Path=/").unwrap();
    assert_eq!(
        other.header_for(&u("https://a.example/"), &nav(), now()),
        Some("x=1; z=1".into())
    );
}

#[test]
fn limits_evict_oldest() {
    let mut jar = CookieJar::new();
    for i in 0..(MAX_PER_DOMAIN + 5) {
        set(&mut jar, "https://example.com/", &format!("c{i}=1; Path=/")).unwrap();
    }
    assert_eq!(jar.len(), MAX_PER_DOMAIN);
    let header = jar
        .header_for(&u("https://example.com/"), &nav(), now())
        .unwrap();
    assert!(!header.contains("c0=1"), "最も古いものから捨てる");
    assert!(header.contains(&format!("c{}=1", MAX_PER_DOMAIN + 4)));
    let big = format!("big={}", "x".repeat(MAX_NAME_VALUE_BYTES));
    assert_eq!(
        set(&mut jar, "https://example.com/", &big),
        Err(Rejected::TooLarge)
    );
}

#[test]
fn ip_hosts_cannot_widen_with_domain() {
    let mut jar = CookieJar::new();
    assert_eq!(
        set(&mut jar, "http://192.0.2.1/", "a=1; Domain=192.0.2.0"),
        Err(Rejected::DomainMismatch)
    );
    set(
        &mut jar,
        "http://192.0.2.1/",
        "a=1; Domain=192.0.2.1; Path=/",
    )
    .unwrap();
    assert_eq!(
        jar.header_for(&u("http://192.0.2.1/"), &nav(), now()),
        Some("a=1".into())
    );
    assert_eq!(
        set(&mut jar, "https://example.com/", "bad"),
        Err(Rejected::Unparsable)
    );
}

//! テキストエンジンの結合テスト（スタブ HTTP サーバに対して、セッション越しに操作する）。

#![allow(clippy::panic, clippy::unwrap_used)]

mod support;

use std::sync::Arc;

use browser::{
    Action, BrowserError, BrowserSession, ElementKind, RefId, Snapshot, TextEngine, TextLimits,
};
use support::{allow_loopback, ctx, http, serve, Req, ToStub};
use url::Url;

fn find(snap: &Snapshot, label: &str) -> RefId {
    snap.elements
        .iter()
        .find(|e| e.label == label)
        .unwrap_or_else(|| panic!("「{label}」が無い: {}", snap.text))
        .id
}

async fn session(route: support::Route) -> (BrowserSession, u16, Arc<std::sync::Mutex<Vec<Req>>>) {
    let (addr, log) = serve(route).await;
    let engine = TextEngine::with_guard(
        Arc::new(ToStub(addr)),
        allow_loopback,
        TextLimits::default(),
    );
    (
        BrowserSession::new(ctx(), Arc::new(engine), []),
        addr.port(),
        log,
    )
}

fn u(port: u16, host: &str, path: &str) -> Url {
    Url::parse(&format!("http://{host}:{port}{path}")).unwrap()
}

fn hits(log: &Arc<std::sync::Mutex<Vec<Req>>>, path: &str) -> usize {
    log.lock()
        .unwrap()
        .iter()
        .filter(|r| r.target == path)
        .count()
}

#[tokio::test]
async fn login_sets_cookie_and_back_does_not_refetch() {
    let route: support::Route = Arc::new(|req: &Req| {
        match (req.method.as_str(), req.target.as_str()) {
            ("GET", "/login") => http(
                "200 OK",
                &[],
                r#"<form method="post" action="/login"><label>ユーザー名 <input name="user"></label>
               <label>パスワード <input type="password" name="pass"></label><button>ログイン</button></form>"#,
            ),
            ("POST", "/login") if req.body == "user=yamada&pass=p%40ss" => http(
                "302 Found",
                &[
                    ("Location", "/account"),
                    ("Set-Cookie", "sid=tok-4f9a; Path=/; HttpOnly"),
                ],
                "",
            ),
            ("GET", "/account") if req.cookie.as_deref() == Some("sid=tok-4f9a") => http(
                "200 OK",
                &[],
                "<h1>マイページ</h1><p>所属: 営業二部</p><a href=\"/login\">戻る</a>",
            ),
            _ => http("403 Forbidden", &[], "<p>だめ</p>"),
        }
    });
    let (mut s, port, log) = session(route).await;

    let snap = s
        .navigate(u(port, "portal.example.com", "/login"))
        .await
        .unwrap();
    let user = find(&snap, "ユーザー名");
    let pass = find(&snap, "パスワード");
    s.act(Action::Fill(user, "yamada".into())).await.unwrap();
    let snap = s.act(Action::Fill(pass, "p@ss".into())).await.unwrap();
    assert!(
        !snap.text.contains("p@ss"),
        "パスワードは伏せる: {}",
        snap.text
    );
    let snap = s.act(Action::Click(find(&snap, "ログイン"))).await.unwrap();
    assert_eq!(snap.url.path(), "/account");
    assert!(snap.text.contains("営業二部"), "{}", snap.text);
    assert!(
        !snap.text.contains("tok-4f9a"),
        "cookie の値はスナップショットに出ない"
    );
    assert_eq!(
        s.cookie_summary(),
        vec![("portal.example.com".to_string(), 1)]
    );

    // 戻る: 保持した応答を表示し直し、POST を再送しない（§5.3）。
    let before = log.lock().unwrap().len();
    let back = s.act(Action::Back).await.unwrap();
    assert_eq!(back.url.path(), "/login");
    assert_eq!(log.lock().unwrap().len(), before, "back で要求を送らない");
    assert_eq!(hits(&log, "/login"), 2, "GET と POST の 1 回ずつだけ");
    assert!(matches!(
        s.act(Action::Back).await,
        Err(BrowserError::NoHistory)
    ));
}

#[tokio::test]
async fn get_form_link_and_stale_refs() {
    let route: support::Route = Arc::new(|req: &Req| match req.target.as_str() {
        "/catalog" => http(
            "200 OK",
            &[],
            r#"<form action="/catalog/search"><input name="q" aria-label="キーワード">
               <select name="cat" aria-label="分類"><option value="">全部</option><option value="tool">工具</option></select>
               <input type="submit" value="検索"></form>"#,
        ),
        t if t.starts_with("/catalog/search?") => http(
            "200 OK",
            &[],
            &format!("<p>検索: {t}</p><a href=\"/item/a2\">ACME-X42</a>"),
        ),
        "/item/a2" => http("200 OK", &[], "<dl><dt>税込価格</dt><dd>¥18,640</dd></dl>"),
        _ => http("404 Not Found", &[], ""),
    });
    let (mut s, port, _log) = session(route).await;
    let snap = s
        .navigate(u(port, "shop.example.com", "/catalog"))
        .await
        .unwrap();
    let kw = find(&snap, "キーワード");
    s.act(Action::Fill(kw, "X42 ドリル".into())).await.unwrap();
    s.act(Action::Select(find(&snap, "分類"), "工具".into()))
        .await
        .unwrap();
    let snap = s.act(Action::Submit(kw)).await.unwrap();
    assert!(
        snap.text.contains("q=X42+ドリル&cat=tool") || snap.text.contains("q=X42+%E3%83%89"),
        "{}",
        snap.text
    );
    // 古い世代の ref は拒否する。
    assert!(matches!(
        s.act(Action::Click(kw)).await,
        Err(BrowserError::StaleRef(_))
    ));
    let link = find(&snap, "ACME-X42");
    assert!(
        s.provenance()
            .allows(&u(port, "shop.example.com", "/item/a2")),
        "現れたリンクは出所に入る"
    );
    let snap = s.act(Action::Click(link)).await.unwrap();
    assert!(snap.text.contains("¥18,640"));
}

#[tokio::test]
async fn same_site_strict_cookie_is_not_sent_from_another_site() {
    let route: support::Route = Arc::new(|req: &Req| {
        match (req.host.as_str(), req.target.as_str()) {
            ("bank.example.com", "/") => http(
                "200 OK",
                &[
                    ("Set-Cookie", "s=strict; SameSite=Strict; Path=/"),
                    ("Set-Cookie", "l=lax; Path=/"),
                ],
                "<p>銀行</p>",
            ),
            ("attacker.example.net", "/") => http(
                "200 OK",
                &[],
                &format!(
                    r#"<form method="post" action="http://bank.example.com:{}/transfer"><input name="to" value="x" aria-label="宛先"><button>送る</button></form>
                   <a href="http://bank.example.com:{}/me">銀行へ</a>"#,
                    req.port, req.port
                ),
            ),
            (_, "/transfer" | "/me") => http("200 OK", &[], "<p>受け付けました</p>"),
            _ => http("404 Not Found", &[], ""),
        }
    });
    let (mut s, port, log) = session(route).await;
    s.navigate(u(port, "bank.example.com", "/")).await.unwrap();
    let snap = s
        .navigate(u(port, "attacker.example.net", "/"))
        .await
        .unwrap();

    // 別サイトからのリンク（トップレベル GET）: Lax は付くが Strict は付かない。
    s.act(Action::Click(find(&snap, "銀行へ"))).await.unwrap();
    let me = log
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|r| r.target == "/me")
        .cloned()
        .unwrap();
    assert_eq!(me.cookie.as_deref(), Some("l=lax"));

    // 別サイトからの POST: どちらも付かない（D 経由だけの CSRF を作らない）。
    let snap = s
        .navigate(u(port, "attacker.example.net", "/"))
        .await
        .unwrap();
    s.act(Action::Click(find(&snap, "送る"))).await.unwrap();
    let transfer = log
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|r| r.target == "/transfer")
        .cloned()
        .unwrap();
    assert_eq!(transfer.cookie, None);
}

#[tokio::test]
async fn redirect_rules() {
    let route: support::Route = Arc::new(|req: &Req| {
        match (req.method.as_str(), req.target.as_str()) {
            (_, "/form") => http(
                "200 OK",
                &[],
                r#"<form method="post" action="/submit307"><input name="v" value="secret" aria-label="値"><button>送307</button></form>
               <form method="post" action="/submit303"><input name="v" value="x" aria-label="値2"><button>送303</button></form>
               <a href="/to-private">私的</a><a href="/loop">ループ</a><a href="/pdf">PDF</a><button type="button">JSボタン</button>"#,
            ),
            ("POST", "/submit307") => http(
                "307 Temporary Redirect",
                &[("Location", "http://other.example.org/collect")],
                "",
            ),
            ("POST", "/submit303") => http("303 See Other", &[("Location", "/done")], ""),
            ("GET", "/done") => http("200 OK", &[], "<p>完了</p>"),
            (_, "/to-private") => http("302 Found", &[("Location", "http://127.0.0.1/admin")], ""),
            (_, "/loop") => http("302 Found", &[("Location", "/loop")], ""),
            (_, "/pdf") => http("200 OK", &[("Content-Type", "application/pdf")], "%PDF-1.7"),
            _ => http("404 Not Found", &[], ""),
        }
    });
    let (mut s, port, log) = session(route).await;
    let form = || u(port, "app.example.com", "/form");

    // 307 で別オリジンへ転送される POST は本文を再送せずに止める。
    let snap = s.navigate(form()).await.unwrap();
    let err = s
        .act(Action::Click(find(&snap, "送307")))
        .await
        .unwrap_err();
    assert!(
        matches!(err, BrowserError::RedirectBlocked { .. }),
        "{err:?}"
    );
    assert_eq!(hits(&log, "/collect"), 0, "転送先へは 1 度も送らない");

    // 303 は本文なしの GET になる。
    let snap = s.navigate(form()).await.unwrap();
    let done = s.act(Action::Click(find(&snap, "送303"))).await.unwrap();
    assert!(done.text.contains("完了"));
    assert!(log
        .lock()
        .unwrap()
        .iter()
        .any(|r| r.method == "GET" && r.target == "/done" && r.body.is_empty()));

    // 私的アドレス（IP 直指定）へのリダイレクトは関門で拒否する。
    let snap = s.navigate(form()).await.unwrap();
    let err = s.act(Action::Click(find(&snap, "私的"))).await.unwrap_err();
    assert!(matches!(err, BrowserError::Rejected(_)), "{err:?}");

    // リダイレクトの上限。
    let snap = s.navigate(form()).await.unwrap();
    let err = s
        .act(Action::Click(find(&snap, "ループ")))
        .await
        .unwrap_err();
    assert_eq!(err, BrowserError::TooManyRedirects);
    assert_eq!(hits(&log, "/loop"), 6, "最初の 1 回＋リダイレクト 5 回");

    // HTML 以外は web_fetch に回す。
    let snap = s.navigate(form()).await.unwrap();
    let err = s.act(Action::Click(find(&snap, "PDF"))).await.unwrap_err();
    assert!(matches!(err, BrowserError::NotHtml { .. }), "{err:?}");

    // JS でしか動かないボタン。
    let snap = s.navigate(form()).await.unwrap();
    let js = snap
        .elements
        .iter()
        .find(|e| e.label == "JSボタン")
        .unwrap();
    assert_eq!(js.kind, ElementKind::Button { submits: false });
    let err = s.act(Action::Click(js.id)).await.unwrap_err();
    assert!(matches!(err, BrowserError::NeedsJs { .. }), "{err:?}");
}

#[tokio::test]
async fn production_guard_rejects_loopback_and_bad_urls() {
    let route: support::Route = Arc::new(|_: &Req| http("200 OK", &[], "<p>x</p>"));
    let (addr, log) = serve(route).await;
    // 本番の検証（公開 IP のみ）では、ループバックに解決される名前は開けない。
    let engine = TextEngine::new(Arc::new(ToStub(addr)), TextLimits::default());
    let mut s = BrowserSession::new(ctx(), Arc::new(engine), []);
    let err = s
        .navigate(u(addr.port(), "rebind.example.com", "/"))
        .await
        .unwrap_err();
    assert!(matches!(err, BrowserError::Rejected(_)), "{err:?}");
    for bad in [
        "http://localhost/",
        "http://intranet/",
        "file:///etc/passwd",
    ] {
        let err = s.navigate(Url::parse(bad).unwrap()).await.unwrap_err();
        assert!(matches!(err, BrowserError::Rejected(_)), "{bad}: {err:?}");
    }
    assert!(log.lock().unwrap().is_empty(), "1 度も接続しない");
    assert!(matches!(
        s.act(Action::Back).await,
        Err(BrowserError::NoHistory | BrowserError::NoPage)
    ));
}

#[tokio::test]
async fn shift_jis_pages_are_decoded_and_spa_is_flagged() {
    let route: support::Route = Arc::new(|req: &Req| match req.target.as_str() {
        "/sjis" => {
            // 「総務省」を Shift_JIS で。
            let mut v = b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=Shift_JIS\r\nConnection: close\r\nContent-Length: 13\r\n\r\n<p>".to_vec();
            v.extend_from_slice(&[0x91, 0x8d, 0x96, 0xb1, 0x8f, 0xc8]);
            v.extend_from_slice(b"</p>");
            v
        }
        _ => http(
            "200 OK",
            &[],
            r#"<html><head><script type="module" src="/app.js"></script></head><body><div id="root"></div></body></html>"#,
        ),
    });
    let (mut s, port, _) = session(route).await;
    let snap = s
        .navigate(u(port, "www.example.go.jp", "/sjis"))
        .await
        .unwrap();
    assert!(snap.text.contains("総務省"), "{}", snap.text);
    let spa = s
        .navigate(u(port, "app.example.com", "/spa"))
        .await
        .unwrap();
    assert_eq!(spa.needs_js, Some(browser::NeedsJs::ScriptRendered));
}

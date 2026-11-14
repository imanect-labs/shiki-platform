//! テキストエンジンのフォーム送信の規則（HTML 標準の form submission）と、送る前の判定（plan）・
//! リダイレクトの照合・取得の期限の結合テスト。

#![allow(clippy::panic, clippy::unwrap_used)]

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use browser::{
    Action, BrowserError, BrowserSession, Method, RefId, Snapshot, TextEngine, TextLimits,
};
use support::{allow_loopback, ctx, http, serve, Req, ToStub};
use url::Url;

type Log = Arc<Mutex<Vec<Req>>>;

fn find(snap: &Snapshot, label: &str) -> RefId {
    snap.elements
        .iter()
        .find(|e| e.label == label)
        .unwrap_or_else(|| panic!("「{label}」が無い: {}", snap.text))
        .id
}

async fn start(route: support::Route, limits: TextLimits) -> (BrowserSession, u16, Log) {
    let (addr, log) = serve(route).await;
    let engine = TextEngine::with_guard(Arc::new(ToStub(addr)), allow_loopback, limits);
    (
        BrowserSession::new(ctx(), Arc::new(engine), []),
        addr.port(),
        log,
    )
}

fn u(port: u16, host: &str, path: &str) -> Url {
    Url::parse(&format!("http://{host}:{port}{path}")).unwrap()
}

fn last(log: &Log, path: &str) -> Req {
    log.lock()
        .unwrap()
        .iter()
        .rev()
        .find(|r| r.target.starts_with(path))
        .cloned()
        .unwrap_or_else(|| panic!("{path} への要求が無い"))
}

const FORM_PAGE: &str = r#"
<form id="main" method="post" action="/save">
  <input type="hidden" name="csrf" value="T1">
  <div style="display:none"><input name="token" value="T2"></div>
  <input name="a" value="1" disabled aria-label="無効欄">
  <fieldset disabled><input name="b" value="2" aria-label="囲まれた無効欄"></fieldset>
  <label>件名 <input name="q"></label>
  <select multiple name="tags" aria-label="タグ"><option value="x">エックス</option><option value="y">ワイ</option></select>
  <select name="none" aria-label="空"></select>
  <button name="op" value="save">保存</button>
  <button name="op" value="del" formaction="/delete" formmethod="get">削除</button>
  <button name="op" value="ng" disabled>押せない</button>
</form>
<button form="main" name="op" value="outside">外から送信</button>
<form method="post" action="/other"><input name="z" aria-label="別"><button disabled>既定は無効</button><button>二番目</button></form>
"#;

fn form_route() -> support::Route {
    Arc::new(|req: &Req| match req.target.as_str() {
        "/form" => http("200 OK", &[], FORM_PAGE),
        _ => http("200 OK", &[], "<p>受信</p>"),
    })
}

#[tokio::test]
async fn submission_follows_html_rules() {
    let (mut s, port, log) = start(form_route(), TextLimits::default()).await;
    let snap = s
        .navigate(u(port, "app.example.com", "/form"))
        .await
        .unwrap();
    assert!(
        !snap.text.contains("T2"),
        "見えない項目は表示しない: {}",
        snap.text
    );
    assert!(
        snap.text.contains(r#"button "押せない" (無効)"#),
        "{}",
        snap.text
    );
    assert!(
        snap.text.contains(r#"button "外から送信"]"#),
        "form 属性の送信ボタンは押せる: {}",
        snap.text
    );

    // 無効な部品は入力も押下もできない。
    assert!(matches!(
        s.act(Action::Fill(find(&snap, "無効欄"), "x".into())).await,
        Err(BrowserError::NotInteractive(_))
    ));
    assert!(matches!(
        s.act(Action::Click(find(&snap, "押せない"))).await,
        Err(BrowserError::NotInteractive(_))
    ));

    s.act(Action::Fill(find(&snap, "件名"), "題".into()))
        .await
        .unwrap();
    s.act(Action::Select(find(&snap, "タグ"), "x".into()))
        .await
        .unwrap();
    let snap = s
        .act(Action::Select(find(&snap, "タグ"), "ワイ".into()))
        .await
        .unwrap();
    assert!(snap.text.contains(r#"value=["x", "y"]"#), "{}", snap.text);

    // Enter での送信は最初の送信ボタン（保存）を submitter にする。無効な部品・空のセレクトは載らず、
    // CSS で隠れた項目は載る。
    let planned = s
        .plan(&Action::Submit(find(&snap, "件名")))
        .unwrap()
        .unwrap();
    assert_eq!(planned.request.method, Method::Post);
    s.act(Action::Submit(find(&snap, "件名"))).await.unwrap();
    assert_eq!(
        last(&log, "/save").body,
        "csrf=T1&token=T2&q=%E9%A1%8C&tags=x&tags=y&op=save"
    );

    // formaction / formmethod は押したボタンが上書きする。
    let snap = s
        .navigate(u(port, "app.example.com", "/form"))
        .await
        .unwrap();
    let planned = s
        .plan(&Action::Click(find(&snap, "削除")))
        .unwrap()
        .unwrap();
    assert_eq!(planned.request.method, Method::Get);
    assert_eq!(planned.request.url.path(), "/delete");
    s.act(Action::Click(find(&snap, "削除"))).await.unwrap();
    let del = last(&log, "/delete");
    assert_eq!(del.method, "GET");
    assert!(del.target.ends_with("&op=del"), "{}", del.target);

    // form="id" でフォームの外にある送信ボタン。
    let snap = s
        .navigate(u(port, "app.example.com", "/form"))
        .await
        .unwrap();
    s.act(Action::Click(find(&snap, "外から送信")))
        .await
        .unwrap();
    assert!(last(&log, "/save").body.ends_with("op=outside"));

    // 既定の送信ボタン（最初の送信ボタン）が無効なら Enter では送らない。
    let snap = s
        .navigate(u(port, "app.example.com", "/form"))
        .await
        .unwrap();
    let err = s.act(Action::Submit(find(&snap, "別"))).await.unwrap_err();
    assert!(matches!(err, BrowserError::NotInteractive(_)), "{err:?}");
}

#[tokio::test]
async fn plan_reports_target_without_sending() {
    let route: support::Route = Arc::new(|req: &Req| match req.target.as_str() {
        "/login" => http(
            "200 OK",
            &[],
            r#"<form method="post" action="https://sso.example.org/auth"><input name="u" aria-label="ID">
               <input type="password" name="p" aria-label="パスワード"><button>入る</button></form><a href="/next">次</a>"#,
        ),
        _ => http("200 OK", &[], "<p>x</p>"),
    });
    let (mut s, port, log) = start(route, TextLimits::default()).await;
    assert!(matches!(s.plan(&Action::Back), Err(BrowserError::NoPage)));
    let snap = s
        .navigate(u(port, "app.example.com", "/login"))
        .await
        .unwrap();
    let before = log.lock().unwrap().len();
    let planned = s
        .plan(&Action::Click(find(&snap, "入る")))
        .unwrap()
        .unwrap();
    assert_eq!(planned.request.method, Method::Post);
    assert_eq!(planned.request.url.host_str(), Some("sso.example.org"));
    assert_eq!(planned.masked_fields, vec!["p".to_string()]);
    let link = s.plan(&Action::Click(find(&snap, "次"))).unwrap().unwrap();
    assert_eq!(link.request.method, Method::Get);
    assert!(s
        .plan(&Action::Fill(find(&snap, "ID"), "x".into()))
        .unwrap()
        .is_none());
    assert_eq!(log.lock().unwrap().len(), before, "plan は送らない");
}

#[tokio::test]
async fn redirect_check_and_cross_site_chain() {
    let route: support::Route =
        Arc::new(|req: &Req| match (req.host.as_str(), req.target.as_str()) {
            ("site.example.com", "/") => http(
                "200 OK",
                &[("Set-Cookie", "s=1; SameSite=Strict; Path=/")],
                &format!(
                    r#"<a href="http://evil.example.net:{}/r">外へ</a>"#,
                    req.port
                ),
            ),
            ("evil.example.net", "/r") => http(
                "302 Found",
                &[(
                    "Location",
                    &format!("http://site.example.com:{}/account", req.port),
                )],
                "",
            ),
            (_, "/account") => http("200 OK", &[], "<p>アカウント</p>"),
            (_, "/doc") => http(
                "200 OK",
                &[(
                    "Content-Type",
                    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                )],
                "PK\u{3}\u{4}",
            ),
            (_, "/slow") => {
                std::thread::sleep(Duration::from_millis(400));
                http("302 Found", &[("Location", "/slow")], "")
            }
            _ => http("404 Not Found", &[], ""),
        });

    // 連鎖の途中で別サイトを通ったら、戻ってきても Strict の cookie は付かない。
    let (mut s, port, log) = start(route.clone(), TextLimits::default()).await;
    let snap = s.navigate(u(port, "site.example.com", "/")).await.unwrap();
    s.act(Action::Click(find(&snap, "外へ"))).await.unwrap();
    assert_eq!(last(&log, "/account").cookie, None);

    // 転送先の照合（出所の規則）を差し込める。拒否したら転送先へは送らない。
    let (addr, log2) = serve(route.clone()).await;
    let engine = TextEngine::with_guard(
        Arc::new(ToStub(addr)),
        allow_loopback,
        TextLimits::default(),
    );
    let mut s = BrowserSession::new(ctx(), Arc::new(engine), []).with_redirect_check(Arc::new(
        |to: &Url| to.host_str() != Some("site.example.com"),
    ));
    let snap = s
        .navigate(u(addr.port(), "site.example.com", "/"))
        .await
        .unwrap();
    let err = s.act(Action::Click(find(&snap, "外へ"))).await.unwrap_err();
    assert!(
        matches!(err, BrowserError::RedirectNotAllowed { .. }),
        "{err:?}"
    );
    assert!(log2.lock().unwrap().iter().all(|r| r.target != "/account"));

    // Office 文書（Content-Type に xml を含む）は HTML として読まない。
    let err = s
        .navigate(u(addr.port(), "site.example.com", "/doc"))
        .await
        .unwrap_err();
    assert!(matches!(err, BrowserError::NotHtml { .. }), "{err:?}");

    // 期限はリダイレクトを含めた取得全体にかかる。
    let limits = TextLimits {
        timeout: Duration::from_secs(1),
        ..TextLimits::default()
    };
    let (mut s, port, _) = start(route, limits).await;
    let started = std::time::Instant::now();
    let err = s
        .navigate(u(port, "slow.example.com", "/slow"))
        .await
        .unwrap_err();
    assert!(matches!(err, BrowserError::Fetch(_)), "{err:?}");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

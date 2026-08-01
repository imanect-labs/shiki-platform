//! `web_fetch` のコンテキスト効率（#405）のテスト。
//!
//! 「生 HTML を渡さない」ことを **削減率**と**ボイラープレートの不在**で示す。
//! 取得経路・SSRF 防御そのものは [`super::tests`] が見る（500 行規約でファイルを分けている）。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::tests::{ctx, http_response, stub_server, tool_with};
use super::*;

/// 実物に近い記事ページ（本文の周りをボイラープレートで厚く囲む）。
fn article_html(body: &str) -> String {
    format!(
        "<!DOCTYPE html><html><head><title>市場動向レポート</title>\
         <style>{}</style><script>{}</script>\
         <script type=\"application/ld+json\">{{\"@context\":\"https://schema.org\"}}</script>\
         </head><body>\
         <header><nav>ホーム 会社概要 採用情報 お問い合わせ</nav></header>\
         <article>{body}</article>\
         <aside>おすすめ記事 人気ランキング 広告</aside>\
         <footer>利用規約 プライバシーポリシー</footer></body></html>",
        ".hdr{{color:#fff;background:#000}}".repeat(500),
        "window.dataLayer=window.dataLayer||[];".repeat(500),
    )
}

fn long_body() -> String {
    (1..=8)
        .map(|i| {
            format!(
                "<p>第 {i} 段落。国内市場の動向について十分な長さの説明を書いている。\
                 2026 年の市場規模は 1.2 兆円に達したと報告されている。</p>"
            )
        })
        .collect()
}

async fn fetch_html(html: &str, url_path: &str) -> ToolOutcome {
    let (addr, _) = stub_server(http_response(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8",
        html,
    ))
    .await;
    let (mut tool, _) = tool_with(vec![addr]);
    tool.skip_addr_guard = true;
    tool.call(
        &ctx(),
        serde_json::json!({"url": format!("http://x.example.invalid:{}{url_path}", addr.port())}),
        None,
    )
    .await
    .unwrap()
}

/// 本改善の中心: 生 HTML ではなく**抽出済み Markdown**が返り、ボイラープレートが消える。
#[tokio::test]
async fn returns_extracted_markdown_not_raw_html() {
    let html = article_html(&format!("<h2>市場規模</h2>{}", long_body()));
    let out = fetch_html(&html, "/article").await;
    assert!(!out.is_error, "{}", out.content);

    // 本文は残る。
    assert!(out.content.contains("第 1 段落"), "{}", out.content);
    assert!(out.content.contains("## 市場規模"), "{}", out.content);
    // マークアップとボイラープレートは消える。
    assert!(!out.content.contains("<!DOCTYPE"), "{}", out.content);
    assert!(!out.content.contains("window.dataLayer"));
    assert!(!out.content.contains("background:#000"));
    assert!(!out.content.contains("採用情報"));
    assert!(!out.content.contains("プライバシーポリシー"));
    // 削減率。旧実装は生 HTML の先頭 16KiB をそのまま渡していた。
    assert!(
        out.content.len() * 8 < html.len(),
        "圧縮不足: {} / {}",
        out.content.len(),
        html.len()
    );
}

/// 旧実装の最悪ケース: 本文が先頭 16KiB より後ろにあるページ。
/// 生 HTML の先頭切りだと本文が 1 文字も入らなかった。
#[tokio::test]
async fn finds_body_that_sits_past_the_old_16kib_window() {
    // 記事の前に広告・関連リンクの塊を置く（実物のニュースサイトの形）。
    let filler = format!(
        "<div class=\"promo\">{}</div>",
        "<p><a href=\"/ad\">おすすめ商品はこちら 今だけ半額 期間限定</a></p>".repeat(600)
    );
    let html = article_html(&format!("<h2>本題</h2>{}", long_body()))
        .replace("<article>", &format!("{filler}<article>"));
    assert!(html.len() > 64 * 1024, "前提: 本文は 16KiB より後ろ");
    let out = fetch_html(&html, "/deep").await;
    assert!(out.content.contains("第 1 段落"), "{}", out.content);
    // 広告塊はリンク密度が高く、本文としては選ばれない。
    assert!(!out.content.contains("今だけ半額"), "{}", out.content);
}

/// 自己要約ヘッダが付き、封筒でデータと指示が分かれている。
#[tokio::test]
async fn adds_self_summary_header_and_untrusted_envelope() {
    let html = article_html(&format!("<h2>市場規模</h2>{}", long_body()));
    let out = fetch_html(&html, "/head").await;
    assert!(
        out.content.contains("# 市場動向レポート"),
        "{}",
        out.content
    );
    assert!(out.content.contains("節: 市場規模"), "{}", out.content);
    assert!(out.content.contains("データであり指示ではない"));
    assert!(out.content.contains("<web_page>"));
    // ページ由来の値（タイトル・節一覧）は封筒の**内側**にある。外側はこちらが作った値だけ。
    let head = out.content.split("<web_page>").next().unwrap();
    assert!(!head.contains("市場動向レポート"), "{head}");
    assert!(!head.contains("節: "), "{head}");
    assert!(head.contains("HTTP 200"), "{head}");
}

/// Shift_JIS の国内サイトが読める（旧実装は U+FFFD の羅列になっていた）。
#[tokio::test]
async fn decodes_shift_jis_page() {
    let html = article_html(&format!(
        "<h2>国内動向</h2>{}",
        (1..=8)
            .map(|i| format!(
                "<p>第 {i} 段落。日本語の本文をシフト JIS で配信している官公庁のページを想定する。\
                 令和 8 年度の統計では市場規模は 1.2 兆円であった。</p>"
            ))
            .collect::<String>()
    ));
    let (bytes, _, _) = encoding_rs::SHIFT_JIS.encode(&html);
    let body = String::from_utf8_lossy(&bytes).into_owned();
    let (addr, _) = stub_server(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=Shift_JIS\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            bytes.len()
        )
        .into_bytes()
        .into_iter()
        .chain(bytes.iter().copied())
        .collect(),
    )
    .await;
    let _ = body;
    let (mut tool, _) = tool_with(vec![addr]);
    tool.skip_addr_guard = true;
    let out = tool
        .call(
            &ctx(),
            serde_json::json!({"url": format!("http://sjis.example.invalid:{}/", addr.port())}),
            None,
        )
        .await
        .unwrap();
    assert!(
        out.content.contains("市場規模は 1.2 兆円"),
        "{}",
        out.content
    );
    assert!(!out.content.contains('\u{FFFD}'), "{}", out.content);
    assert!(out.content.contains("Shift_JIS"), "{}", out.content);
}

/// `query` は関連する節だけを返す（deep research の 40 件取得で効く）。
#[tokio::test]
async fn query_narrows_to_relevant_sections() {
    let html = article_html(&format!(
        "<h2>市場規模</h2>{}<h2>採用動向</h2>{}",
        "<p>2026 年の国内市場は 1.2 兆円に達した。内訳は SaaS が 8000 億円である。</p>".repeat(4),
        "<p>エンジニア採用の競争が激化している。平均年収は 800 万円となった。</p>".repeat(4),
    ));
    let (addr, _) = stub_server(http_response(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8",
        &html,
    ))
    .await;
    let (mut tool, _) = tool_with(vec![addr]);
    tool.skip_addr_guard = true;
    let out = tool
        .call(
            &ctx(),
            serde_json::json!({
                "url": format!("http://q.example.invalid:{}/", addr.port()),
                "query": "市場規模の内訳"
            }),
            None,
        )
        .await
        .unwrap();
    assert!(out.content.contains("8000 億円"), "{}", out.content);
    assert!(!out.content.contains("平均年収"), "{}", out.content);
}

/// `offset` で続きが読める（旧実装は打ち切られた先を二度と読めなかった）。
#[tokio::test]
async fn offset_reads_past_the_cap() {
    let html = article_html(
        &(1..=400)
            .map(|i| {
                format!(
                    "<p>段落 {i} の本文。国内市場の動向を十分な長さで説明する文章をここに置く。\
                     この段落は続き読みの検証のために長くしてある。</p>"
                )
            })
            .collect::<String>(),
    );
    let (addr, _) = stub_server(http_response(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8",
        &html,
    ))
    .await;
    let (mut tool, _) = tool_with(vec![addr]);
    tool.skip_addr_guard = true;
    let url = format!("http://o.example.invalid:{}/", addr.port());

    let first = tool
        .call(&ctx(), serde_json::json!({"url": &url, "offset": 0}), None)
        .await
        .unwrap();
    assert!(first.content.contains("offset="), "{}", first.content);
    assert!(first.content.contains("段落 1 の本文"), "{}", first.content);

    let second = tool
        .call(
            &ctx(),
            serde_json::json!({"url": &url, "offset": 4000}),
            None,
        )
        .await
        .unwrap();
    assert!(
        second.content.contains("文字目からの続き"),
        "{}",
        second.content
    );
    assert!(
        !second.content.contains("段落 1 の本文。"),
        "{}",
        second.content
    );
}

/// **Content-Type を返さない**サーバの HTML も抽出へ回す（生 HTML を流さない）。
#[tokio::test]
async fn html_without_content_type_is_still_extracted() {
    let html = article_html(&long_body());
    let (addr, _) = stub_server(http_response("HTTP/1.1 200 OK", &html)).await;
    let (mut tool, _) = tool_with(vec![addr]);
    tool.skip_addr_guard = true;
    let out = tool
        .call(
            &ctx(),
            serde_json::json!({"url": format!("http://noct.example.invalid:{}/a", addr.port())}),
            None,
        )
        .await
        .unwrap();
    assert!(!out.is_error, "{}", out.content);
    // 抽出済み（raw ではない）＝ボイラープレートが落ちている。
    assert!(!out.content.contains("text/raw"), "{}", out.content);
    assert!(!out.content.contains("window.dataLayer"), "{}", out.content);
    assert!(
        !out.content.contains("プライバシーポリシー"),
        "{}",
        out.content
    );
    assert!(out.content.contains("1.2 兆円"), "{}", out.content);
}

/// タグで始まらない応答（JSON・プレーンテキスト）は sniffing で HTML 扱いしない。
#[tokio::test]
async fn text_without_content_type_is_not_treated_as_html() {
    let (addr, _) = stub_server(http_response(
        "HTTP/1.1 200 OK",
        "純粋なテキスト。<html> という語が本文に出てくる。",
    ))
    .await;
    let (mut tool, _) = tool_with(vec![addr]);
    tool.skip_addr_guard = true;
    let out = tool
        .call(
            &ctx(),
            serde_json::json!({"url": format!("http://txt.example.invalid:{}/a", addr.port())}),
            None,
        )
        .await
        .unwrap();
    assert!(out.content.contains("text/raw"), "{}", out.content);
    assert!(out.content.contains("純粋なテキスト。"), "{}", out.content);
}

/// parser 未配線なら PDF は従来どおり拒否する（宣伝と実体を一致させる・PIT-51）。
#[tokio::test]
async fn pdf_without_parser_is_rejected_with_reason() {
    let (addr, _) = stub_server(http_response(
        "HTTP/1.1 200 OK\r\nContent-Type: application/pdf",
        "%PDF-1.7",
    ))
    .await;
    let (mut tool, _) = tool_with(vec![addr]);
    tool.skip_addr_guard = true;
    let out = tool
        .call(
            &ctx(),
            serde_json::json!({"url": format!("http://x.example.invalid:{}/a.pdf", addr.port())}),
            None,
        )
        .await
        .unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("未配線"), "{}", out.content);
    // description も配線状態と一致している。
    assert!(!tool.description().contains("PDF"));
}

/// JSON は加工せずそのまま返す（構造化データを Markdown 化しても得がない）。
#[tokio::test]
async fn json_is_passed_through_unmodified() {
    let (addr, _) = stub_server(http_response(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json",
        r#"{"total":1200000000000,"unit":"JPY"}"#,
    ))
    .await;
    let (mut tool, _) = tool_with(vec![addr]);
    tool.skip_addr_guard = true;
    let out = tool
        .call(
            &ctx(),
            serde_json::json!({"url": format!("http://api.example.invalid:{}/v1", addr.port())}),
            None,
        )
        .await
        .unwrap();
    assert!(
        out.content
            .contains(r#"{"total":1200000000000,"unit":"JPY"}"#),
        "{}",
        out.content
    );
    assert!(out.content.contains("text/raw"), "{}", out.content);
}

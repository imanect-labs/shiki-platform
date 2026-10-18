//! Word エンジンの実機結合テスト（#725）。`engine_calc_it.rs` と同じ環境変数で走る
//! （`OFFICE_ENGINE_TEST_WS` / `OFFICE_ENGINE_TEST_WOPI` / `OFFICE_ENGINE_TEST_FILES`）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use office::engine::writer::{normalize::normalize_doc_ops, run};
use office::live::{CoolWsClient, CoolWsConfig};
use serde_json::json;

struct Env {
    ws: String,
    wopi: String,
    files: String,
}

fn env() -> Option<Env> {
    Some(Env {
        ws: std::env::var("OFFICE_ENGINE_TEST_WS").ok()?,
        wopi: std::env::var("OFFICE_ENGINE_TEST_WOPI").ok()?,
        files: std::env::var("OFFICE_ENGINE_TEST_FILES").ok()?,
    })
}

async fn open(env: &Env, name: &str) -> CoolWsClient {
    let mut cfg = CoolWsConfig::new(env.ws.clone());
    cfg.total_deadline = std::time::Duration::from_mins(5);
    office::live::connect_with_retry(&cfg, &format!("{}/{name}", env.wopi), "test-token")
        .await
        .unwrap()
}

/// 一覧のテキストから、本文が `needle` で始まる最初のブロックの ID を取る。
fn id_of(overview: &str, needle: &str) -> String {
    let line = overview
        .lines()
        .find(|l| l.contains(needle))
        .and_then(|l| l.strip_prefix('['))
        .and_then(|l| l.split_once(']'));
    match line {
        Some((id, _)) => id.to_string(),
        None => panic!("{needle} が一覧に無い: {overview}"),
    }
}

#[tokio::test]
async fn builds_a_report_and_survives_a_docx_round_trip() {
    let Some(env) = env() else {
        eprintln!("skip: OFFICE_ENGINE_TEST_* 未設定");
        return;
    };
    let name = format!("engine-{}.docx", uuid::Uuid::new_v4());
    std::fs::write(
        format!("{}/{name}", env.files),
        office::blank_template(office::OfficeKind::Document),
    )
    .unwrap();
    let mut client = open(&env, &name).await;
    let ops = normalize_doc_ops(&[
        json!({"op": "insert", "at": "start", "html": "\
            <style>.lead { color: #595959 } table.t th { background: #1F3864; color: #FFFFFF } table.t td { border: 1px solid #808080 }</style>\
            <h1 data-style=\"Title\">2026年度 事業報告</h1>\
            <p class=\"lead\">経営企画部</p>\
            <h2 data-style=\"Heading 1\">1. 概要</h2>\
            <p>売上高は 24億円となった。</p>\
            <h2 data-style=\"Heading 1\">2. 部門別</h2>\
            <table class=\"t\"><tr><th>部門</th><th>売上</th></tr><tr><td rowspan=\"2\">国内</td><td>1,200</td></tr><tr><td>300</td></tr></table>\
            <ul><li>新製品</li><li>コスト最適化</li></ul>"}),
        json!({"op": "set_style", "name": "Heading 1", "font": {"color": "#1F3864"}}),
        json!({"op": "footer", "content": {"text": "- &[Page] -", "align": "center"}}),
        json!({"op": "replace_text", "find": "24億円", "replace": "25億円"}),
        json!({"op": "insert_toc", "at": "start", "levels": 2}),
    ])
    .unwrap();
    let out = run::apply(&mut client, "報告書", &ops).await.unwrap();
    eprintln!("{}", out.text);
    assert!(out.applied, "{}", out.text);
    assert!(
        out.text.contains("Title「2026年度 事業報告」"),
        "Title スタイル: {}",
        out.text
    );
    assert!(out.text.contains("Heading 1「1. 概要」"), "{}", out.text);
    assert!(out.text.contains("目次（"), "{}", out.text);
    // 新規のテンプレートは A4（日本の業務文書の既定）。
    assert!(out.text.contains("A4"), "用紙: {}", out.text);
    client.save().await.unwrap();
    client.close().await;

    // 開き直して、docx から読み込んだ状態を確かめる。
    let mut client = open(&env, &name).await;
    let overview = run::read_overview(&mut client, &name, 0).await.unwrap();
    eprintln!("{overview}");
    assert!(overview.contains("フッター「- &[Page] -」"), "{overview}");
    assert!(
        overview.contains("「売上高は 25億円となった。」"),
        "{overview}"
    );
    assert!(overview.contains("表 3行×2列"), "{overview}");
    assert!(overview.contains("箇条書き1「新製品」"), "{overview}");
    let lead = id_of(&overview, "「経営企画部」");
    let table = id_of(&overview, "表 3行");
    let detail = run::read_blocks(&mut client, &lead, Some(&table))
        .await
        .unwrap();
    eprintln!("{detail}");
    assert!(
        detail.contains("color: #595959"),
        "クラスの色がインライン化されて残る: {detail}"
    );
    assert!(
        detail.contains("background-color: #1F3864"),
        "見出しセルの塗り: {detail}"
    );
    client.close().await;
}

#[tokio::test]
async fn a_stale_block_id_rolls_back_the_whole_batch() {
    let Some(env) = env() else {
        eprintln!("skip: OFFICE_ENGINE_TEST_* 未設定");
        return;
    };
    let name = format!("engine-{}.docx", uuid::Uuid::new_v4());
    std::fs::write(
        format!("{}/{name}", env.files),
        office::blank_template(office::OfficeKind::Document),
    )
    .unwrap();
    let mut client = open(&env, &name).await;
    let ops = normalize_doc_ops(&[
        json!({"op": "insert", "at": "end", "html": "<p>残ってはいけない</p>"}),
        json!({"op": "delete", "target": "b99-ffff"}),
    ])
    .unwrap();
    let out = run::apply(&mut client, "失敗", &ops).await.unwrap();
    eprintln!("{}", out.text);
    assert!(!out.applied);
    assert!(
        out.text.contains("office.read で読み直して"),
        "{}",
        out.text
    );
    let overview = run::read_overview(&mut client, &name, 0).await.unwrap();
    assert!(!overview.contains("残ってはいけない"), "{overview}");
    client.close().await;
}

/// 新しい白紙の docx を作って開く。
async fn fresh(env: &Env) -> (String, CoolWsClient) {
    let name = format!("engine-{}.docx", uuid::Uuid::new_v4());
    std::fs::write(
        format!("{}/{name}", env.files),
        office::blank_template(office::OfficeKind::Document),
    )
    .unwrap();
    let client = open(env, &name).await;
    (name, client)
}

async fn apply_ok(client: &mut CoolWsClient, ops: &[serde_json::Value]) -> String {
    let ops = normalize_doc_ops(ops).unwrap();
    let out = run::apply(client, "テスト", &ops).await.unwrap();
    assert!(out.applied, "{}", out.text);
    out.text
}

#[tokio::test]
async fn block_ids_stay_on_their_content_through_inserts_and_replacements() {
    let Some(env) = env() else {
        eprintln!("skip: OFFICE_ENGINE_TEST_* 未設定");
        return;
    };
    let (name, mut client) = fresh(&env).await;
    // 白紙の段落を置き換えると、その段落は残らず、全段落がそろう。
    let text = apply_ok(
        &mut client,
        &[json!({"op": "replace", "target": "b0-16ff", "html": "<p>一</p><p>二</p><p>三</p><table><tr><td>A</td></tr></table>"})],
    )
    .await;
    assert!(
        text.contains("「一」") && text.contains("「二」") && text.contains("「三」"),
        "{text}"
    );
    let overview = run::read_overview(&mut client, &name, 0).await.unwrap();
    let first = id_of(&overview, "「一」");
    let last = id_of(&overview, "（空の段落）");
    let table = id_of(&overview, "表 1行");
    // 末尾の段落の後ろへ差し込んだ後でも、同じ一括の中の元の ID は元の段落を指す。
    // 表の隣への差し込み・表の置き換えも通り、表どうしは空の段落で隔てられる。
    let text = apply_ok(
        &mut client,
        &[
            json!({"op": "insert", "after": last, "html": "<p>末尾</p>"}),
            json!({"op": "insert", "after": table, "html": "<table><tr><td>B</td></tr></table>"}),
            json!({"op": "replace", "target": table, "html": "<table><tr><td>C</td></tr></table>"}),
            json!({"op": "format_paragraph", "target": first, "align": "center"}),
        ],
    )
    .await;
    assert!(text.contains("中央揃え「一」"), "{text}");
    assert!(text.contains("「末尾」"), "{text}");
    let tables: Vec<&str> = text.lines().filter(|l| l.contains("] 表 ")).collect();
    assert_eq!(tables.len(), 2, "{text}");
    assert!(
        tables[0].contains("「C」") && tables[1].contains("「B」"),
        "{text}"
    );
    client.close().await;
}

#[tokio::test]
async fn content_goes_around_the_toc_and_deleting_it_removes_it_whole() {
    let Some(env) = env() else {
        eprintln!("skip: OFFICE_ENGINE_TEST_* 未設定");
        return;
    };
    let (name, mut client) = fresh(&env).await;
    apply_ok(
        &mut client,
        &[
            json!({"op": "insert", "at": "start", "html": "<h1 data-style=\"Heading 1\">1. 概要</h1><p>本文</p>"}),
            json!({"op": "insert_toc", "at": "start", "levels": 2}),
        ],
    )
    .await;
    client.save().await.unwrap();
    client.close().await;
    // docx を開き直した目次の前（文書の先頭）へ差し込む。
    let mut client = open(&env, &name).await;
    let text = apply_ok(
        &mut client,
        &[json!({"op": "insert", "at": "start", "html": "<p data-style=\"Title\">表紙</p>"})],
    )
    .await;
    let lines: Vec<&str> = text.lines().filter(|l| l.starts_with("[b")).collect();
    assert!(lines[0].contains("Title「表紙」"), "{text}");
    assert!(lines[1].contains("目次（"), "{text}");
    let toc = id_of(&text, "目次（");
    let text = apply_ok(&mut client, &[json!({"op": "delete", "target": toc})]).await;
    let blocks: Vec<&str> = text.lines().filter(|l| l.starts_with("[b")).collect();
    assert_eq!(blocks.len(), 3, "目次は見出しも項目も残らない: {text}");
    assert!(text.contains("Heading 1「1. 概要」"), "{text}");
    client.close().await;
}

#[tokio::test]
async fn tracked_changes_are_recorded_read_back_and_accepted() {
    let Some(env) = env() else {
        eprintln!("skip: OFFICE_ENGINE_TEST_* 未設定");
        return;
    };
    let (name, mut client) = fresh(&env).await;
    // 失敗した一括の中で記録を有効にしても、記録の状態は元へ戻る（undo に乗らない設定）。
    let failing = normalize_doc_ops(&[
        json!({"op": "track_changes", "enabled": true}),
        json!({"op": "replace_text", "find": "存在しない語", "replace": "x"}),
    ])
    .unwrap();
    let out = run::apply(&mut client, "失敗", &failing).await.unwrap();
    assert!(!out.applied, "{}", out.text);
    let overview = run::read_overview(&mut client, &name, 0).await.unwrap();
    assert!(!overview.contains("変更履歴を記録中"), "{overview}");
    apply_ok(
        &mut client,
        &[json!({"op": "replace", "target": "b0-16ff", "html": "<p>委託料は月額金500,000円とする。</p><p>旧署名欄</p><p>最終段落</p>"})],
    )
    .await;
    let overview = run::read_overview(&mut client, &name, 0).await.unwrap();
    let fee = id_of(&overview, "委託料");
    let old = id_of(&overview, "旧署名欄");
    let last = id_of(&overview, "最終段落");
    let text = apply_ok(
        &mut client,
        &[
            json!({"op": "track_changes", "enabled": true}),
            json!({"op": "replace_text", "find": "500,000", "replace": "550,000"}),
            json!({"op": "insert", "after": fee, "html": "<p>新しい条文</p>"}),
            json!({"op": "delete", "target": old}),
        ],
    )
    .await;
    // 番兵や空の段落は残らない（差し込みの後片付けは履歴に残さない）。
    assert!(
        !text.contains("SHIKI-IMPORT") && !text.contains("空の段落"),
        "{text}"
    );
    assert!(text.contains("未承認の変更"), "{text}");
    let overview = run::read_overview(&mut client, &name, 0).await.unwrap();
    let detail = run::read_blocks(&mut client, &id_of(&overview, "委託料"), Some(&last))
        .await
        .unwrap();
    assert!(
        detail.contains("<ins>550,000</ins><del>500,000</del>"),
        "{detail}"
    );
    assert!(detail.contains("<ins>新しい条文</ins>"), "{detail}");
    assert!(detail.contains("<del>旧署名欄</del>"), "{detail}");
    let text = apply_ok(&mut client, &[json!({"op": "accept_all_changes"})]).await;
    assert!(
        text.contains("「委託料は月額金550,000円とする。」"),
        "{text}"
    );
    assert!(
        !text.contains("旧署名欄") && !text.contains("未承認"),
        "{text}"
    );
    client.close().await;
}

#[tokio::test]
async fn inserted_headings_follow_their_style_and_the_title_page_has_no_header() {
    let Some(env) = env() else {
        eprintln!("skip: OFFICE_ENGINE_TEST_* 未設定");
        return;
    };
    let (name, mut client) = fresh(&env).await;
    apply_ok(
        &mut client,
        &[
            json!({"op": "insert", "at": "start", "html": "<p data-style=\"Title\">表紙</p><p>本文 <span style=\"font-size: 20pt; color: #C00000\">大</span></p>"}),
            json!({"op": "header", "content": {"text": "社外秘"}}),
            json!({"op": "title_page", "enabled": true}),
        ],
    )
    .await;
    let overview = run::read_overview(&mut client, &name, 0).await.unwrap();
    assert!(overview.contains("表紙あり"), "{overview}");
    assert!(overview.contains("ヘッダー「社外秘」"), "{overview}");
    let body = id_of(&overview, "「本文 大」");
    // 段落の間へ差し込んだ見出しに、取り込みの既定の書体・大きさが直接付かない。
    apply_ok(
        &mut client,
        &[json!({"op": "insert", "after": body, "html": "<h2 data-style=\"Heading 2\">見出し</h2><p>次</p>"})],
    )
    .await;
    let overview = run::read_overview(&mut client, &name, 0).await.unwrap();
    let heading = id_of(&overview, "「見出し」");
    let detail = run::read_blocks(&mut client, &heading, None).await.unwrap();
    assert!(
        detail.contains("<p data-style=\"Heading 2\">見出し</p>"),
        "{detail}"
    );
    // 直接書式を消すとスタイルどおりに戻る。
    apply_ok(
        &mut client,
        &[json!({"op": "format_paragraph", "target": body, "clear_formatting": true})],
    )
    .await;
    let detail = run::read_blocks(&mut client, &body, None).await.unwrap();
    assert!(
        detail.contains("<p data-style=\"Standard\">本文 大</p>"),
        "{detail}"
    );
    client.close().await;
}

#[tokio::test]
async fn tables_are_edited_in_place_and_read_back_with_their_look() {
    let Some(env) = env() else {
        eprintln!("skip: OFFICE_ENGINE_TEST_* 未設定");
        return;
    };
    let (name, mut client) = fresh(&env).await;
    apply_ok(
        &mut client,
        &[json!({"op": "insert", "at": "start", "html": "<table><tr><th>地域</th><th>売上</th></tr><tr><td>東</td><td>10</td></tr><tr><td>西</td><td>20</td></tr></table><ul><li>一<ul><li>二</li></ul></li></ul>"})],
    )
    .await;
    let overview = run::read_overview(&mut client, &name, 0).await.unwrap();
    let table = id_of(&overview, "表 3行×2列");
    let text = apply_ok(
        &mut client,
        &[
            json!({"op": "set_cells", "table": table, "cell": "B2", "values": [["15"], ["25"]]}),
            json!({"op": "insert_rows", "table": table, "after_row": 3, "values": [["合計", "40"]]}),
            json!({"op": "style_table", "table": table, "total_row": true}),
            json!({"op": "table_props", "table": table, "columns_mm": [60, 30], "align": "center"}),
        ],
    )
    .await;
    assert!(
        text.contains("表 4行×2列「地域|売上|東|15|西|25|合計|40」"),
        "{text}"
    );
    let overview = run::read_overview(&mut client, &name, 0).await.unwrap();
    let table = id_of(&overview, "表 4行");
    let detail = run::read_blocks(&mut client, &table, None).await.unwrap();
    assert!(
        detail.contains("<col width=\"60mm\"><col width=\"30mm\">"),
        "{detail}"
    );
    assert!(detail.contains("align=\"center\""), "{detail}");
    assert!(
        detail.contains("background-color: #1F4E79"),
        "見出しの塗り: {detail}"
    );
    assert!(detail.contains("double"), "合計行の二重線: {detail}");
    assert!(
        detail.contains("text-align: right"),
        "数値は右揃え: {detail}"
    );
    // 箇条書きは <ul> の入れ子として読み戻せる。
    let first = id_of(&overview, "「一」");
    let detail = run::read_blocks(&mut client, &first, Some(&id_of(&overview, "「二」")))
        .await
        .unwrap();
    assert!(
        detail.contains("<ul><li data-style=\"Standard\">一</li></ul>"),
        "{detail}"
    );
    assert!(
        detail.contains("<ul><ul><li data-style=\"Standard\">二</li></ul></ul>"),
        "{detail}"
    );
    client.close().await;
}

#[tokio::test]
async fn style_only_edits_are_saved() {
    let Some(env) = env() else {
        eprintln!("skip: OFFICE_ENGINE_TEST_* 未設定");
        return;
    };
    let (name, mut client) = fresh(&env).await;
    // スタイルだけの変更は文書の「変更あり」にならず、保存で捨てられていた。
    apply_ok(
        &mut client,
        &[json!({"op": "set_style", "name": "Heading 1", "font": {"size": 20, "color": "#C00000"}})],
    )
    .await;
    client.save().await.unwrap();
    client.close().await;
    let mut client = open(&env, &name).await;
    let overview = run::read_overview(&mut client, &name, 0).await.unwrap();
    assert!(overview.contains("Heading 1（見出し1・20pt"), "{overview}");
    assert!(overview.contains("#C00000"), "{overview}");
    client.close().await;
}

/// 文書全体の作り直し（target: "all"）と、ページの区切り・最後のページへのはみ出しの警告。
#[tokio::test]
async fn whole_document_replace_and_page_overflow_warning() {
    let Some(env) = env() else {
        eprintln!("skip: OFFICE_ENGINE_TEST_* 未設定");
        return;
    };
    let (_name, mut client) = fresh(&env).await;
    let page = |n: usize| -> String {
        let body = (0..n).fold(String::new(), |mut acc, i| {
            acc.push_str("<p>本文の段落 ");
            acc.push_str(&i.to_string());
            acc.push_str("</p>");
            acc
        });
        format!("<h1 data-style=\"Title\">題</h1>{body}")
    };
    // まず多めに入れて、1 ページ目に入る段落の数を一覧のページ区切りから数える。
    let text = apply_ok(
        &mut client,
        &[json!({"op": "replace", "target": "all", "html": page(80)})],
    )
    .await;
    let first_page = text
        .split("── 2 ページ目 ──")
        .next()
        .unwrap()
        .matches("本文の段落")
        .count();
    assert!(first_page > 5, "{text}");
    // 1 ページに入る数より 1 つ多くすると、最後の 1 段落だけが 2 ページ目へ出る。
    let text = apply_ok(
        &mut client,
        &[json!({"op": "replace", "target": "all", "html": page(first_page + 1)})],
    )
    .await;
    assert!(text.contains("── 2 ページ目 ──"), "{text}");
    assert!(text.contains("はみ出しています"), "{text}");
    // もう一度 all で置き換えると、前の内容は残らない。
    let text = apply_ok(
        &mut client,
        &[json!({"op": "replace", "target": "all", "html": "<p>短い文書</p>"})],
    )
    .await;
    assert!(
        !text.contains("本文の段落") && !text.contains("ページ目"),
        "{text}"
    );
    client.close().await;
}

/// 目次の直前に同じ題の見出しがあると、その見出しを消して題を二重にしない。
#[tokio::test]
async fn toc_does_not_duplicate_a_heading_with_the_same_title() {
    let Some(env) = env() else {
        eprintln!("skip: OFFICE_ENGINE_TEST_* 未設定");
        return;
    };
    let (name, mut client) = fresh(&env).await;
    apply_ok(
        &mut client,
        &[json!({"op": "replace", "target": "all", "html": "<h1 data-style=\"Title\">報告書</h1><h2 data-style=\"Heading 1\">目次</h2><h2 data-style=\"Heading 1\">1. 概要</h2><p>本文</p>"})],
    )
    .await;
    let overview = run::read_overview(&mut client, &name, 0).await.unwrap();
    let toc_heading = id_of(&overview, "Heading 1「目次」");
    let text = apply_ok(
        &mut client,
        &[json!({"op": "insert_toc", "after": toc_heading, "title": "目次"})],
    )
    .await;
    assert!(text.contains("同じ題の段落「目次」を消しました"), "{text}");
    assert!(!text.contains("Heading 1「目次」"), "{text}");
    client.close().await;
}

/// 1 段落をスタイル無しの 1 段落で置き換えると元のスタイルを引き継ぎ、表の作り直しは結果で知らせる。
#[tokio::test]
async fn single_paragraph_replace_keeps_the_style_and_table_rebuilds_are_reported() {
    let Some(env) = env() else {
        eprintln!("skip: OFFICE_ENGINE_TEST_* 未設定");
        return;
    };
    let (name, mut client) = fresh(&env).await;
    apply_ok(
        &mut client,
        &[json!({"op": "replace", "target": "all", "html": "<h1 data-style=\"Title\">月次営業報告（9月）</h1><table><tr><th>地域</th><th>売上</th></tr><tr><td>首都圏</td><td>612</td></tr></table><p>以上</p>"})],
    )
    .await;
    let overview = run::read_overview(&mut client, &name, 0).await.unwrap();
    let title = id_of(&overview, "Title「月次営業報告（9月）」");
    let table = id_of(&overview, "表 ");
    let text = apply_ok(
        &mut client,
        &[
            json!({"op": "replace", "target": title, "html": "<h1>月次営業報告（10月）</h1>"}),
            json!({"op": "replace", "target": table, "html": "<table><tr><th>地域</th><th>売上</th></tr><tr><td>首都圏</td><td>628</td></tr></table>"}),
        ],
    )
    .await;
    assert!(text.contains("Title「月次営業報告（10月）」"), "{text}");
    assert!(
        text.contains("元の段落スタイル Title を引き継ぎました"),
        "{text}"
    );
    assert!(text.contains("表を作り直しました"), "{text}");
    client.close().await;
}

/// 同じ一括で置き換えたブロックの ID は、置き換え後の中身を指す（表を作り直して、その表を整え、
/// その後ろに注記を足す・Gemma 4 の評価で繰り返し書かれた形）。
#[tokio::test]
async fn a_replaced_id_points_at_its_replacement_in_the_same_batch() {
    let Some(env) = env() else {
        eprintln!("skip: OFFICE_ENGINE_TEST_* 未設定");
        return;
    };
    let (name, mut client) = fresh(&env).await;
    apply_ok(
        &mut client,
        &[json!({"op": "replace", "target": "all", "html": "<h2>2. 地域別売上</h2><table><tr><th>地域</th><th>売上</th></tr><tr><td>首都圏</td><td>612</td></tr></table><h2>3. トピック</h2>"})],
    )
    .await;
    let overview = run::read_overview(&mut client, &name, 0).await.unwrap();
    let table = id_of(&overview, "表 ");
    let text = apply_ok(
        &mut client,
        &[
            json!({"op": "replace", "target": table, "html": "<table><tr><th>地域</th><th>売上</th></tr><tr><td>首都圏</td><td>628</td></tr><tr><td>関西</td><td>401</td></tr></table>"}),
            json!({"op": "style_table", "table": table, "theme": "blue"}),
            json!({"op": "insert", "after": table, "html": "<p>※単位：百万円</p>"}),
        ],
    )
    .await;
    // 注記は新しい表のすぐ後ろ、次の見出しの前に入る。
    let after_table = text.split("表 3行×2列").nth(1).unwrap_or("");
    let note = after_table.find("※単位：百万円").unwrap_or(usize::MAX);
    let next = after_table.find("3. トピック").unwrap_or(0);
    assert!(note < next, "注記が表の後ろに無い: {text}");
    let blocks = run::read_blocks(&mut client, &id_of(&text, "表 3行×2列"), None)
        .await
        .unwrap();
    assert!(blocks.contains("background"), "表が整っていない: {blocks}");
    client.close().await;
}

/// 同じ一括で入れた表は table: "last" で指せ、set_cells の範囲は左上から書く。
#[tokio::test]
async fn the_last_inserted_table_can_be_styled_in_the_same_batch() {
    let Some(env) = env() else {
        eprintln!("skip: OFFICE_ENGINE_TEST_* 未設定");
        return;
    };
    let (_name, mut client) = fresh(&env).await;
    let text = apply_ok(
        &mut client,
        &[
            json!({"op": "insert", "at": "end", "html": "<table><tr><th>工程</th><th>期間</th></tr><tr><td>要件定義</td><td>-</td></tr></table>"}),
            json!({"op": "style_table", "table": "last", "theme": "green", "summary": "表を整える"}),
            json!({"op": "set_cells", "table": "last", "cell": "B2:B2", "values": [["11月"]]}),
        ],
    )
    .await;
    assert!(
        text.contains("表 2行×2列「工程|期間|要件定義|11月」"),
        "{text}"
    );
    client.close().await;
}

/// 位置を書かない目次は、表紙（題・日付）の後ろ、最初の見出しの前に入る。
#[tokio::test]
async fn an_unplaced_toc_goes_after_the_cover() {
    let Some(env) = env() else {
        eprintln!("skip: OFFICE_ENGINE_TEST_* 未設定");
        return;
    };
    let (_name, mut client) = fresh(&env).await;
    let text = apply_ok(
        &mut client,
        &[
            json!({"op": "replace", "target": "all", "html": "<p data-style=\"Title\">報告書</p><p>2026年10月20日</p><h1>1. 概要</h1><p>本文</p>"}),
            json!({"op": "insert_toc", "title": "目次"}),
        ],
    )
    .await;
    let title = text.find("Title「報告書」").unwrap_or(usize::MAX);
    let toc = text.find("目次（").unwrap_or(0);
    let heading = text.find("「1. 概要」").unwrap_or(0);
    assert!(
        title < toc && toc < heading,
        "目次が表紙の後ろ・見出しの前に無い: {text}"
    );
    client.close().await;
}

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

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
    assert!(overview.contains("フッター「- 1 -」"), "{overview}");
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

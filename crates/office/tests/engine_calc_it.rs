//! Excel エンジンの実機結合テスト（#725）。
//!
//! 実機の Collabora（26.04 以降・executescript あり）と偽 WOPI ホストに対して、
//! 正規化 → 適用 → 読み戻し → 失敗時の全取り消し → 保存を確かめる。次の環境変数があるときだけ走る:
//! - `OFFICE_ENGINE_TEST_WS`（例 `ws://localhost:9982`）
//! - `OFFICE_ENGINE_TEST_WOPI`（例 `http://127.0.0.1:9990/wopi/files`）
//! - `OFFICE_ENGINE_TEST_FILES`（偽 WOPI がファイルを配る/受けるディレクトリ）

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use office::engine::a1;
use office::engine::calc::normalize::normalize;
use office::engine::calc::ops::SheetOp;
use office::engine::calc::run;
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

/// 白紙の xlsx を新しい名前で置き、そこへ接続する。
async fn open_blank(env: &Env, name: &str) -> CoolWsClient {
    let blank = office::blank_template(office::OfficeKind::Spreadsheet);
    std::fs::write(format!("{}/{name}", env.files), blank).unwrap();
    let wopi_src = format!("{}/{name}", env.wopi);
    let mut cfg = CoolWsConfig::new(env.ws.clone());
    cfg.total_deadline = std::time::Duration::from_mins(5);
    CoolWsClient::connect(&cfg, &wopi_src, "test-token", "ja")
        .await
        .unwrap()
}

fn ops(v: serde_json::Value) -> Vec<SheetOp> {
    serde_json::from_value(v).unwrap()
}

#[tokio::test]
async fn builds_a_formatted_report_and_reads_it_back() {
    let Some(env) = env() else {
        eprintln!("skip: OFFICE_ENGINE_TEST_* 未設定");
        return;
    };
    let name = format!("engine-{}.xlsx", uuid::Uuid::new_v4());
    let mut client = open_blank(&env, &name).await;
    let batch = ops(json!([
        {"op": "rename_sheet", "sheet": "Sheet1", "name": "売上"},
        {"op": "set_values", "range": "売上!A1", "values": [
            ["支店", "4月", "5月", "6月", "合計"],
            ["東京", 1200, 1350, 1500, null],
            ["大阪", 800, 950, 700, null],
            ["名古屋", 500, 450, 620, null],
            ["合計", null, null, null, null]
        ]},
        {"op": "fill_formula", "range": "売上!E2:E4", "formula": "=SUM(B2:D2)"},
        {"op": "fill_formula", "range": "売上!B5:E5", "formula": "=SUM(B2:B4)"},
        {"op": "format", "range": "売上!A1:E1", "font": {"bold": true, "color": "#FFFFFF"}, "fill": "#1F3864", "align": {"horizontal": "center"}},
        {"op": "format", "range": "売上!B2:E5", "number_format": "#,##0"},
        {"op": "format", "range": "売上!A1:E5", "border": {"all": "thin"}},
        {"op": "conditional_format", "range": "売上!B2:D4", "when": {"operator": "<", "value": 600}, "style": {"font_color": "#C00000", "fill": "#FFC7CE"}},
        {"op": "column_width", "columns": "売上!A", "width": 10},
        {"op": "freeze", "cell": "売上!B2"},
        {"op": "add_chart", "name": "支店別", "type": "column", "data": "売上!A1:D4", "anchor": "売上!G2", "title": "支店別 月次売上"}
    ]));
    let normalized = normalize(&batch, None).unwrap();
    let out = run::apply(&mut client, "売上表", &normalized)
        .await
        .unwrap();
    eprintln!("{}", out.text);
    assert!(out.applied, "{}", out.text);
    assert!(
        out.text.contains("11 件の操作を適用しました"),
        "{}",
        out.text
    );
    assert!(
        out.text.contains("4,050"),
        "合計の計算結果が読み戻せる: {}",
        out.text
    );

    let area = a1::parse_area("売上!A1:E5").unwrap();
    let read = run::read_range(&mut client, &area, None, 500, true)
        .await
        .unwrap();
    eprintln!("{read}");
    assert!(
        read.contains("E2:E4 =SUM(B2:D2)（3 行、下へ同じ形）"),
        "{read}"
    );
    assert!(read.contains("塗り #1F3864: A1:E1"), "{read}");
    assert!(
        read.contains("罫線 細線 #000000（各セルの上下左右）: A1:E5"),
        "{read}"
    );
    assert!(read.contains("表示形式 #,##0: B2:E5"), "{read}");
    assert!(read.contains("条件付き書式: B2:D4"), "{read}");
    assert!(read.contains("グラフ「支店別」縦棒"), "{read}");

    let overview = run::read_overview(&mut client, &name).await.unwrap();
    eprintln!("{overview}");
    assert!(overview.contains("- 売上: 使用範囲 A1:E5"), "{overview}");
    client.close().await;
}

#[tokio::test]
async fn a_failing_batch_leaves_the_document_untouched() {
    let Some(env) = env() else {
        eprintln!("skip: OFFICE_ENGINE_TEST_* 未設定");
        return;
    };
    let name = format!("engine-{}.xlsx", uuid::Uuid::new_v4());
    let mut client = open_blank(&env, &name).await;
    // 3 件目で存在しないシートを指す → 1・2 件目も取り消される。
    let batch = ops(json!([
        {"op": "set_values", "range": "A1", "values": [["残ってはいけない"]]},
        {"op": "add_sheet", "name": "途中"},
        {"op": "format", "range": "無いシート!A1", "fill": "#FF0000"}
    ]));
    let normalized = normalize(&batch, None).unwrap();
    let out = run::apply(&mut client, "失敗する一括", &normalized)
        .await
        .unwrap();
    eprintln!("{}", out.text);
    assert!(!out.applied);
    assert!(out.text.contains("3 件目の操作"), "{}", out.text);
    let overview = run::read_overview(&mut client, &name).await.unwrap();
    assert!(
        !overview.contains("途中"),
        "シート追加が残っている: {overview}"
    );
    assert!(
        !overview.contains("残ってはいけない"),
        "値が残っている: {overview}"
    );
    client.close().await;
}

/// xlsx へ保存して開き直しても、文字色と罫線の線種が保たれる（テーマ色・幅の丸めの回帰防止）。
#[tokio::test]
async fn font_colors_and_border_styles_survive_an_xlsx_round_trip() {
    let Some(env) = env() else {
        eprintln!("skip: OFFICE_ENGINE_TEST_* 未設定");
        return;
    };
    let name = format!("engine-{}.xlsx", uuid::Uuid::new_v4());
    let mut client = open_blank(&env, &name).await;
    let parsed = office::engine::calc::parse::parse_ops(&[
        json!({"op": "set_values", "range": "A1", "values": [["見出し", "金額"], ["東京", 1200], ["合計", "=SUM(B2:B2)"]]}),
        json!({"op": "style_table", "range": "A1:B3", "theme": "navy", "total_row": true}),
        json!({"op": "format", "range": "B2", "font": {"color": "#C00000"}, "border": {"right": "medium"}}),
    ])
    .unwrap();
    let items: Vec<_> = parsed.iter().map(|(o, s)| (o, s.as_deref())).collect();
    let normalized = office::engine::calc::normalize::normalize_with_sheets(&items, None).unwrap();
    let out = run::apply(&mut client, "往復", &normalized).await.unwrap();
    assert!(out.applied, "{}", out.text);
    client.save().await.unwrap();
    client.close().await;

    // 別のセッションで開き直す（xlsx から読み込んだ状態を読む）。
    let wopi_src = format!("{}/{name}", env.wopi);
    let cfg = CoolWsConfig::new(env.ws.clone());
    let mut client = office::live::connect_with_retry(&cfg, &wopi_src, "test-token")
        .await
        .unwrap();
    let area = a1::parse_area("A1:B3").unwrap();
    let read = run::read_range(&mut client, &area, None, 100, false)
        .await
        .unwrap();
    eprintln!("{read}");
    assert!(
        read.contains("文字色 #FFFFFF: A1:B1"),
        "見出しの白字: {read}"
    );
    assert!(read.contains("文字色 #C00000: B2"), "赤字: {read}");
    assert!(read.contains("二重線"), "合計行の上の二重線: {read}");
    assert!(read.contains("中線"), "中線: {read}");
    client.close().await;
}

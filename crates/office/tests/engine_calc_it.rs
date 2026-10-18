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

/// シートの移動は 1 始まりの位置どおりになり、外のデータを読む数式は検証で弾かれる。
#[tokio::test]
async fn sheets_move_to_the_given_position_and_external_formulas_are_rejected() {
    let Some(env) = env() else {
        eprintln!("skip: OFFICE_ENGINE_TEST_* 未設定");
        return;
    };
    let name = format!("engine-{}.xlsx", uuid::Uuid::new_v4());
    let mut client = open_blank(&env, &name).await;
    let first = run::read_overview(&mut client, &name).await.unwrap();
    let sheet1 = if first.contains("Sheet1") {
        "Sheet1"
    } else {
        "シート1"
    };
    let batch = ops(json!([
        {"op": "add_sheet", "name": "B"},
        {"op": "add_sheet", "name": "C"},
        {"op": "move_sheet", "sheet": sheet1, "position": 3}
    ]));
    let out = run::apply(&mut client, "移動", &normalize(&batch, None).unwrap())
        .await
        .unwrap();
    assert!(out.applied, "{}", out.text);
    let overview = run::read_overview(&mut client, &name).await.unwrap();
    let pos = |n: &str| overview.find(&format!("\n- {n}:")).unwrap_or(usize::MAX);
    assert!(
        pos("B") < pos("C") && pos("C") < pos(sheet1),
        "B, C, {sheet1} の順になっていない: {overview}"
    );
    let bad = ops(json!([
        {"op": "set_values", "range": "A1", "values": [["=WEBSERVICE(\"http://127.0.0.1:9990/\")"]]}
    ]));
    let err = normalize(&bad, None).unwrap_err();
    assert!(err.to_string().contains("WEBSERVICE"), "{err}");
    client.close().await;
}

/// シートを足した後の、シートを書いていない操作は新しいシートへ書き、既存の値の上書きは結果に出る。
#[tokio::test]
async fn writes_after_add_sheet_go_to_the_new_sheet_and_overwrites_are_reported() {
    let Some(env) = env() else {
        eprintln!("skip: OFFICE_ENGINE_TEST_* 未設定");
        return;
    };
    let name = format!("engine-{}.xlsx", uuid::Uuid::new_v4());
    let mut client = open_blank(&env, &name).await;
    let seed = ops(json!([{"op": "set_values", "range": "A1", "values": [["元データ"], [1]]}]));
    let out = run::apply(&mut client, "元", &normalize(&seed, None).unwrap())
        .await
        .unwrap();
    assert!(out.applied, "{}", out.text);
    assert!(!out.text.contains("上書きしました"), "{}", out.text);
    let batch = ops(json!([
        {"op": "add_sheet", "name": "集計"},
        {"op": "set_values", "range": "A1", "values": [["集計表"]]}
    ]));
    let out = run::apply(&mut client, "集計", &normalize(&batch, None).unwrap())
        .await
        .unwrap();
    assert!(out.applied, "{}", out.text);
    assert!(
        !out.text.contains("上書きしました"),
        "元の表を上書きした: {}",
        out.text
    );
    let overview = run::read_overview(&mut client, &name).await.unwrap();
    assert!(overview.contains("- 集計: 使用範囲 A1"), "{overview}");
    // 既存の値を書き換えると、どこを何セル上書きしたかが返る。
    let over = ops(json!([{"op": "set_values", "range": "集計!A1", "values": [["別の値"]]}]));
    let out = run::apply(&mut client, "上書き", &normalize(&over, None).unwrap())
        .await
        .unwrap();
    assert!(
        out.text
            .contains("集計!A1:A1 の既存の値 1 セルを上書きしました"),
        "{}",
        out.text
    );
    client.close().await;
}

/// 別の呼び出しでシートを足した後も、シートを書かない操作は足したシートへ書く（作業中のシートになる）。
#[tokio::test]
async fn an_added_sheet_becomes_the_default_for_the_next_call() {
    let Some(env) = env() else {
        eprintln!("skip: OFFICE_ENGINE_TEST_* 未設定");
        return;
    };
    let name = format!("engine-{}.xlsx", uuid::Uuid::new_v4());
    let mut client = open_blank(&env, &name).await;
    let seed = ops(json!([{"op": "set_values", "range": "A1", "values": [["元データ"]]}]));
    run::apply(&mut client, "元", &normalize(&seed, None).unwrap())
        .await
        .unwrap();
    let add = ops(json!([{"op": "add_sheet", "name": "集計"}]));
    let out = run::apply(&mut client, "足す", &normalize(&add, None).unwrap())
        .await
        .unwrap();
    assert!(out.applied, "{}", out.text);
    client.save().await.unwrap();
    client.close().await;
    // 開き直した別の呼び出し（シートを書かない）。
    let mut client = office::live::connect_with_retry(
        &{
            let mut cfg = CoolWsConfig::new(env.ws.clone());
            cfg.total_deadline = std::time::Duration::from_mins(5);
            cfg
        },
        &format!("{}/{name}", env.wopi),
        "test-token",
    )
    .await
    .unwrap();
    let write = ops(json!([{"op": "set_values", "range": "A1", "values": [["集計表"]]}]));
    let out = run::apply(&mut client, "書く", &normalize(&write, None).unwrap())
        .await
        .unwrap();
    assert!(
        !out.text.contains("上書きしました"),
        "元の表を上書きした: {}",
        out.text
    );
    let overview = run::read_overview(&mut client, &name).await.unwrap();
    assert!(overview.contains("- 集計: 使用範囲 A1"), "{overview}");
    client.close().await;
}

/// 題つきのグラフも xlsx へ保存でき、開き直してもグラフが残る（題の文字の太さを整数で入れると
/// 26.04.5 の xlsx の書き出しが失敗していた）。
#[tokio::test]
async fn a_titled_chart_survives_saving_to_xlsx() {
    let Some(env) = env() else {
        eprintln!("skip: OFFICE_ENGINE_TEST_* 未設定");
        return;
    };
    let name = format!("engine-{}.xlsx", uuid::Uuid::new_v4());
    let mut client = open_blank(&env, &name).await;
    let batch = ops(json!([
        {"op": "set_values", "range": "A1", "values": [["項目", "4月", "5月"], ["家具", 1, 2], ["文具", 3, 4]]},
        {"op": "add_chart", "type": "line", "data": "A1:C3", "anchor": "A6", "title": "カテゴリ別の推移"}
    ]));
    let out = run::apply(&mut client, "グラフ", &normalize(&batch, None).unwrap())
        .await
        .unwrap();
    assert!(out.applied, "{}", out.text);
    client.save().await.unwrap();
    client.close().await;
    let mut client = office::live::connect_with_retry(
        &{
            let mut cfg = CoolWsConfig::new(env.ws.clone());
            cfg.total_deadline = std::time::Duration::from_mins(5);
            cfg
        },
        &format!("{}/{name}", env.wopi),
        "test-token",
    )
    .await
    .unwrap();
    let overview = run::read_overview(&mut client, &name).await.unwrap();
    assert!(overview.contains("題「カテゴリ別の推移」"), "{overview}");
    client.close().await;
}

/// 1 枚のシートの印刷設定は、同じページスタイルを共有する他のシートを変えない。見出し 0 行の表も整う。
#[tokio::test]
async fn page_setup_does_not_leak_to_sheets_sharing_the_style() {
    let Some(env) = env() else {
        eprintln!("skip: OFFICE_ENGINE_TEST_* 未設定");
        return;
    };
    let name = format!("engine-{}.xlsx", uuid::Uuid::new_v4());
    let mut client = open_blank(&env, &name).await;
    let first = run::read_overview(&mut client, &name).await.unwrap();
    let sheet1 = if first.contains("Sheet1") {
        "Sheet1"
    } else {
        "シート1"
    };
    let batch = ops(json!([
        {"op": "add_sheet", "name": "別紙"},
        {"op": "set_values", "range": format!("{sheet1}!A1"), "values": [["項目", "値"], ["a", 1], ["b", 2]]},
        {"op": "style_table", "range": format!("{sheet1}!A1:B3"), "header_rows": 0},
        {"op": "page_setup", "sheet": sheet1, "orientation": "landscape"}
    ]));
    let out = run::apply(&mut client, "印刷", &normalize(&batch, None).unwrap())
        .await
        .unwrap();
    assert!(out.applied, "{}", out.text);
    let other = run::read_range(
        &mut client,
        &a1::parse_area("A1").unwrap(),
        Some("別紙"),
        10,
        true,
    )
    .await
    .unwrap();
    let mine = run::read_range(
        &mut client,
        &a1::parse_area("A1").unwrap(),
        Some(sheet1),
        10,
        true,
    )
    .await
    .unwrap();
    let landscape = |t: &str| t.contains("A4横") || t.contains("用紙横");
    assert!(landscape(&mine), "{mine}");
    assert!(!landscape(&other), "別紙まで横向きになった: {other}");
    client.close().await;
}

/// 「自動」の列幅は、折り返しの見出し（style_table）も 1 行に収まる幅にする。
#[tokio::test]
async fn auto_width_fits_wrapped_headers_on_one_line() {
    let Some(env) = env() else {
        eprintln!("skip: OFFICE_ENGINE_TEST_* 未設定");
        return;
    };
    let name = format!("engine-{}.xlsx", uuid::Uuid::new_v4());
    let mut client = open_blank(&env, &name).await;
    let batch = ops(json!([
        {"op": "set_values", "range": "A1", "values": [["部門", "予算（千円）"], ["営業", 1], ["開発", 2]]},
        {"op": "style_table", "range": "A1:B3"},
        {"op": "column_width", "columns": "A:B", "width": "auto"}
    ]));
    let out = run::apply(&mut client, "幅", &normalize(&batch, None).unwrap())
        .await
        .unwrap();
    assert!(out.applied, "{}", out.text);
    // 「予算（千円）」は全角 6 文字。1 行に収まるなら B の幅は 6 文字分を超える。
    let widths = out
        .text
        .split("列幅（文字数）:")
        .nth(1)
        .unwrap_or("")
        .to_string();
    let b = widths
        .split_whitespace()
        .find_map(|w| w.strip_prefix("B="))
        .and_then(|w| w.parse::<f64>().ok())
        .unwrap_or(0.0);
    eprintln!("列幅: {widths}");
    assert!(b >= 10.0, "B の幅が見出しを 1 行に収めていない: {widths}");
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

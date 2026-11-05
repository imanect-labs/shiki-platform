//! 隔離ランナーが Excel（.xlsx）を読むテスト（CSV を集計の読み取りだけ・D-17・PIT-39）。
//!
//! ランナーのバイナリを実際に起動する（無ければスキップ・CI は runner をビルドしてから流す）。ローカル実行:
//!   `cargo build --manifest-path crates/tabular/runner/Cargo.toml`
//!   `SHIKI_TABULAR_RUNNER=crates/tabular/runner/target/debug/shiki-tabular-runner \`
//!   `  cargo test -p shiki-tabular --test runner_xlsx_it`
//!
//! 読み方（左端のシート・見出し・型・数式の値）と、敵対的な Excel（zip 爆弾・中の数・離れたセル・共有の文字の
//! 繰り返し・値の数・外部の実体・外部のブックへの参照）を、読み込みの上限（`tabular::input`）で断ることを確かめる。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::pedantic
)]

use std::fmt::Write as _;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use flate2::write::DeflateEncoder;
use flate2::Compression;
use serde_json::json;
use tabular::input::{
    MAX_CELLS, MAX_COLUMNS, MAX_ENTRIES, MAX_ENTRY_UNPACKED_BYTES, MAX_FILE_BYTES, MAX_TEXT_BYTES,
    MAX_UNPACKED_BYTES,
};
use tabular::{InputFormat, RunnerConfig, RunnerOp, RunnerRequest, RunnerResponse};

const MAIN: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const PKG: &str = "http://schemas.openxmlformats.org/package/2006/relationships";

fn runner_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SHIKI_TABULAR_RUNNER") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    ["release", "debug"]
        .into_iter()
        .map(|profile| {
            manifest
                .join("runner/target")
                .join(profile)
                .join("shiki-tabular-runner")
        })
        .find(|p| p.exists())
}

/// zip の 1 つの中身。
struct Entry {
    name: String,
    data: Vec<u8>,
    deflate: bool,
    /// ヘッダに書く展開後の大きさ（None は本当の大きさ）。
    declared: Option<u32>,
    /// ヘッダに書く圧縮の方式（None は本当の方式）。
    method: Option<u16>,
}

fn entry(name: &str, data: impl Into<Vec<u8>>) -> Entry {
    Entry {
        name: name.into(),
        data: data.into(),
        deflate: true,
        declared: None,
        method: None,
    }
}

/// 最小の zip を書く（ローカルヘッダ・中央ディレクトリ・終わりの印）。
fn zip(entries: &[Entry]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for e in entries {
        let crc = crc32fast::hash(&e.data);
        let body = if e.deflate {
            let mut enc = DeflateEncoder::new(Vec::new(), Compression::fast());
            enc.write_all(&e.data).unwrap();
            enc.finish().unwrap()
        } else {
            e.data.clone()
        };
        let method: u16 = e.method.unwrap_or(if e.deflate { 8 } else { 0 });
        let size = e.declared.unwrap_or(e.data.len() as u32);
        let offset = out.len() as u32;
        let name = e.name.as_bytes();
        let mut head = Vec::new();
        head.extend(20u16.to_le_bytes()); // 展開に要る版
        head.extend(0x0800u16.to_le_bytes()); // 名前は UTF-8
        head.extend(method.to_le_bytes());
        head.extend(0u16.to_le_bytes()); // 時刻
        head.extend(0x5881u16.to_le_bytes()); // 日付（2024-04-01）
        head.extend(crc.to_le_bytes());
        head.extend((body.len() as u32).to_le_bytes());
        head.extend(size.to_le_bytes());
        head.extend((name.len() as u16).to_le_bytes());
        head.extend(0u16.to_le_bytes()); // 追加の項目
        out.extend(0x0403_4b50u32.to_le_bytes());
        out.extend(&head);
        out.extend(name);
        out.extend(&body);
        central.extend(0x0201_4b50u32.to_le_bytes());
        central.extend(20u16.to_le_bytes()); // 作った版
        central.extend(&head);
        central.extend(0u16.to_le_bytes()); // 注釈
        central.extend(0u16.to_le_bytes()); // ディスク
        central.extend(0u16.to_le_bytes()); // 内部の属性
        central.extend(0u32.to_le_bytes()); // 外部の属性
        central.extend(offset.to_le_bytes());
        central.extend(name);
    }
    let cd_offset = out.len() as u32;
    out.extend(&central);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend((central.len() as u32).to_le_bytes());
    out.extend(cd_offset.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out
}

/// シート（名前・状態〔"" か "hidden"〕・sheetData の中身）と共有の文字から、ブックの中身を作る。
fn book(sheets: &[(&str, &str, String)], shared: &[&str]) -> Vec<Entry> {
    let mut types = String::new();
    let mut list = String::new();
    let mut rels = String::new();
    let mut parts = Vec::new();
    for (i, (name, state, data)) in sheets.iter().enumerate() {
        let n = i + 1;
        let _ = write!(
            types,
            r#"<Override PartName="/xl/worksheets/sheet{n}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>"#
        );
        let state = if state.is_empty() {
            String::new()
        } else {
            format!(r#" state="{state}""#)
        };
        let _ = write!(
            list,
            r#"<sheet name="{name}" sheetId="{n}"{state} r:id="rId{n}"/>"#
        );
        let _ = write!(
            rels,
            r#"<Relationship Id="rId{n}" Type="{REL}/worksheet" Target="worksheets/sheet{n}.xml"/>"#
        );
        parts.push(entry(
            &format!("xl/worksheets/sheet{n}.xml"),
            format!(r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="{MAIN}"><sheetData>{data}</sheetData></worksheet>"#),
        ));
    }
    let k = sheets.len();
    let sst: String = shared
        .iter()
        .map(|s| format!("<si><t>{s}</t></si>"))
        .collect();
    let mut all = vec![
        entry(
            "[Content_Types].xml",
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>{types}</Types>"#
            ),
        ),
        entry(
            "_rels/.rels",
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="{PKG}"><Relationship Id="rId1" Type="{REL}/officeDocument" Target="xl/workbook.xml"/></Relationships>"#
            ),
        ),
        entry(
            "xl/workbook.xml",
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><workbook xmlns="{MAIN}" xmlns:r="{REL}"><sheets>{list}</sheets></workbook>"#
            ),
        ),
        entry(
            "xl/_rels/workbook.xml.rels",
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="{PKG}">{rels}<Relationship Id="rId{}" Type="{REL}/sharedStrings" Target="sharedStrings.xml"/><Relationship Id="rId{}" Type="{REL}/styles" Target="styles.xml"/></Relationships>"#,
                k + 1,
                k + 2
            ),
        ),
        // 0 = 標準、1 = 日付（14）、2 = 日時（22）、3 = 時刻（20）。
        entry(
            "xl/styles.xml",
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><styleSheet xmlns="{MAIN}"><cellXfs count="4"><xf numFmtId="0"/><xf numFmtId="14" applyNumberFormat="1"/><xf numFmtId="22" applyNumberFormat="1"/><xf numFmtId="20" applyNumberFormat="1"/></cellXfs></styleSheet>"#
            ),
        ),
        entry(
            "xl/sharedStrings.xml",
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><sst xmlns="{MAIN}" count="{0}" uniqueCount="{0}">{sst}</sst>"#,
                shared.len()
            ),
        ),
    ];
    all.extend(parts);
    all
}

/// 文字のセル（共有の文字の番号）。
fn s(cell: &str, i: usize) -> String {
    format!(r#"<c r="{cell}" t="s"><v>{i}</v></c>"#)
}

/// 書式つきの数のセル。
fn n(cell: &str, v: &str, style: u32) -> String {
    format!(r#"<c r="{cell}" s="{style}"><v>{v}</v></c>"#)
}

fn row(r: u32, cells: &[String]) -> String {
    format!(r#"<row r="{r}">{}</row>"#, cells.concat())
}

fn write_file(bytes: &[u8]) -> tempfile::NamedTempFile {
    let mut f = tempfile::Builder::new().suffix(".xlsx").tempfile().unwrap();
    f.write_all(bytes).unwrap();
    f.flush().unwrap();
    f
}

async fn run(file: &tempfile::NamedTempFile, op: RunnerOp) -> RunnerResponse {
    let path = runner_path().expect("runner binary");
    let config = RunnerConfig::new(path.to_string_lossy().to_string(), Duration::from_secs(60));
    let req = RunnerRequest {
        op,
        csv_path: file.path().to_string_lossy().into_owned(),
        format: InputFormat::Xlsx,
        memory_limit_mb: 512,
        max_rows: 1000,
    };
    tabular::runner::run_isolated(&config, &req).await.unwrap()
}

fn query(sql: &str) -> RunnerOp {
    RunnerOp::Query { sql: sql.into() }
}

/// 読めない Excel は、利用者が直す誤り（input_error）として、文言を返す。
fn assert_refused(resp: &RunnerResponse, needle: &str) {
    assert!(!resp.ok, "読めてしまった: {:?}", resp.columns);
    assert!(resp.input_error, "利用者が直す誤りにする: {:?}", resp.error);
    let message = resp.error.clone().unwrap_or_default();
    assert!(message.contains(needle), "{message}");
}

/// 共通の材料（左端のシート「明細」と「集計」・make_expenses_xlsx.py）: 左端のシートを表として読み、いちばん上の
/// 行を列の名前にする。列の型はセルの種類から決め、日付・真偽・文字の 007 を種類のまま返す。ふりがなは読まず、
/// 数式は保存された値を読み、空の行は飛ばす。グリッド（Rows）は文字の形だけ。
#[tokio::test]
async fn reads_the_leftmost_sheet_with_types() {
    let Some(_) = runner_path() else {
        eprintln!("runner 未ビルドのためスキップ");
        return;
    };
    let file = write_file(include_bytes!("fixtures/expenses.xlsx"));
    let resp = run(&file, query("SELECT * FROM data ORDER BY 日付")).await;
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(
        resp.columns,
        ["日付", "品目", "金額", "精算済み", "社員番号"]
    );
    assert_eq!(
        resp.column_types,
        ["DATE", "VARCHAR", "BIGINT", "BOOLEAN", "VARCHAR"]
    );
    assert_eq!(
        resp.values,
        [
            vec![
                json!("2024-04-01"),
                json!("交通費"),
                json!(1200),
                json!(true),
                json!("007")
            ],
            vec![
                json!("2024-04-02"),
                json!("会議費"),
                json!(3000),
                json!(false),
                json!("012")
            ],
            vec![
                json!("2024-04-03"),
                json!("交通費"),
                json!(800),
                json!(true),
                json!("007")
            ],
            vec![
                json!("2024-04-05"),
                json!("宿泊費"),
                json!(9800),
                json!(false),
                json!("031")
            ],
        ]
    );
    assert_eq!(
        resp.rows[0]
            .iter()
            .map(|c| c.as_deref())
            .collect::<Vec<_>>(),
        [
            Some("2024-04-01"),
            Some("交通費"),
            Some("1200"),
            Some("true"),
            Some("007")
        ]
    );
    assert_eq!(resp.total_rows, Some(4));

    let total = run(
        &file,
        query("SELECT 品目, SUM(金額) AS 合計 FROM data GROUP BY 品目 ORDER BY 合計 DESC"),
    )
    .await;
    assert!(total.ok, "{:?}", total.error);
    assert_eq!(
        total.values,
        [
            [json!("宿泊費"), json!(9800)],
            [json!("会議費"), json!(3000)],
            [json!("交通費"), json!(2000)]
        ]
    );

    let schema = run(&file, RunnerOp::Schema).await;
    assert!(schema.ok, "{:?}", schema.error);
    assert_eq!(schema.columns, resp.columns);
    assert_eq!(schema.column_types, resp.column_types);
    assert_eq!(schema.total_rows, Some(4));
    let grid = run(&file, RunnerOp::Rows { offset: 0 }).await;
    assert!(grid.ok, "{:?}", grid.error);
    assert_eq!(grid.rows[0][2].as_deref(), Some("1200"));
    assert_eq!(grid.rows[0][3].as_deref(), Some("TRUE"));
    assert!(grid.values.is_empty());

    // Excel を読んだ後も外への読み取りは塞がれている（PIT-39）。
    let escape = run(&file, query("SELECT * FROM read_csv_auto('/etc/passwd')")).await;
    assert!(!escape.ok && !escape.input_error, "{:?}", escape.error);
}

/// 隠したシートは飛ばし、左端の見えるシートを読む。見出しの名前は DuckDB の CSV の読み込みと同じ決め方（前後の
/// 空白を外す・空は column{位置}・大文字小文字を区別せずに重なれば _1）。数は Excel と同じ 15 桁に丸める。
/// 種類の混ざった列はテキスト。日時と時刻、値の無い数式（空）と誤りの値（#DIV/0!）も読む。
#[tokio::test]
async fn names_columns_and_decides_types_like_the_csv_reader() {
    let Some(_) = runner_path() else {
        return;
    };
    let shared = ["名前", " 金額 ", "", "名前", "NAME", "あ", "い"];
    let visible = [
        row(
            1,
            &[
                s("A1", 0),
                s("B1", 1),
                s("C1", 2),
                s("D1", 3),
                s("E1", 4),
                s("F1", 0),
                s("G1", 0),
                s("H1", 0),
            ],
        ),
        row(
            2,
            &[
                s("A2", 5),
                n("B2", "0.30000000000000004", 0),
                n("C2", "45383.5", 2),
                n("D2", "0.375", 3),
                r#"<c r="E2"><f>1/0</f><v>#DIV/0!</v></c>"#.replace("<c ", r#"<c t="e" "#),
                r#"<c r="F2"><f>A1&amp;"x"</f></c>"#.into(),
                n("G2", "1", 0),
                n("H2", "45383", 1),
            ],
        ),
        row(
            3,
            &[
                s("A3", 6),
                n("B3", "2", 0),
                n("C3", "45384", 1),
                n("D3", "0.5", 3),
                n("E3", "5", 0),
                n("F3", "7", 0),
                s("G3", 6),
                n("H3", "45384", 1),
            ],
        ),
    ]
    .concat();
    let parts = book(
        &[
            ("隠したシート", "hidden", row(1, &[s("A1", 6)])),
            ("表", "", visible),
        ],
        &shared,
    );
    let file = write_file(&zip(&parts));
    let resp = run(&file, query("SELECT * FROM data ORDER BY 1")).await;
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(
        resp.columns,
        ["名前", "金額", "column2", "名前_1", "NAME", "名前_2", "名前_3", "名前_4"]
    );
    assert_eq!(
        resp.column_types,
        [
            "VARCHAR",
            "DOUBLE",
            "TIMESTAMP",
            "TIME",
            "VARCHAR",
            "BIGINT",
            "VARCHAR",
            "DATE"
        ]
    );
    assert_eq!(
        resp.values[0],
        [
            json!("あ"),
            json!(0.3),
            json!("2024-04-01 12:00:00"),
            json!("09:00:00"),
            json!("#DIV/0!"),
            json!(null),
            json!("1"),
            json!("2024-04-01")
        ]
    );
    assert_eq!(
        resp.values[1],
        [
            json!("い"),
            json!(2),
            json!("2024-04-02 00:00:00"),
            json!("12:00:00"),
            json!("5"),
            json!(7),
            json!("い"),
            json!("2024-04-02")
        ]
    );
}

/// 外部のブックへの参照を持つ数式は、保存された値を読む（参照はたどらない）。XML の外部の実体（DOCTYPE）は
/// 読まない（ファイルの中身を値に入れない）。
#[tokio::test]
async fn never_follows_external_links_or_entities() {
    let Some(_) = runner_path() else {
        return;
    };
    let mut parts = book(
        &[(
            "表",
            "",
            [
                row(1, &[s("A1", 0)]),
                row(2, &[r#"<c r="A2"><f>[1]Sheet1!A1</f><v>42</v></c>"#.into()]),
            ]
            .concat(),
        )],
        &["値"],
    );
    parts.push(entry(
        "xl/externalLinks/externalLink1.xml",
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><externalLink xmlns="{MAIN}"><externalBook xmlns:r="{REL}" r:id="rId1"/></externalLink>"#
        ),
    ));
    parts.push(entry(
        "xl/externalLinks/_rels/externalLink1.xml.rels",
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="{PKG}"><Relationship Id="rId1" Type="{REL}/externalLinkPath" Target="file:///etc/passwd" TargetMode="External"/></Relationships>"#
        ),
    ));
    let file = write_file(&zip(&parts));
    let resp = run(&file, query("SELECT * FROM data")).await;
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(resp.values, [[json!(42)]]);

    // 外部の実体を値に差し込もうとするシート。読めないなら断り、読めても実体の中身は入らない。
    let mut parts = book(&[("表", "", String::new())], &["値"]);
    let sheet = parts.last_mut().unwrap();
    sheet.data = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE w [<!ENTITY e SYSTEM "file:///etc/passwd">]><worksheet xmlns="{MAIN}"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>見出し</t></is></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>&e;</t></is></c></row></sheetData></worksheet>"#
    )
    .into_bytes();
    let file = write_file(&zip(&parts));
    let resp = run(&file, query("SELECT * FROM data")).await;
    if resp.ok {
        let text = format!("{:?}", resp.values);
        assert!(!text.contains("root:"), "外部の実体を読んだ: {text}");
    } else {
        assert!(resp.input_error, "{:?}", resp.error);
    }
}

/// zip 爆弾: 展開すると 1 つの上限を超える中身は、ヘッダが小さな大きさを書いていても読まない。合わせた上限も、
/// 中の数の上限も同じ。
#[tokio::test]
async fn refuses_zip_bombs() {
    let Some(_) = runner_path() else {
        return;
    };
    let big = vec![b' '; (MAX_ENTRY_UNPACKED_BYTES + 1) as usize];
    for declared in [None, Some(1_000)] {
        let mut parts = book(&[("表", "", row(1, &[s("A1", 0)]))], &["値"]);
        parts.push(Entry {
            declared,
            ..entry("xl/media/padding.bin", big.clone())
        });
        let file = write_file(&zip(&parts));
        assert!(
            file.as_file().metadata().unwrap().len() < 1 << 20,
            "圧縮で小さい"
        );
        let resp = run(&file, query("SELECT * FROM data")).await;
        if declared.is_none() {
            assert_refused(&resp, &format!("{} MB", MAX_UNPACKED_BYTES >> 20));
        } else {
            // 大きさを偽ったヘッダ: 書かれた大きさで打ち切るか、超えて展開して断るか。どちらでも読めない。
            assert!(!resp.ok && resp.input_error, "{:?}", resp.error);
        }
    }

    // 1 つずつは上限の内でも、合わせると上限を超える。
    let chunk = vec![b' '; (MAX_ENTRY_UNPACKED_BYTES - 1) as usize];
    let count = (MAX_UNPACKED_BYTES / MAX_ENTRY_UNPACKED_BYTES) as usize + 1;
    let mut parts = book(&[("表", "", row(1, &[s("A1", 0)]))], &["値"]);
    for i in 0..count {
        parts.push(entry(&format!("xl/media/padding{i}.bin"), chunk.clone()));
    }
    let resp = run(&write_file(&zip(&parts)), query("SELECT * FROM data")).await;
    assert_refused(&resp, "大きすぎて読めません");

    // 中の数の上限。
    let mut parts = book(&[("表", "", row(1, &[s("A1", 0)]))], &["値"]);
    for i in 0..=MAX_ENTRIES {
        parts.push(Entry {
            deflate: false,
            ..entry(&format!("x/{i}"), Vec::new())
        });
    }
    let resp = run(&write_file(&zip(&parts)), query("SELECT * FROM data")).await;
    assert_refused(&resp, "大きすぎて読めません");
}

/// ファイルの大きさの上限（中身を読む前に断る）。
#[tokio::test]
async fn refuses_files_over_the_size_limit() {
    let Some(_) = runner_path() else {
        return;
    };
    let file = write_file(b"PK\x03\x04");
    file.as_file().set_len(MAX_FILE_BYTES + 1).unwrap();
    let resp = run(&file, RunnerOp::Schema).await;
    assert_refused(&resp, &format!("{} MB まで", MAX_FILE_BYTES >> 20));
}

/// 離れた 2 つのセル（A1 と XFD1048576）は、宣言の範囲で表を確保せず、列の幅の上限で断る。行だけが離れたセルは、
/// 間の空の行を作らずに読む。
#[tokio::test]
async fn refuses_far_apart_cells_without_allocating_the_range() {
    let Some(_) = runner_path() else {
        return;
    };
    let far = [row(1, &[s("A1", 0)]), row(1_048_576, &[s("XFD1048576", 0)])].concat();
    let parts = book(&[("表", "", far)], &["値"]);
    let resp = run(&write_file(&zip(&parts)), query("SELECT * FROM data")).await;
    assert_refused(&resp, &format!("{} 列まで", group(MAX_COLUMNS as usize)));

    let tall = [row(1, &[s("A1", 0)]), row(1_048_576, &[s("A1048576", 0)])].concat();
    let parts = book(&[("表", "", tall)], &["値"]);
    let resp = run(&write_file(&zip(&parts)), query("SELECT * FROM data")).await;
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(resp.values, [[json!("値")]]);
}

/// 同じ共有の文字（1 MiB）を何度も指すシートは、値の文字の合計の上限で断る（メモリを使い切らない）。値の数の上限も
/// 同じく断る。
#[tokio::test]
async fn refuses_too_much_text_or_too_many_cells() {
    let Some(_) = runner_path() else {
        return;
    };
    let long = "あ".repeat(1 << 18); // 768 KiB
    let repeat = MAX_TEXT_BYTES / long.len() + 2;
    let cells: String = (1..=repeat as u32)
        .map(|r| row(r, &[s(&format!("A{r}"), 0)]))
        .collect();
    let parts = book(&[("表", "", cells)], &[&long]);
    let resp = run(
        &write_file(&zip(&parts)),
        query("SELECT count(*) FROM data"),
    )
    .await;
    assert_refused(&resp, &format!("{} MB まで", MAX_TEXT_BYTES >> 20));

    let mut cells = String::with_capacity(MAX_CELLS * 24);
    for r in 1..=(MAX_CELLS as u32 + 1) {
        let _ = write!(cells, r#"<row r="{r}"><c r="A{r}"><v>1</v></c></row>"#);
    }
    let parts = book(&[("表", "", cells)], &["値"]);
    let resp = run(
        &write_file(&zip(&parts)),
        query("SELECT count(*) FROM data"),
    )
    .await;
    assert_refused(&resp, &format!("{} 個まで", group(MAX_CELLS)));
}

/// Excel でない zip・壊れた zip・deflate でない圧縮は、利用者が直す誤りとして断る。
#[tokio::test]
async fn refuses_files_that_are_not_workbooks() {
    let Some(_) = runner_path() else {
        return;
    };
    let docx = zip(&[entry("word/document.xml", "<w:document/>")]);
    let mut broken = zip(&book(&[("表", "", row(1, &[s("A1", 0)]))], &["値"]));
    broken.truncate(broken.len() / 2);
    // 中身の圧縮の方式を bzip2（12）と書く（このランナーは deflate だけを読む）。
    let bzip = zip(&book(&[("表", "", row(1, &[s("A1", 0)]))], &["値"])
        .into_iter()
        .map(|e| Entry {
            method: Some(12),
            ..e
        })
        .collect::<Vec<_>>());
    for bytes in [docx, broken, bzip] {
        let resp = run(&write_file(&bytes), RunnerOp::Schema).await;
        assert_refused(&resp, "Excel のファイル");
    }
    // 表のシートが空。
    let empty = zip(&book(&[("表", "", String::new())], &["値"]));
    let resp = run(&write_file(&empty), RunnerOp::Schema).await;
    assert_refused(&resp, "「表」に値がありません");
}

fn group(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

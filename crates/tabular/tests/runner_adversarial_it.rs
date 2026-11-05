//! 隔離 DuckDB ランナーの敵対的テスト（Task 11P.7・PIT-39 の CI 必須テスト）。
//!
//! ランナーバイナリを実際に起動し、外部パス参照・URL 参照・DML/DDL・クォータ超過の
//! 4 系統がすべて拒否され、**api プロセス（このテストプロセス）を巻き込まない**ことを検証する。
//!
//! ランナーは workspace 除外クレート `crates/tabular/runner` を別ビルドする（DuckDB を含む）。
//! バイナリが無ければスキップする（CI は runner をビルドしてから実行する）。ローカル実行:
//!   `cargo build --manifest-path crates/tabular/runner/Cargo.toml --release`
//!   `SHIKI_TABULAR_RUNNER=crates/tabular/runner/target/release/shiki-tabular-runner \`
//!   `  cargo test -p shiki-tabular --test runner_adversarial_it`

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic
)]

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;
use tabular::{validate_read_only, InputFormat, RunnerConfig, RunnerOp, RunnerRequest};

/// ランナーバイナリのパス。`SHIKI_TABULAR_RUNNER` 優先、無ければ除外クレートの既定 target。
fn runner_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SHIKI_TABULAR_RUNNER") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    // 除外クレートの既定ビルド先（release / debug の順で探す）。
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")); // crates/tabular
    for profile in ["release", "debug"] {
        let p = manifest
            .join("runner/target")
            .join(profile)
            .join("shiki-tabular-runner");
        if p.exists() {
            return Some(p);
        }
    }
    None
}

fn write_csv(contents: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
    f.write_all(contents.as_bytes()).unwrap();
    f.flush().unwrap();
    f
}

async fn run(csv: &tempfile::NamedTempFile, op: RunnerOp) -> tabular::RunnerResponse {
    let path = runner_path().expect("runner binary");
    let config = RunnerConfig::new(path.to_string_lossy().to_string(), Duration::from_secs(20));
    let req = RunnerRequest {
        op,
        csv_path: csv.path().to_string_lossy().into_owned(),
        format: InputFormat::Csv,
        memory_limit_mb: 256,
        max_rows: 1000,
        max_result_bytes: None,
    };
    tabular::runner::run_isolated(&config, &req).await.unwrap()
}

#[tokio::test]
async fn schema_and_rows_work_on_real_csv() {
    let Some(_) = runner_path() else {
        eprintln!("runner 未ビルドのためスキップ");
        return;
    };
    let csv = write_csv("id,name\n1,alice\n2,bob\n3,carol\n");
    let schema = run(&csv, RunnerOp::Schema).await;
    assert!(schema.ok, "schema: {:?}", schema.error);
    assert_eq!(schema.columns, vec!["id", "name"]);
    assert_eq!(schema.total_rows, Some(3));

    let rows = run(&csv, RunnerOp::Rows { offset: 1 }).await;
    assert!(rows.ok);
    assert_eq!(rows.rows.len(), 2, "offset=1 で残り 2 行");
    assert_eq!(rows.rows[0][1], Some("bob".to_string()));
}

#[tokio::test]
async fn valid_select_query_works() {
    let Some(_) = runner_path() else {
        return;
    };
    let csv = write_csv("id,score\n1,10\n2,30\n3,20\n");
    let resp = run(
        &csv,
        RunnerOp::Query {
            sql: "SELECT id FROM data WHERE CAST(score AS INT) >= 20 ORDER BY id".into(),
        },
    )
    .await;
    assert!(resp.ok, "query: {:?}", resp.error);
    assert_eq!(resp.rows.len(), 2);
    assert_eq!(resp.rows[0][0], Some("2".to_string()));
}

/// Query 経路は型推論で読み込むため、数値集計（sum/avg 等）がキャストなしで通る（Task 11P.10 UX）。
#[tokio::test]
async fn aggregation_query_infers_numeric_types() {
    let Some(_) = runner_path() else {
        return;
    };
    let csv = write_csv("region,amount\ntokyo,100\nosaka,50\ntokyo,30\n");
    let resp = run(
        &csv,
        RunnerOp::Query {
            sql:
                "SELECT region, sum(amount) AS total FROM data GROUP BY region ORDER BY total DESC"
                    .into(),
        },
    )
    .await;
    assert!(resp.ok, "集計クエリは型推論で成功する: {:?}", resp.error);
    assert_eq!(resp.rows[0][0], Some("tokyo".to_string()));
    assert_eq!(
        resp.rows[0][1],
        Some("130".to_string()),
        "sum(amount) が数値集計される"
    );
}

/// 集計の結果: 日付・時刻・小数（DECIMAL）・リストを DuckDB の文字の形で返す（以前は `Date32(19753)` のような
/// デバッグ表示だった）。値の種類を保った行（values）も返し、数・真偽・空は種類のまま、JavaScript で正確に扱えない
/// 大きな整数は文字にする。結果の列の型と、打ち切らなかったときの行の数も返す（GAP-40）。
#[tokio::test]
async fn query_returns_readable_text_and_typed_values() {
    let Some(_) = runner_path() else {
        return;
    };
    let csv = write_csv(
        "品目,金額,済,日付,時刻\n\
         交通費,1200,true,2024-01-31,2024-01-31 09:30:00\n\
         会議費,,false,2024-02-29,2024-02-29 18:00:05\n",
    );
    let resp = run(
        &csv,
        RunnerOp::Query {
            sql: "SELECT 品目, 金額, 済, 日付, 時刻, CAST(1.5 AS DECIMAL(10,2)) AS 単価, 金額 / 8 AS 割, \
                  0.25 * 2 AS 半, 9007199254740993 AS 大, [1, 2] AS l FROM data ORDER BY 日付"
                .into(),
        },
    )
    .await;
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(
        resp.columns,
        ["品目", "金額", "済", "日付", "時刻", "単価", "割", "半", "大", "l"]
    );
    assert_eq!(
        resp.column_types,
        [
            "VARCHAR",
            "BIGINT",
            "BOOLEAN",
            "DATE",
            "TIMESTAMP",
            "DECIMAL(10,2)",
            "DOUBLE",
            "DECIMAL(13,2)",
            "BIGINT",
            "INTEGER[]"
        ]
    );
    fn text(r: &[Option<String>]) -> Vec<Option<&str>> {
        r.iter().map(|c| c.as_deref()).collect()
    }
    assert_eq!(
        text(&resp.rows[0]),
        [
            Some("交通費"),
            Some("1200"),
            Some("true"),
            Some("2024-01-31"),
            Some("2024-01-31 09:30:00"),
            Some("1.50"),
            Some("150"),
            Some("0.50"),
            Some("9007199254740993"),
            Some("[1, 2]")
        ]
    );
    assert_eq!(
        text(&resp.rows[1])[..5],
        [
            Some("会議費"),
            None,
            Some("false"),
            Some("2024-02-29"),
            Some("2024-02-29 18:00:05")
        ]
    );
    assert_eq!(
        resp.values,
        [
            vec![
                json!("交通費"),
                json!(1200),
                json!(true),
                json!("2024-01-31"),
                json!("2024-01-31 09:30:00"),
                json!(1.5),
                json!(150),
                json!(0.5),
                json!("9007199254740993"),
                json!("[1, 2]")
            ],
            vec![
                json!("会議費"),
                json!(null),
                json!(false),
                json!("2024-02-29"),
                json!("2024-02-29 18:00:05"),
                json!(1.5),
                json!(null),
                json!(0.5),
                json!("9007199254740993"),
                json!("[1, 2]")
            ]
        ]
    );
    assert_eq!(resp.total_rows, Some(2));
    assert!(!resp.truncated);
}

/// 集計の SQL の末尾のセミコロンと `--` の注釈で失敗しない（包んだ SQL が壊れない）。同じ名前の列は DuckDB が
/// `a_1` と付け直し、値が混ざらない。打ち切ったときは結果の行の数を返さない。
#[tokio::test]
async fn query_tolerates_trailing_semicolons_comments_and_duplicate_names() {
    let Some(path) = runner_path() else {
        return;
    };
    let csv = write_csv("a,b\n1,x\n2,y\n");
    for sql in [
        "SELECT a FROM data ORDER BY a;",
        "SELECT a FROM data ORDER BY a ; ;  ",
        "SELECT a FROM data ORDER BY a -- 注釈",
    ] {
        let resp = run(&csv, RunnerOp::Query { sql: sql.into() }).await;
        assert!(resp.ok, "{sql}: {:?}", resp.error);
        assert_eq!(resp.values, [[json!(1)], [json!(2)]], "{sql}");
    }
    let resp = run(
        &csv,
        RunnerOp::Query {
            sql: "SELECT a, b AS a FROM data ORDER BY 1".into(),
        },
    )
    .await;
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(resp.columns, ["a", "a_1"]);
    assert_eq!(resp.values[0], [json!(1), json!("x")]);

    let config = RunnerConfig::new(path.to_string_lossy().to_string(), Duration::from_secs(20));
    let req = RunnerRequest {
        op: RunnerOp::Query {
            sql: "SELECT a FROM data".into(),
        },
        csv_path: csv.path().to_string_lossy().into_owned(),
        format: InputFormat::Csv,
        memory_limit_mb: 256,
        max_rows: 1,
        max_result_bytes: None,
    };
    let cut = tabular::runner::run_isolated(&config, &req).await.unwrap();
    assert!(cut.truncated);
    assert_eq!(
        (cut.rows.len(), cut.values.len(), cut.total_rows),
        (1, 1, None)
    );
}

/// 集計の結果の大きさの上限（GAP-92）: 上限を超える行の手前で打ち切り、truncated を立てる（total_rows は null）。
/// 大きさはワークフローの結果に入れる形（rows の 1 行と items の 1 行を JSON にした大きさと区切り）で数える。上限に
/// ちょうど収まる結果は打ち切らず、1 行目が収まらなければ行の無い結果になる。上限が無ければ（グリッドなど）全部返す。
#[tokio::test]
async fn query_results_stop_before_the_size_limit() {
    let Some(path) = runner_path() else {
        return;
    };
    // 引用符・改行・制御文字・日本語を含む値（JSON にすると大きさが変わる）と、数・空。
    let body: String = (0..50)
        .map(|i| format!("n{i:02},\"本文 \"\"{i}\"\"\n\\改行\t\u{1}\",{i},\n"))
        .collect();
    let csv = write_csv(&format!("名前,メモ,数,空\n{body}"));
    let config = RunnerConfig::new(path.to_string_lossy().to_string(), Duration::from_secs(20));
    let run_with = |max_result_bytes: Option<u64>| {
        let req = RunnerRequest {
            op: RunnerOp::Query {
                sql: "SELECT * FROM data ORDER BY 1".into(),
            },
            csv_path: csv.path().to_string_lossy().into_owned(),
            format: InputFormat::Csv,
            memory_limit_mb: 256,
            max_rows: 1000,
            max_result_bytes,
        };
        let config = config.clone();
        async move { tabular::runner::run_isolated(&config, &req).await.unwrap() }
    };
    let full = run_with(None).await;
    assert!(full.ok, "{:?}", full.error);
    assert_eq!((full.rows.len(), full.truncated), (50, false));
    // 1 行の大きさは、api がワークフローの結果に入れる rows と items の 1 行を JSON にした大きさと区切りの和。
    let sizes: Vec<u64> = full
        .rows
        .iter()
        .zip(&full.values)
        .map(|(texts, values)| {
            let item: serde_json::Map<String, serde_json::Value> = full
                .columns
                .iter()
                .cloned()
                .zip(values.iter().cloned())
                .collect();
            let json_len = serde_json::to_string(texts).unwrap().len()
                + serde_json::to_string(&item).unwrap().len()
                + 2;
            let counted = tabular::protocol::result_row_bytes(&full.columns, texts, values);
            assert_eq!(counted, json_len as u64, "{texts:?}");
            counted
        })
        .collect();
    let ten: u64 = sizes[..10].iter().sum();
    let cut = run_with(Some(ten + sizes[10] - 1)).await;
    assert!(cut.ok, "{:?}", cut.error);
    assert!(cut.truncated);
    assert_eq!(
        (cut.rows.len(), cut.values.len(), cut.total_rows),
        (10, 10, None)
    );
    assert_eq!(cut.rows[..], full.rows[..10]);
    let exact = run_with(Some(sizes.iter().sum())).await;
    assert_eq!(
        (exact.rows.len(), exact.truncated, exact.total_rows),
        (50, false, Some(50))
    );
    let none = run_with(Some(1)).await;
    assert!(
        none.ok && none.truncated && none.rows.is_empty(),
        "{none:?}"
    );
}

/// 列の案内（Schema）は、集計で読むときの列の型を返す。グリッドの行（Rows）は文字のまま（先頭の 0 を潰さない）。
#[tokio::test]
async fn schema_reports_the_types_the_query_reads() {
    let Some(_) = runner_path() else {
        return;
    };
    let csv = write_csv("品目,金額,日付,番号\n交通費,1200,2024-01-31,007\n");
    let schema = run(&csv, RunnerOp::Schema).await;
    assert!(schema.ok, "{:?}", schema.error);
    assert_eq!(
        schema.column_types,
        ["VARCHAR", "BIGINT", "DATE", "VARCHAR"]
    );
    let rows = run(&csv, RunnerOp::Rows { offset: 0 }).await;
    assert_eq!(rows.rows[0][1].as_deref(), Some("1200"));
    assert_eq!(rows.rows[0][3].as_deref(), Some("007"));
    assert!(rows.values.is_empty(), "グリッドは文字の形だけ");
}

/// グリッド（Rows/Schema）は all_varchar 固定＝編集/往復の忠実性を保つ（先頭ゼロ等を潰さない）。
#[tokio::test]
async fn grid_rows_stay_varchar_for_fidelity() {
    let Some(_) = runner_path() else {
        return;
    };
    let csv = write_csv("code\n007\n042\n");
    let rows = run(&csv, RunnerOp::Rows { offset: 0 }).await;
    assert!(rows.ok);
    assert_eq!(
        rows.rows[0][0],
        Some("007".to_string()),
        "グリッドは文字列忠実（型推論で 7 に潰れない）"
    );
}

/// PIT-39 ①: 任意パス参照（read_csv('/etc/passwd')）が拒否される。
#[tokio::test]
async fn rejects_arbitrary_path_read() {
    let Some(_) = runner_path() else {
        return;
    };
    let csv = write_csv("a\n1\n");
    // api 側検証で弾かれる（多層防御の第 1 層）。
    assert!(validate_read_only("SELECT * FROM read_csv('/etc/passwd')").is_err());
    // ランナーへ直接届いても enable_external_access=false で失敗する（第 2 層）。
    let resp = run(
        &csv,
        RunnerOp::Query {
            sql: "SELECT * FROM read_csv_auto('/etc/passwd')".into(),
        },
    )
    .await;
    assert!(!resp.ok, "外部パス参照は拒否されるべき");
}

/// PIT-39 ②: URL 参照（httpfs・SSRF）が拒否される。
#[tokio::test]
async fn rejects_url_read() {
    let Some(_) = runner_path() else {
        return;
    };
    let csv = write_csv("a\n1\n");
    let resp = run(
        &csv,
        RunnerOp::Query {
            sql: "SELECT * FROM read_csv_auto('https://example.com/x.csv')".into(),
        },
    )
    .await;
    assert!(!resp.ok, "URL 参照は拒否されるべき");
}

/// PIT-39 ③: DML/DDL・ATTACH・PRAGMA・LOAD/INSTALL がランナーで実行されない。
#[tokio::test]
async fn rejects_dml_ddl_control() {
    let Some(_) = runner_path() else {
        return;
    };
    let csv = write_csv("a\n1\n");
    for sql in [
        "INSERT INTO data VALUES ('x')",
        "DROP TABLE data",
        "ATTACH 'evil.db'",
        "INSTALL httpfs",
        "LOAD httpfs",
        "PRAGMA database_list",
        "SET enable_external_access=true",
        "COPY data TO '/tmp/leak.csv'",
    ] {
        let resp = run(
            &csv,
            RunnerOp::Query {
                sql: sql.to_string(),
            },
        )
        .await;
        assert!(!resp.ok, "拒否されるべき: {sql}");
    }
}

/// lock_configuration により、外部アクセスを SQL から再有効化できない。
#[tokio::test]
async fn cannot_reenable_external_access() {
    let Some(_) = runner_path() else {
        return;
    };
    let csv = write_csv("a\n1\n");
    let resp = run(
        &csv,
        RunnerOp::Query {
            sql: "SET enable_external_access=true; SELECT * FROM read_csv_auto('/etc/passwd')"
                .into(),
        },
    )
    .await;
    assert!(!resp.ok, "設定変更＋外部参照は拒否されるべき");
}

/// PIT-39 ④: クォータ（時間）超過が api を巻き込まず打ち切られる。
#[tokio::test]
async fn quota_timeout_kills_runner_not_caller() {
    let Some(path) = runner_path() else {
        return;
    };
    let csv = write_csv("a\n1\n");
    // 極端に短いタイムアウト＋重いクエリ（巨大クロス結合）でクォータ超過を誘発する。
    let config = RunnerConfig::new(
        path.to_string_lossy().to_string(),
        Duration::from_millis(300),
    );
    let req = RunnerRequest {
        op: RunnerOp::Query {
            sql: "SELECT count(*) FROM range(100000000) t1, range(1000) t2".into(),
        },
        csv_path: csv.path().to_string_lossy().into_owned(),
        format: InputFormat::Csv,
        memory_limit_mb: 256,
        max_rows: 1000,
        max_result_bytes: None,
    };
    let result = tabular::runner::run_isolated(&config, &req).await;
    assert!(
        matches!(result, Err(tabular::TabularError::QuotaExceeded(_))),
        "クォータ超過になるべき: {result:?}"
    );
    // 呼び出し側（このプロセス）は生きている＝プロセス境界で封じられた。
    assert_eq!(2 + 2, 4);
}

/// 区切りと改行の推定（BUG-144）。集計がタブで列を分ける CSV は、推定もタブと答える（編集が同じ区切りで読み書き
/// する）。CRLF の CSV は CRLF と答える。推定は利用者の SQL を流さない。
#[tokio::test]
async fn dialect_reports_the_delimiter_the_query_uses() {
    let Some(_) = runner_path() else {
        return;
    };
    for (text, delimiter, newline) in [
        (
            "品目\t金額\r\n交通費\t1200\r\n宿泊費\t9800\r\n",
            "\t",
            "\r\n",
        ),
        ("品目,金額\n交通費,1200\n", ",", "\n"),
        ("品目;金額\n交通費;1200\n宿泊費;9800\n", ";", "\n"),
        ("品目|金額\r\n交通費|1200\r\n宿泊費|9800\r\n", "|", "\r\n"),
        // 見出しだけの CSV（ドライブで作った直後など）。
        ("品目,金額\n", ",", "\n"),
    ] {
        let csv = write_csv(text);
        let resp = run(&csv, RunnerOp::Dialect).await;
        assert!(resp.ok, "{text:?}: {:?}", resp.error);
        let dialect = resp.dialect.expect("dialect");
        assert_eq!(
            (dialect.delimiter.as_str(), dialect.newline.as_str()),
            (delimiter, newline),
            "{text:?}"
        );
        let schema = run(&csv, RunnerOp::Schema).await;
        assert_eq!(schema.columns, vec!["品目", "金額"], "{text:?}");
    }
}

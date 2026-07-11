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

use tabular::{validate_read_only, RunnerConfig, RunnerOp, RunnerRequest};

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
        memory_limit_mb: 256,
        max_rows: 1000,
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
            sql: "SELECT region, sum(amount) AS total FROM data GROUP BY region ORDER BY total DESC"
                .into(),
        },
    )
    .await;
    assert!(resp.ok, "集計クエリは型推論で成功する: {:?}", resp.error);
    assert_eq!(resp.rows[0][0], Some("tokyo".to_string()));
    assert_eq!(resp.rows[0][1], Some("130".to_string()), "sum(amount) が数値集計される");
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
        memory_limit_mb: 256,
        max_rows: 1000,
    };
    let result = tabular::runner::run_isolated(&config, &req).await;
    assert!(
        matches!(result, Err(tabular::TabularError::QuotaExceeded(_))),
        "クォータ超過になるべき: {result:?}"
    );
    // 呼び出し側（このプロセス）は生きている＝プロセス境界で封じられた。
    assert_eq!(2 + 2, 4);
}

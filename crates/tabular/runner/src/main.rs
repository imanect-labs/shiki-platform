//! 隔離 DuckDB ランナー（Phase 11-pre Task 11P.7・PIT-39）。
//!
//! 非特権プロセスとして stdin から [`tabular::RunnerRequest`] を 1 件読み、ロックダウンした
//! DuckDB で実行し、[`tabular::RunnerResponse`] を stdout へ書く。**資格情報を持たない**
//! （認可は api 側で完了済み）。CSV の解釈はこのプロセス内でのみ行う。
//!
//! # ロックダウン手順（順序が重要）
//! 1. 信頼できるパス（api が渡した一時ファイル）から `read_csv_auto` で `data` テーブルへロード。
//!    これが**唯一のファイルシステム読取**（我々が実行）。
//! 2. `SET enable_external_access=false` — 以降 SQL からの任意パス/URL 到達を封じる。
//! 3. `SET lock_configuration=true` — 設定変更（外部アクセス再有効化等）を封じる。
//! 4. autoload/autoinstall 無効化。
//! 5. ユーザーの**検証済み**読み取り専用 SQL を `data` に対して実行。
//!
//! Excel（.xlsx・[`InputFormat::Xlsx`]）は 1 の代わりに [`xlsx`] が展開して左端のシートを `data` に入れる
//! （CSV を集計の読み取りだけ・D-17）。読めない Excel は利用者が直す誤り（`input_error`）として返す。
//!
//! 集計の結果は、行の数（`max_rows`）と大きさ（`max_result_bytes`）の上限の手前で打ち切る（GAP-92・[`query`]）。
//!
//! 入力を読む前に、プロセスのアドレス空間に上限（RLIMIT_AS・[`tabular::protocol::runner_address_space_bytes`]）を
//! 付ける。DuckDB の memory_limit は Rust 側（Excel の読み取り）の確保に効かないため。超えると確保に失敗して
//! 落ち、api はメモリの超過として返す（PIT-39）。

mod query;
mod xlsx;

use std::io::{Read, Write};

use duckdb::Connection;
use rustix::process::{getrlimit, setrlimit, Resource, Rlimit};
use tabular::protocol::runner_address_space_bytes;
use tabular::{CsvDialect, InputFormat, RunnerOp, RunnerRequest, RunnerResponse};

fn main() {
    let mut input = Vec::new();
    if std::io::stdin().read_to_end(&mut input).is_err() {
        emit(&RunnerResponse::failure("stdin 読取に失敗"));
        std::process::exit(1);
    }
    let request: RunnerRequest = match serde_json::from_slice(&input) {
        Ok(r) => r,
        Err(e) => {
            emit(&RunnerResponse::failure(format!(
                "request デコードに失敗: {e}"
            )));
            std::process::exit(1);
        }
    };
    if let Err(e) = limit_address_space(request.memory_limit_mb) {
        emit(&RunnerResponse::failure(format!(
            "メモリの上限の設定に失敗: {e}"
        )));
        std::process::exit(1);
    }
    let response = execute(&request).unwrap_or_else(RunnerResponse::failure);
    emit(&response);
    let _ = std::io::stdout().flush();
}

/// プロセスのアドレス空間に上限を付ける（下げるだけ。すでにもっと低い上限があればそれを使う）。
fn limit_address_space(memory_limit_mb: u32) -> std::io::Result<()> {
    let wanted = runner_address_space_bytes(memory_limit_mb);
    let limit = getrlimit(Resource::As)
        .maximum
        .map_or(wanted, |max| max.min(wanted));
    setrlimit(
        Resource::As,
        Rlimit {
            current: Some(limit),
            maximum: Some(limit),
        },
    )
    .map_err(std::io::Error::from)
}

fn emit(resp: &RunnerResponse) {
    if let Ok(bytes) = serde_json::to_vec(resp) {
        let _ = std::io::stdout().write_all(&bytes);
    }
}

fn execute(req: &RunnerRequest) -> Result<RunnerResponse, String> {
    let conn = Connection::open_in_memory().map_err(|e| e.to_string())?;
    if matches!(req.op, RunnerOp::Dialect) {
        // 推定だけ（利用者の SQL は流さない）。読むのは渡された信頼パスだけ。
        set_quotas(&conn, req)?;
        return dialect(&conn, &req.csv_path);
    }
    let query_types = match req.format {
        InputFormat::Csv => lockdown_load(&conn, req)?,
        InputFormat::Xlsx => {
            set_quotas(&conn, req)?;
            let typed = matches!(req.op, RunnerOp::Query { .. });
            let types = match xlsx::load(&conn, &req.csv_path, typed) {
                Ok(types) => types,
                Err(xlsx::LoadError::Input(e)) => return Ok(RunnerResponse::input_failure(e.0)),
                Err(xlsx::LoadError::Internal(e)) => return Err(e),
            };
            lockdown(&conn)?;
            matches!(req.op, RunnerOp::Schema).then_some(types)
        }
    };
    match &req.op {
        RunnerOp::Schema => schema(&conn, query_types),
        RunnerOp::Rows { offset } => rows(&conn, *offset, req.max_rows),
        RunnerOp::Query { sql } => query::query(&conn, sql, req.max_rows, req.max_result_bytes),
        RunnerOp::Dialect => Err("unreachable: dialect".into()),
    }
}

/// メモリ/スレッド/一時領域のクォータ。
fn set_quotas(conn: &Connection, req: &RunnerRequest) -> Result<(), String> {
    conn.execute_batch(&format!(
        "SET memory_limit='{}MB'; SET threads=1; SET max_temp_directory_size='0GB';",
        req.memory_limit_mb
    ))
    .map_err(|e| format!("クォータ設定に失敗: {e}"))
}

/// 区切りと改行を推定する（BUG-144）。読み込み（[`lockdown_load`] の `read_csv_auto`）と同じ推定器と引数
/// （`header=true`）なので、集計が列を分けたのと同じ区切りになる。
fn dialect(conn: &Connection, csv_path: &str) -> Result<RunnerResponse, String> {
    let mut stmt = conn
        .prepare("SELECT Delimiter, NewLineDelimiter FROM sniff_csv(?, header=true)")
        .map_err(|e| format!("区切りの推定の準備に失敗: {e}"))?;
    let (delimiter, newline): (String, String) = stmt
        .query_row([csv_path], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|e| format!("区切りの推定に失敗: {e}"))?;
    // 改行は `\r\n` などのエスケープした文字列で返る（実際の改行の文字のこともある）。
    let crlf = newline.contains('\r') || newline.contains("\\r");
    Ok(RunnerResponse {
        ok: true,
        dialect: Some(CsvDialect {
            delimiter: unescape_delimiter(&delimiter),
            newline: if crlf { "\r\n" } else { "\n" }.into(),
        }),
        ..RunnerResponse::default()
    })
}

/// 推定器が返す区切りのエスケープ（`\t`）を文字に戻す。
fn unescape_delimiter(delimiter: &str) -> String {
    match delimiter {
        "\\t" => "\t".into(),
        other => other.into(),
    }
}

/// CSV をロードしてから外部アクセスを封鎖する（順序が要）。
///
/// Schema のときは、集計（Query）で読むときの列の型も返す（グリッドは全列を文字で読むが、ワークフローの列の案内は
/// 集計の結果の値の種類を言う）。
fn lockdown_load(conn: &Connection, req: &RunnerRequest) -> Result<Option<Vec<String>>, String> {
    set_quotas(conn, req)?;

    // 唯一のファイル読取: 我々が渡した信頼パスから data テーブルへ。
    // - グリッド（Schema/Rows）: 全列 VARCHAR で取り込み、型推論の副作用を避ける
    //   ＝セル編集・往復（patch/write）を安定させる（値の正規化で勝手に書き換えない）。
    // - 分析（Query）: 型推論を有効にする。VARCHAR だと sum/avg 等の集計がそのまま効かず
    //   利用者に不親切なため、クエリ経路だけ数値/日付を推論する（結果は読み取り専用で忠実性不要）。
    let all_varchar = !matches!(req.op, RunnerOp::Query { .. });
    let load_sql = format!(
        "CREATE TABLE data AS SELECT * FROM read_csv_auto(?, all_varchar={all_varchar}, header=true)"
    );
    let mut stmt = conn
        .prepare(&load_sql)
        .map_err(|e| format!("CSV ロード準備に失敗: {e}"))?;
    stmt.execute([req.csv_path.as_str()])
        .map_err(|e| format!("CSV ロードに失敗: {e}"))?;
    drop(stmt);
    let query_types = if matches!(req.op, RunnerOp::Schema) {
        // 集計と同じ推定（型推論あり・同じ引数）。パスは api が置いた一時ファイル（引用符は重ねて打ち消す）。
        let sql = format!(
            "DESCRIBE SELECT * FROM read_csv_auto('{}', header=true)",
            req.csv_path.replace('\'', "''")
        );
        // 推定できなくてもグリッドは読める（型は文字のまま返す）。
        query::describe(conn, &sql).ok().map(|(_, types)| types)
    } else {
        None
    };

    lockdown(conn)?;
    Ok(query_types)
}

/// 以降、SQL からの外部到達を封じる（PIT-39 の核）。lock_configuration で enable_external_access を戻せないように
/// する。表を作った後に呼ぶ。
fn lockdown(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "SET autoinstall_known_extensions=false; \
         SET autoload_known_extensions=false; \
         SET enable_external_access=false; \
         SET lock_configuration=true;",
    )
    .map_err(|e| format!("ロックダウン設定に失敗: {e}"))
}

fn total_rows(conn: &Connection) -> Result<u64, String> {
    conn.query_row("SELECT count(*) FROM data", [], |r| r.get::<_, i64>(0))
        .map(|n| n as u64)
        .map_err(|e| e.to_string())
}

/// 列の名前と型と総行数。型は集計で読むときの型（分かれば・列の数が合うときだけ）。
fn schema(conn: &Connection, query_types: Option<Vec<String>>) -> Result<RunnerResponse, String> {
    let (columns, types) = query::describe(conn, "DESCRIBE SELECT * FROM data")?;
    let column_types = match query_types {
        Some(t) if t.len() == columns.len() => t,
        _ => types,
    };
    Ok(RunnerResponse {
        ok: true,
        columns,
        column_types,
        total_rows: Some(total_rows(conn)?),
        ..RunnerResponse::default()
    })
}

fn rows(conn: &Connection, offset: u64, max_rows: u32) -> Result<RunnerResponse, String> {
    let total = total_rows(conn)?;
    let sql = format!(
        "SELECT * FROM data LIMIT {} OFFSET {}",
        u64::from(max_rows).saturating_add(1),
        offset
    );
    let mut resp = query::read_rows(conn, &sql, max_rows, None)?;
    resp.total_rows = Some(total);
    Ok(resp)
}

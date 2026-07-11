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

use std::io::{Read, Write};

use duckdb::types::ValueRef;
use duckdb::{Connection, Row};
use tabular::{RunnerOp, RunnerRequest, RunnerResponse};

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
    let response = execute(&request).unwrap_or_else(RunnerResponse::failure);
    emit(&response);
    let _ = std::io::stdout().flush();
}

fn emit(resp: &RunnerResponse) {
    if let Ok(bytes) = serde_json::to_vec(resp) {
        let _ = std::io::stdout().write_all(&bytes);
    }
}

fn execute(req: &RunnerRequest) -> Result<RunnerResponse, String> {
    let conn = Connection::open_in_memory().map_err(|e| e.to_string())?;
    lockdown_load(&conn, req)?;
    match &req.op {
        RunnerOp::Schema => schema(&conn),
        RunnerOp::Rows { offset } => rows(&conn, *offset, req.max_rows),
        RunnerOp::Query { sql } => query(&conn, sql, req.max_rows),
    }
}

/// CSV をロードしてから外部アクセスを封鎖する（順序が要）。
fn lockdown_load(conn: &Connection, req: &RunnerRequest) -> Result<(), String> {
    // メモリ/スレッド/一時領域のクォータ。
    conn.execute_batch(&format!(
        "SET memory_limit='{}MB'; SET threads=1; SET max_temp_directory_size='0GB';",
        req.memory_limit_mb
    ))
    .map_err(|e| format!("クォータ設定に失敗: {e}"))?;

    // 唯一のファイル読取: 我々が渡した信頼パスから data テーブルへ（全列 VARCHAR で取り込み、
    // 型推論の副作用を避ける＝セル編集・往復を安定させる）。
    let mut stmt = conn
        .prepare(
            "CREATE TABLE data AS SELECT * FROM read_csv_auto(?, all_varchar=true, header=true)",
        )
        .map_err(|e| format!("CSV ロード準備に失敗: {e}"))?;
    stmt.execute([req.csv_path.as_str()])
        .map_err(|e| format!("CSV ロードに失敗: {e}"))?;
    drop(stmt);

    // 以降、SQL からの外部到達を封じる（PIT-39 の核）。lock_configuration で
    // enable_external_access を戻せないようにする。
    conn.execute_batch(
        "SET autoinstall_known_extensions=false; \
         SET autoload_known_extensions=false; \
         SET enable_external_access=false; \
         SET lock_configuration=true;",
    )
    .map_err(|e| format!("ロックダウン設定に失敗: {e}"))?;
    Ok(())
}

fn total_rows(conn: &Connection) -> Result<u64, String> {
    conn.query_row("SELECT count(*) FROM data", [], |r| r.get::<_, i64>(0))
        .map(|n| n as u64)
        .map_err(|e| e.to_string())
}

fn schema(conn: &Connection) -> Result<RunnerResponse, String> {
    let (columns, types) = describe(conn, "DESCRIBE SELECT * FROM data")?;
    Ok(RunnerResponse {
        ok: true,
        columns,
        column_types: types,
        rows: Vec::new(),
        total_rows: Some(total_rows(conn)?),
        truncated: false,
        error: None,
    })
}

fn describe(conn: &Connection, sql: &str) -> Result<(Vec<String>, Vec<String>), String> {
    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let mut names = Vec::new();
    let mut types = Vec::new();
    let mut r = stmt.query([]).map_err(|e| e.to_string())?;
    while let Some(row) = r.next().map_err(|e| e.to_string())? {
        // DESCRIBE: column_name, column_type, ...
        names.push(row.get::<_, String>(0).map_err(|e| e.to_string())?);
        types.push(row.get::<_, String>(1).unwrap_or_default());
    }
    Ok((names, types))
}

fn rows(conn: &Connection, offset: u64, max_rows: u32) -> Result<RunnerResponse, String> {
    let total = total_rows(conn)?;
    let sql = format!(
        "SELECT * FROM data LIMIT {} OFFSET {}",
        u64::from(max_rows).saturating_add(1),
        offset
    );
    let mut resp = select(conn, &sql, max_rows)?;
    resp.total_rows = Some(total);
    Ok(resp)
}

fn query(conn: &Connection, sql: &str, max_rows: u32) -> Result<RunnerResponse, String> {
    // ユーザー SQL は api 側で検証済み（単一 SELECT）。ラップして結果サイズを打ち切る。
    let wrapped = format!(
        "SELECT * FROM ({sql}) AS q LIMIT {}",
        u64::from(max_rows).saturating_add(1)
    );
    select(conn, &wrapped, max_rows)
}

/// SELECT を実行し、全セルを文字列化して返す（max_rows で打ち切り）。
fn select(conn: &Connection, sql: &str, max_rows: u32) -> Result<RunnerResponse, String> {
    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let mut r = stmt.query([]).map_err(|e| e.to_string())?;
    // 列名は**実行後**に取れる（query() 前だと DuckDB が None を返す）。Rows 経由で取得。
    let columns: Vec<String> = r
        .as_ref()
        .map(|s| s.column_names().iter().map(ToString::to_string).collect())
        .unwrap_or_default();
    let ncols = columns.len();
    let mut out_rows: Vec<Vec<Option<String>>> = Vec::new();
    let mut truncated = false;
    while let Some(row) = r.next().map_err(|e| e.to_string())? {
        if out_rows.len() as u32 >= max_rows {
            truncated = true;
            break;
        }
        let mut cells = Vec::with_capacity(ncols);
        for i in 0..ncols {
            cells.push(cell_to_string(row, i));
        }
        out_rows.push(cells);
    }
    Ok(RunnerResponse {
        ok: true,
        columns,
        column_types: Vec::new(),
        rows: out_rows,
        total_rows: None,
        truncated,
        error: None,
    })
}

/// セルを文字列へ（NULL は None）。all_varchar ロードなので基本 Text。
fn cell_to_string(row: &Row<'_>, i: usize) -> Option<String> {
    match row.get_ref(i) {
        Ok(ValueRef::Null) => None,
        Ok(ValueRef::Text(bytes)) => Some(String::from_utf8_lossy(bytes).into_owned()),
        Ok(ValueRef::Boolean(b)) => Some(b.to_string()),
        Ok(ValueRef::TinyInt(n)) => Some(n.to_string()),
        Ok(ValueRef::SmallInt(n)) => Some(n.to_string()),
        Ok(ValueRef::Int(n)) => Some(n.to_string()),
        Ok(ValueRef::BigInt(n)) => Some(n.to_string()),
        Ok(ValueRef::HugeInt(n)) => Some(n.to_string()),
        Ok(ValueRef::UTinyInt(n)) => Some(n.to_string()),
        Ok(ValueRef::USmallInt(n)) => Some(n.to_string()),
        Ok(ValueRef::UInt(n)) => Some(n.to_string()),
        Ok(ValueRef::UBigInt(n)) => Some(n.to_string()),
        Ok(ValueRef::Float(f)) => Some(f.to_string()),
        Ok(ValueRef::Double(f)) => Some(f.to_string()),
        Ok(other) => Some(format!("{other:?}")),
        Err(_) => None,
    }
}

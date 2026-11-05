//! Excel（.xlsx）を表として読み、DuckDB に入れる（CSV を集計の読み取りだけ・D-17）。
//!
//! アップロードされた Excel は敵対的な入力として扱う。上限（`tabular::input` の定数）は次の順に確かめる。
//!
//! 1. ファイルの大きさ・zip の中の数。中のファイルを 1 つずつ実際に展開して数え、1 つずつと合わせた大きさの上限を
//!    超えたら読まない（zip が書く大きさは信じない・zip 爆弾）。展開は deflate だけ（ほかの圧縮・暗号は読まない）。
//! 2. calamine でブックを開き、読むシートを 1 セルずつ読む（[`read`]）。値の数・値の文字の合計・表の大きさ（値のある
//!    行の数 × 列の幅）は読むシートを合わせて、列の幅はシートごとに数え、上限を超えたら止める。
//! 3. 値は疎のまま（行ごとに値のある列だけ）持ち、表に入れるときに 1 行ずつ幅の分に広げる（行の数 × 幅の表を
//!    まとめて確保しない）。ランナーのプロセスにはアドレス空間の上限も付いている（`main.rs`）。
//!
//! 読むシート（GAP-84）:
//! - `data` は表示している左端のシート（隠したシート・グラフのシートは飛ばす。見えるシートが無ければ左端のシート・
//!   D-148）。
//! - 集計（Query）は、SQL が表として読む名前（`FROM "集計"`）のシートも、その名前の表にする。`data` のシートも名前で
//!   読める。隠したシートも名前で読める。SQL が読まないシートは読まない（`data` も、SQL が読まなければ読まない）。
//! - DuckDB の名前は大文字と小文字を区別しないので、`data` と同じ名前のほかのシート、前のシートと大文字と小文字だけが
//!   違う名前のシート、名前の無いシートは名前で読めない（[`SheetInfo::usable`]）。
//! - グリッドと列の案内（Rows・Schema）は `data` だけを読む。どれもシートの一覧を返す。
//!
//! 見出しと型の決め方は [`shape`]（表の上の表題の行は飛ばす・GAP-83）。

mod read;
mod shape;

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufReader, Read};

use calamine::{Reader, SheetType, SheetVisible, Xlsx, XlsxError};
use duckdb::{appender_params_from_iter, Connection};
use tabular::input::{
    file_too_large, MAX_ENTRIES, MAX_ENTRY_UNPACKED_BYTES, MAX_FILE_BYTES, MAX_UNPACKED_BYTES,
};
use tabular::SheetInfo;

use shape::Sheet;

/// Excel を読めなかった理由（利用者向けの文言・ランナーは `input_error` で返す）。
#[derive(Debug)]
pub struct Unreadable(pub String);

/// 読めない・壊れた Excel。
const BROKEN: &str =
    "Excel のファイル（.xlsx）として読めませんでした。ファイルが壊れていないか確かめてください。";

/// パスワードを付けた Excel。
const PASSWORD: &str =
    "パスワードを付けた Excel のファイルは読めません。パスワードを外して保存し直してください。";

/// `data` の名前（小文字）。
const DATA: &str = "data";

/// 展開した大きさ・中の数が上限を超えた Excel。
fn unpacked_too_large() -> Unreadable {
    Unreadable(format!(
        "Excel のファイルの中身が大きすぎて読めません（展開すると {} MB を超えます）。使う行と列だけを CSV に保存して選んでください。",
        MAX_UNPACKED_BYTES >> 20
    ))
}

/// 読むシート。
pub enum Want<'a> {
    /// `data` だけ（グリッド・列の案内）。全列を文字で入れる。
    Data,
    /// SQL が表として読むシート（集計）。列に型を付ける。
    Query(&'a str),
}

/// 読んだ結果（返り値の材料）。
#[derive(Debug, Default)]
pub struct Loaded {
    /// `data` を集計で読むときの列の型（`data` を読まなかったときは空）。
    pub types: Vec<String>,
    /// ブックのシート（表のシートだけ・ブックの順）。
    pub sheets: Vec<SheetInfo>,
    /// `data` の見出しにした行（表題の行を飛ばしたときだけ）。
    pub header_row: Option<u32>,
}

/// 読み込みの失敗（利用者が直す誤りと、ランナーの異常）。
#[derive(Debug)]
pub enum LoadError {
    Input(Unreadable),
    Internal(String),
}

impl From<Unreadable> for LoadError {
    fn from(e: Unreadable) -> Self {
        LoadError::Input(e)
    }
}

/// Excel のシートを読み、表を作る（[`Want`]）。
pub fn load(conn: &Connection, path: &str, want: Want<'_>) -> Result<Loaded, LoadError> {
    check_archive(path)?;
    let file = File::open(path).map_err(|_| Unreadable(BROKEN.into()))?;
    let mut book: Xlsx<_> = Xlsx::new(BufReader::new(file)).map_err(|e| match e {
        XlsxError::Password => Unreadable(PASSWORD.into()),
        _ => Unreadable(BROKEN.into()),
    })?;
    let sheets = sheets_of(&book);
    let Some(data) = sheets.iter().position(|s| s.data) else {
        return Err(Unreadable("Excel のファイルに表のシートがありません。".into()).into());
    };
    // 集計は SQL が表として読む名前だけ（読めない SQL は data だけ。api が検証した SQL なので来ない）。
    let (typed, names) = match want {
        Want::Data => (false, None),
        Want::Query(sql) => (true, tabular::sql_guard::table_names(sql)),
    };
    let reads = |info: &SheetInfo| {
        names
            .as_ref()
            .is_none_or(|n| info.usable && n.contains(&info.name.to_lowercase()))
    };
    let raw = raw_table_name(&sheets);
    let mut budget = read::Budget::default();
    let mut loaded = Loaded::default();
    let data_info = &sheets[data];
    if names.as_ref().is_none_or(|n| n.contains(DATA)) || reads(data_info) {
        let Some(sheet) = read::read_sheet(&mut book, &data_info.name, &mut budget)? else {
            return Err(Unreadable(empty_data_sheet(&sheets, data)).into());
        };
        create_table(conn, &raw, DATA, &sheet, typed).map_err(LoadError::Internal)?;
        // data のシートの名前でも読めるようにする（名前が data のときは同じ表）。
        if data_info.usable && data_info.name.to_lowercase() != DATA && names.is_some() {
            conn.execute_batch(&format!(
                "CREATE VIEW {} AS SELECT * FROM {DATA};",
                quote(&data_info.name)
            ))
            .map_err(|e| LoadError::Internal(format!("Excel の表の作成に失敗: {e}")))?;
        }
        loaded.types = sheet.types.iter().map(|t| (*t).to_string()).collect();
        loaded.header_row = sheet.header_row;
    }
    if names.is_some() {
        for (i, info) in sheets.iter().enumerate() {
            if i == data || !reads(info) {
                continue;
            }
            // 値の無いシートは、行の無い表にする（名前を書いた SQL を誤りにしない）。
            let sheet =
                read::read_sheet(&mut book, &info.name, &mut budget)?.unwrap_or_else(|| Sheet {
                    columns: vec!["column0".into()],
                    types: vec!["VARCHAR"],
                    ..Sheet::default()
                });
            create_table(conn, &raw, &info.name, &sheet, typed).map_err(LoadError::Internal)?;
        }
    }
    loaded.sheets = sheets;
    Ok(loaded)
}

/// ブックの表のシート（ブックの順）。`data` は表示している左端のシート（見えるシートが無ければ左端のシート）。
/// 名前で読めるかは、`data` のシートから先に、大文字と小文字を区別せずに名前を取っていって決める。
fn sheets_of<RS: std::io::Read + std::io::Seek>(book: &Xlsx<RS>) -> Vec<SheetInfo> {
    let mut sheets: Vec<SheetInfo> = book
        .sheets_metadata()
        .iter()
        .filter(|s| s.typ == SheetType::WorkSheet)
        .map(|s| SheetInfo {
            name: s.name.clone(),
            hidden: s.visible != SheetVisible::Visible,
            data: false,
            usable: false,
        })
        .collect();
    let data = sheets.iter().position(|s| !s.hidden).unwrap_or(0);
    let Some(first) = sheets.get_mut(data) else {
        return sheets;
    };
    first.data = true;
    let mut used: HashSet<String> = HashSet::from([DATA.to_string()]);
    let order = std::iter::once(data).chain((0..sheets.len()).filter(|&i| i != data));
    for i in order {
        let sheet = &mut sheets[i];
        let key = sheet.name.to_lowercase();
        // data のシートは名前が data でも読める（同じ表）。
        sheet.usable = !sheet.name.trim().is_empty()
            && (used.insert(key.clone()) || (sheet.data && key == DATA));
    }
    sheets
}

/// 左端のシートに値が無いときの文言（ほかのシートを名前で読めることを言う）。
fn empty_data_sheet(sheets: &[SheetInfo], data: usize) -> String {
    let name = &sheets[data].name;
    let other = sheets
        .iter()
        .enumerate()
        .find(|(i, s)| *i != data && s.usable)
        .map(|(_, s)| s.name.as_str());
    match other {
        Some(other) => format!(
            "Excel の左端のシート「{name}」に値がありません。ほかのシートは、SQL に FROM {} のように名前を書いて読めます。",
            quote(other)
        ),
        None => format!(
            "Excel の左端のシート「{name}」に値がありません。表を左端のシートに置いてください。"
        ),
    }
}

/// zip の中の数と、実際に展開した大きさを確かめる（zip 爆弾）。
fn check_archive(path: &str) -> Result<(), Unreadable> {
    let file = File::open(path).map_err(|_| Unreadable(BROKEN.into()))?;
    let len = file.metadata().map(|m| m.len()).unwrap_or(u64::MAX);
    if len > MAX_FILE_BYTES {
        return Err(Unreadable(file_too_large()));
    }
    let mut archive =
        zip::ZipArchive::new(BufReader::new(file)).map_err(|_| Unreadable(BROKEN.into()))?;
    if archive.len() > MAX_ENTRIES {
        return Err(unpacked_too_large());
    }
    let mut total: u64 = 0;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|_| Unreadable(BROKEN.into()))?;
        if entry.is_dir() {
            continue;
        }
        // 1 つの上限か、合わせた上限の残りの小さいほうまで展開して数える（上限 + 1 バイト読めたら超えている）。
        let budget = MAX_ENTRY_UNPACKED_BYTES.min(MAX_UNPACKED_BYTES - total);
        let read = std::io::copy(&mut (&mut entry).take(budget + 1), &mut std::io::sink())
            .map_err(|_| Unreadable(BROKEN.into()))?;
        if read > budget {
            return Err(unpacked_too_large());
        }
        total += read;
    }
    Ok(())
}

/// 識別子の引用（`"` は重ねる）。
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// 表を組む途中の表の名前（どのシートの名前とも重ならない名前）。
fn raw_table_name(sheets: &[SheetInfo]) -> String {
    let taken: HashSet<String> = sheets.iter().map(|s| s.name.to_lowercase()).collect();
    let mut name = String::from("shiki_raw");
    while taken.contains(&name) || name == DATA {
        name.push('_');
    }
    name
}

/// シートを `table` に入れる。まず全列を文字で入れ、集計なら列の型に直す（文字の形はこちらで作ったので直せる）。
fn create_table(
    conn: &Connection,
    raw: &str,
    table: &str,
    sheet: &Sheet,
    typed: bool,
) -> Result<(), String> {
    let columns: Vec<String> = (0..sheet.columns.len())
        .map(|i| format!("c{i} VARCHAR"))
        .collect();
    conn.execute_batch(&format!("CREATE TABLE {raw} ({});", columns.join(", ")))
        .map_err(|e| format!("Excel の表の準備に失敗: {e}"))?;
    {
        let mut app = conn
            .appender(raw)
            .map_err(|e| format!("Excel の表の準備に失敗: {e}"))?;
        // 1 行ずつ幅の分に広げて書く（表全体を密に確保しない）。
        let mut dense: Vec<Option<&str>> = vec![None; sheet.columns.len()];
        for row in &sheet.rows {
            dense.fill(None);
            for (col, text) in row {
                dense[*col] = Some(text.as_str());
            }
            app.append_row(appender_params_from_iter(dense.iter().copied()))
                .map_err(|e| format!("Excel の表の書き込みに失敗: {e}"))?;
        }
        app.flush()
            .map_err(|e| format!("Excel の表の書き込みに失敗: {e}"))?;
    }
    let selected: Vec<String> = sheet
        .columns
        .iter()
        .zip(&sheet.types)
        .enumerate()
        .map(|(i, (name, ty))| {
            if typed && *ty != "VARCHAR" {
                format!("CAST(c{i} AS {ty}) AS {}", quote(name))
            } else {
                format!("c{i} AS {}", quote(name))
            }
        })
        .collect();
    conn.execute_batch(&format!(
        "CREATE TABLE {} AS SELECT {} FROM {raw}; DROP TABLE {raw};",
        quote(table),
        selected.join(", ")
    ))
    .map_err(|e| format!("Excel の表の作成に失敗: {e}"))
}

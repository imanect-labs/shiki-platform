//! 隔離ランナーとの JSON プロトコル（Task 11P.7）。
//!
//! api 側の [`crate::service::TabularService`] は、検証済みハンドル（信頼できる一時ファイル
//! パス）と検証済み SQL・クォータを [`RunnerRequest`] にして stdin へ渡し、[`RunnerResponse`]
//! を stdout から受け取る。ランナー（`shiki-tabular-runner`）は資格情報を一切持たない
//! （INV: 認可は api 側で完了済み・ランナーは渡されたファイルのみを触る）。

use serde::{Deserialize, Serialize};

/// ランナーへの 1 リクエスト（1 プロセス = 1 実行）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerRequest {
    /// 実行種別。
    pub op: RunnerOp,
    /// 対象 CSV の**信頼できる**ローカルパス（api が StorageService から取得して置いた一時ファイル）。
    /// [`InputFormat::Xlsx`] のときは Excel のファイル（そのままのバイト列）。
    pub csv_path: String,
    /// 入力の形式（省略は CSV・この項目が無かったころのランナーと同じ）。
    #[serde(default)]
    pub format: InputFormat,
    /// メモリ上限（MB）。DuckDB の memory_limit に設定する。
    pub memory_limit_mb: u32,
    /// 結果の最大行数（これを超える行は返さない・ページングで取得）。
    pub max_rows: u32,
    /// 集計（[`RunnerOp::Query`]）の結果の大きさの上限（バイト・[`result_row_bytes`] で数える・GAP-92）。超える行は
    /// 返さず、[`RunnerResponse::truncated`] を立てる。省略は上限なし（この項目が無かったころの api と同じ）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_result_bytes: Option<u64>,
}

/// 集計の結果の大きさの上限の既定（8 MiB）。ワークフローの「CSV を集計」の結果（rows と items）がこれを超えないように、
/// 超える行の手前で打ち切る（GAP-92）。外部への送信の本文の上限（`http.request`）と同じ大きさ。
pub const DEFAULT_MAX_RESULT_BYTES: u64 = 8 << 20;

/// 集計の結果の 1 行の大きさ（バイト）。ワークフローの結果に入れる形で数える: 文字の形の行（`rows` の 1 行・
/// `["…", null]`）と、列の名前つきの行（`items` の 1 行・`{"列の名前": 値}`）を JSON にした大きさと、それぞれの
/// 並びの区切りの `,` の和。
#[must_use]
pub fn result_row_bytes(
    columns: &[String],
    texts: &[Option<String>],
    values: &[serde_json::Value],
) -> u64 {
    let separators = |n: usize| n.saturating_sub(1);
    let row = 2
        + texts
            .iter()
            .map(|t| t.as_deref().map_or(4, json_string_bytes))
            .sum::<usize>()
        + separators(texts.len());
    let item = 2
        + columns
            .iter()
            .zip(values)
            .map(|(name, value)| json_string_bytes(name) + 1 + json_value_bytes(value))
            .sum::<usize>()
        + separators(columns.len().min(values.len()));
    (row + item + 2) as u64
}

/// 文字を JSON の文字列にした大きさ（引用符と、`"`・`\`・制御文字のエスケープを含む）。
fn json_string_bytes(s: &str) -> usize {
    2 + s
        .chars()
        .map(|c| match c {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
            c if (c as u32) < 0x20 => 6,
            c => c.len_utf8(),
        })
        .sum::<usize>()
}

/// 値を JSON にした大きさ。
fn json_value_bytes(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::String(s) => json_string_bytes(s),
        serde_json::Value::Null | serde_json::Value::Bool(true) => 4,
        serde_json::Value::Bool(false) => 5,
        other => other.to_string().len(),
    }
}

/// ランナーのプロセスのアドレス空間に、DuckDB の memory_limit へ足す余裕（MiB）。実行ファイル・Excel の読み取り
/// （`crate::input` の上限の内）・DuckDB の外の確保に使う。
const RUNNER_ADDRESS_SPACE_HEADROOM_MB: u64 = 1536;

/// ランナーのプロセスのアドレス空間の上限（バイト）。[`RunnerRequest::memory_limit_mb`] に余裕を足す。ランナーは
/// 入力を読む前に自分にこの上限を付け（RLIMIT_AS）、超えると確保に失敗して落ちる。api はメモリの超過として返す
/// （DuckDB の memory_limit は Rust 側の確保に効かないため・PIT-39）。
#[must_use]
pub fn runner_address_space_bytes(memory_limit_mb: u32) -> u64 {
    (u64::from(memory_limit_mb) + RUNNER_ADDRESS_SPACE_HEADROOM_MB) << 20
}

/// Excel のブックのシート（表のシートだけ・ブックの順・GAP-84）。
///
/// `data` は表示している左端のシート（D-148）。ほかのシートは、SQL に `FROM "シート名"` と書くと、その名前の表として
/// 読める（`data` のシートも名前で読める）。DuckDB の名前は大文字と小文字を区別しないので、`data` と同じ名前のほかの
/// シートと、前のシートと大文字と小文字だけが違う名前のシート、名前の無いシートは名前で読めない（[`SheetInfo::usable`]）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheetInfo {
    /// シートの名前。
    pub name: String,
    /// 隠したシート（名前で読める・`data` にはならない。見えるシートが無いブックだけは左端のシートを `data` にする）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
    /// `data` として読むシート。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub data: bool,
    /// SQL で名前を書いて読めるか。
    pub usable: bool,
}

/// ランナーへ渡す入力の形式（api は先頭のバイトだけで決め、中身は解釈しない・PIT-39）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputFormat {
    /// UTF-8 にした CSV（文字コードは api が決めて直す・GAP-45）。
    #[default]
    Csv,
    /// Excel のブック（.xlsx）。ランナーが展開し、左端のシートを表として読む（CSV を集計の読み取りだけ・D-17）。
    Xlsx,
}

/// ランナーの実行種別。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunnerOp {
    /// スキーマ（列名・型）だけを返す。
    Schema,
    /// ページ取得（安定行番号順・offset/limit）。
    Rows { offset: u64 },
    /// 読み取り専用 SQL（**検証済み**・単一 SELECT/WITH）。テーブル名は `data`（Excel はほかのシートもシートの名前で
    /// 読める・[`SheetInfo`]）。
    Query { sql: String },
    /// 区切り文字と改行を推定して返す（[`RunnerResponse::dialect`]）。集計の読み込み（`read_csv_auto`）と同じ推定器
    /// （DuckDB の `sniff_csv`）を同じ引数で使い、CSV の編集が集計と同じ区切りで読み書きする（BUG-144）。
    Dialect,
}

/// CSV の区切りと改行（[`RunnerOp::Dialect`] の答え）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CsvDialect {
    /// 区切り文字（`,`・タブ・`;`・`|` など）。
    pub delimiter: String,
    /// 行の終わり（`\n` か `\r\n`）。
    pub newline: String,
}

impl Default for CsvDialect {
    /// 推定しないとき（中身が空など）の形。以前の編集が決め打ちしていた形と同じ。
    fn default() -> Self {
        CsvDialect {
            delimiter: ",".into(),
            newline: "\n".into(),
        }
    }
}

/// ランナーの応答。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunnerResponse {
    /// 成功可否。false のとき `error` に理由。
    pub ok: bool,
    /// 列名（順序どおり）。
    #[serde(default)]
    pub columns: Vec<String>,
    /// 列の型名（DuckDB 型・columns と同順）。Schema は集計（Query）で読むときの型（CSV は DuckDB の推定・
    /// Excel はセルの種類から決めた型）、Query は結果の列の型。Rows は空。
    #[serde(default)]
    pub column_types: Vec<String>,
    /// 行（各行はセル文字列の配列・NULL は None）。日付・時刻・小数（DECIMAL）などは DuckDB の文字の形
    /// （`2024-01-31`・`1.50` など）。
    #[serde(default)]
    pub rows: Vec<Vec<Option<String>>>,
    /// 値の種類を保った行（Query のときだけ・`rows` と同じ並び）。数・真偽・空はそのまま、日付と時刻は `rows` と
    /// 同じ文字。JavaScript で正確に扱えない大きな整数（±2^53 を超える）と、無限大・非数も文字。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<Vec<serde_json::Value>>,
    /// テーブル総行数（Schema/Rows で返す・ページング UI 用）。Query は打ち切らなかったときの結果の行の数。
    #[serde(default)]
    pub total_rows: Option<u64>,
    /// max_rows で打ち切ったか。
    #[serde(default)]
    pub truncated: bool,
    /// エラー理由（ok=false のとき）。
    #[serde(default)]
    pub error: Option<String>,
    /// 渡したファイルを表として読めなかった（壊れた・上限を超える Excel など）。`error` は利用者向けの文言で、
    /// 利用者が直す誤り（ランナーの異常ではない）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub input_error: bool,
    /// 推定した区切りと改行（[`RunnerOp::Dialect`] のときだけ）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dialect: Option<CsvDialect>,
    /// Excel のシート（Excel のときだけ・GAP-84）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sheets: Vec<SheetInfo>,
    /// `data` の見出し（列の名前）にした行の番号（Excel の行の番号・1 から）。表の上の表題の行を飛ばしたときだけ
    /// （GAP-83）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_row: Option<u32>,
}

impl RunnerResponse {
    pub fn failure(msg: impl Into<String>) -> Self {
        RunnerResponse {
            ok: false,
            error: Some(msg.into()),
            ..RunnerResponse::default()
        }
    }

    /// 渡したファイルを読めなかった失敗（[`RunnerResponse::input_error`]・文言は利用者向け）。
    pub fn input_failure(msg: impl Into<String>) -> Self {
        RunnerResponse {
            input_error: true,
            ..RunnerResponse::failure(msg)
        }
    }
}

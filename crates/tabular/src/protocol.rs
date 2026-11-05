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
    pub csv_path: String,
    /// メモリ上限（MB）。DuckDB の memory_limit に設定する。
    pub memory_limit_mb: u32,
    /// 結果の最大行数（これを超える行は返さない・ページングで取得）。
    pub max_rows: u32,
}

/// ランナーの実行種別。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunnerOp {
    /// スキーマ（列名・型）だけを返す。
    Schema,
    /// ページ取得（安定行番号順・offset/limit）。
    Rows { offset: u64 },
    /// 読み取り専用 SQL（**検証済み**・単一 SELECT/WITH）。テーブル名は `data`。
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
    /// 列の型名（DuckDB 型・columns と同順）。Schema は集計（Query）で読むときの型（DuckDB の推定）、
    /// Query は結果の列の型。Rows は空。
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
    /// 推定した区切りと改行（[`RunnerOp::Dialect`] のときだけ）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dialect: Option<CsvDialect>,
}

impl RunnerResponse {
    pub fn failure(msg: impl Into<String>) -> Self {
        RunnerResponse {
            ok: false,
            error: Some(msg.into()),
            ..RunnerResponse::default()
        }
    }
}

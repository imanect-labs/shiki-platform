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
}

/// ランナーの応答。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerResponse {
    /// 成功可否。false のとき `error` に理由。
    pub ok: bool,
    /// 列名（順序どおり）。
    #[serde(default)]
    pub columns: Vec<String>,
    /// 列の型名（DuckDB 型・columns と同順）。
    #[serde(default)]
    pub column_types: Vec<String>,
    /// 行（各行はセル文字列の配列・NULL は None）。
    #[serde(default)]
    pub rows: Vec<Vec<Option<String>>>,
    /// テーブル総行数（Schema/Rows で返す・ページング UI 用）。
    #[serde(default)]
    pub total_rows: Option<u64>,
    /// max_rows で打ち切ったか。
    #[serde(default)]
    pub truncated: bool,
    /// エラー理由（ok=false のとき）。
    #[serde(default)]
    pub error: Option<String>,
}

impl RunnerResponse {
    pub fn failure(msg: impl Into<String>) -> Self {
        RunnerResponse {
            ok: false,
            columns: Vec::new(),
            column_types: Vec::new(),
            rows: Vec::new(),
            total_rows: None,
            truncated: false,
            error: Some(msg.into()),
        }
    }
}

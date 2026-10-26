//! 起動した本人に返す run の結果の型（GAP-09・D-28）。gui（ミニアプリ）と chat（会話の図のボタン）が共有し、
//! OpenAPI（utoipa）から TS の型を生成する（手書きの対応物を作らない）。
//!
//! - [`ReplySection`] は `workflow.reply` の step の出力そのもの（executor が整形して書き、読取口が読み戻す）。
//!   正本は step の出力で、別の場所へ写さない（実行履歴の保持期間に従う）。
//! - [`RunFailure`] は失敗した step の code から読取のときに決める（生のエラー文は返さない・[`super::failure`]）。
//! - [`WaitingFor`] は running の run の導出値（engine.md §3.1 の waiting）。

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

pub use crate::ir::params::ReplyTone;

/// 画面に出す結果の 1 節（`workflow.reply` の出力）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ReplySection {
    /// 見出し（作成者が書いた固定の文言）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub tone: ReplyTone,
    /// 上限で切り詰めたか（文字・行・件数）。
    pub truncated: bool,
    #[serde(flatten)]
    pub body: ReplyBody,
}

/// 節の中身（`format` で分ける）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "format", rename_all = "snake_case")]
pub enum ReplyBody {
    /// 文章（改行を保って表示する）。
    Text { text: String },
    /// Markdown（読取のときに操作を持つ図を表示専用へ書き換える）。
    Markdown { text: String },
    /// 表（セルは表示用の文字）。
    Table {
        columns: Vec<String>,
        rows: Vec<Vec<String>>,
        /// 切り詰める前の行数。
        total_rows: u32,
    },
    /// ドライブのファイルへのリンク（開くときに通常の権限で確かめる）。
    FileLink { files: Vec<ReplyFile> },
}

/// ファイルへのリンク 1 件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReplyFile {
    pub id: Uuid,
    pub name: String,
}

/// run が返した結果（実行順）。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, ToSchema)]
pub struct RunReply {
    pub sections: Vec<ReplySection>,
    /// 上限（10 節・32KB）を超えて省いた節の数。
    pub omitted: u32,
}

/// 待機中の run が待っているもの（running の導出値）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WaitingFor {
    /// 時間・時刻まで待つ。
    Timer,
    /// できごと（ファイルの保存・データの変更）を待つ。
    Event,
    /// 担当者の承認を待つ。
    Approval,
}

impl WaitingFor {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            WaitingFor::Timer => "timer",
            WaitingFor::Event => "event",
            WaitingFor::Approval => "approval",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "timer" => WaitingFor::Timer,
            "event" => WaitingFor::Event,
            "approval" => WaitingFor::Approval,
            _ => return None,
        })
    }
}

/// 失敗の分類（利用者向けの文言の単位）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FailureCategory {
    /// 権限が足りない。
    Permission,
    /// ほかの操作と重なった・同じ値がある。
    Conflict,
    /// 対象が見つからない。
    NotFound,
    /// 一時的な問題。
    Temporary,
    /// 利用の上限。
    Quota,
    /// 時間切れ。
    Timeout,
    /// ワークフローの設定の誤り。
    Configuration,
    /// 上記以外。
    Unknown,
}

/// 失敗した run の理由（利用者向け・生のエラー文は含めない）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RunFailure {
    pub category: FailureCategory,
    /// 利用者向けの文言。
    pub message: String,
    /// 参照番号（run_id の先頭 8 桁・作成者が実行履歴で探す手がかり）。
    pub reference: String,
}

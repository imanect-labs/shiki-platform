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

/// 失敗した run を「失敗したところから再開」したときに、結果が変わりうるか（BUG-132・分類は
/// [`crate::run::failure::resume_advice`] の単一定義）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResumeAdvice {
    /// 再開すると、失敗したブロックから続けられる（一時的な問題・ブロックの時間切れ・AI の答え・分類できない失敗）。
    Resume,
    /// 原因（権限・データ・相手のサービス・組織の設定など、この実行の外にあるもの）を直してから再開すると、失敗した
    /// ブロックから続けられる。
    FixFirst,
    /// 再開しても同じ結果になる（原因が、この実行のバージョンの設定か、入力や前のブロックの結果の値にある。再開は
    /// どれも同じものを使う）。
    SameResult,
    /// 実行の期限を過ぎているので、再開してもすぐに止まる（再開は実行の期限を延ばさない）。
    PastDeadline,
}

impl ResumeAdvice {
    /// 直列化した名前（再開の API が断るときの code・serde の名前と同じ）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ResumeAdvice::Resume => "resume",
            ResumeAdvice::FixFirst => "fix_first",
            ResumeAdvice::SameResult => "same_result",
            ResumeAdvice::PastDeadline => "past_deadline",
        }
    }
}

/// 「失敗したところから再開」を勧めるかと、ボタンの近くに出す理由（BUG-132）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ResumeHint {
    pub advice: ResumeAdvice,
    /// 再開を勧めるか（[`ResumeAdvice::Resume`]・[`ResumeAdvice::FixFirst`]）。勧めないときは再開のボタンを出さない。
    pub offer: bool,
    /// 「同じ入力でもう一度実行」（この実行のバージョンで新しく実行する）を勧めるか。この実行のバージョンの設定だけで
    /// 決まる失敗（使ってよい操作の天井など）では勧めない。設定を直して保存した後でも、同じ失敗をくり返す。
    pub offer_rerun: bool,
    /// 再開で何が起きるか、または再開を勧めない理由と代わりの手段（作成者向け）。
    pub note: String,
}

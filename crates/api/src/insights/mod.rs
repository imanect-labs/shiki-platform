//! AI 活用分析（#751）: 従業員がどれだけ・何に AI を使い、何が良くなったか。
//!
//! - [`rollup`][]: 利用者×日のロールアップ（`ai_activity_daily`）を日単位で作り直す。
//! - [`classify`][]: 会話の用途ラベル（`thread_insight`）を AI で付ける（組織の設定でオンのとき）。
//! - [`timer`][]: 上の 2 つを定期実行する（複数インスタンスでも advisory lock で 1 つだけ）。
//! - [`report`][]: 管理画面（`/org-admin/insights/*`）の集計。少人数の区分は伏せる（PIT-17）。
//! - [`settings`][]: 組織の分析設定（分類・個人別表示・前提時間・時給）。
//! - [`service`][]: 管理 API の窓口（範囲は常に `AuthContext` から）。
//!
//! 管理者は会話本文を読めない（PIT-65）。ここで扱う本文は分類のために LLM へ渡すユーザー発話の
//! 抜粋だけで、保存するのはラベルだけ。

pub mod classify;
pub mod report;
pub mod rollup;
pub mod service;
pub mod settings;
pub mod timer;

pub use service::{InsightsService, Range};

use serde::{Deserialize, Serialize};

/// 集計で区分を出す最小人数（これ未満は数値を伏せる・PIT-17）。
pub const MIN_GROUP_SIZE: i64 = 5;

/// 分類器の利用を会計に刻むときの主体 ID（利用者の集計からは除く）。
pub const CLASSIFIER_PRINCIPAL: &str = "system:insights";

/// 用途ラベル（`thread_insight.category` の CHECK と同じ集合・表示順）。
pub const CATEGORIES: &[(&str, &str, i32)] = &[
    // (キー, 表示名, 人がやると何分か の既定値)
    ("writing", "文書作成", 15),
    ("summarize", "要約", 12),
    ("research", "調査・情報収集", 10),
    ("internal_search", "社内文書の検索・問い合わせ", 6),
    ("data_analysis", "データ分析・表計算", 18),
    ("translation", "翻訳", 8),
    ("ideation", "アイデア出し・壁打ち", 8),
    ("coding", "プログラミング", 15),
    ("other", "その他", 5),
];

/// 用途キーの表示名（未知はキーのまま）。
pub fn category_label(key: &str) -> &str {
    CATEGORIES
        .iter()
        .find(|(k, _, _)| *k == key)
        .map_or(key, |(_, label, _)| label)
}

/// 定期集計の設定（`SHIKI__INSIGHTS__*`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsightsConfig {
    /// 定期集計を動かすか（既定 true）。止めても画面は既存の集計を読む。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 実行間隔（秒）。既定 1 時間（下限 60 秒）。
    #[serde(default = "default_interval_secs")]
    pub interval_secs: u64,
    /// 1 回の実行で 1 組織あたり分類する会話の上限（LLM 呼び出しの上限）。
    #[serde(default = "default_classify_batch")]
    pub classify_batch: i64,
    /// 分類に使うモデル（論理名）。省略は組織の既定（Auto の振り先など）。
    #[serde(default)]
    pub classifier_model: Option<String>,
}

impl Default for InsightsConfig {
    fn default() -> Self {
        InsightsConfig {
            enabled: true,
            interval_secs: default_interval_secs(),
            classify_batch: default_classify_batch(),
            classifier_model: None,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_interval_secs() -> u64 {
    3600
}

fn default_classify_batch() -> i64 {
    200
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_have_labels_and_defaults() {
        assert_eq!(category_label("writing"), "文書作成");
        assert_eq!(category_label("unknown"), "unknown");
        assert!(CATEGORIES.iter().all(|(_, _, m)| *m > 0));
        assert_eq!(CATEGORIES.len(), 9);
    }
}

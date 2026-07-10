//! ワークフロー IR（唯一の正本・JSON DAG・ir.md §1/§2）。
//!
//! deny-unknown（全階層）でスキーマ検証可能な構造化データに閉じる。編集手段は dnd と
//! AI 編集のみ（本 crate は保存時検証と実行を担う）。

pub mod edge;
pub mod expr;
pub mod node;
pub mod params;
pub mod trigger;
pub mod validate;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub use edge::Edge;
pub use node::{Backoff, Node, OnError, RetryPolicy};
pub use trigger::{Trigger, TriggerKind};

/// IR エンベロープ（トップレベル・ir.md §2）。
///
/// `deny_unknown_fields` によりフィールド追加でも `ir_version` を上げる（deny-unknown 方針）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct WorkflowIr {
    /// IR バージョン（単調増加・未知の版は保存拒否・§9）。
    pub ir_version: u32,
    /// ワークフロー名（`^[a-z][a-z0-9-]{0,63}$`・tenant 内一意は artifact 層が担保）。
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 宣言スコープ（権限の天井・codegen 語彙へ V3 照合）。
    #[serde(default)]
    pub declared_scopes: Vec<String>,
    /// トリガ（schedule / event / interactive）。
    #[serde(default)]
    pub triggers: Vec<Trigger>,
    /// 対話トリガの入力スキーマ（JSON Schema サブセット・省略可）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "unknown")]
    pub input_schema: Option<serde_json::Value>,
    /// ノード。
    #[serde(default)]
    pub nodes: Vec<Node>,
    /// エッジ。
    #[serde(default)]
    pub edges: Vec<Edge>,
    /// 実行ポリシ（省略時全既定値）。
    #[serde(default)]
    pub policies: Policies,
}

/// このエンジンが受理する最大 IR 版（未知の版は保存拒否・§9）。
pub const MAX_IR_VERSION: u32 = 1;

/// 実行ポリシ（ir.md §2）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct Policies {
    /// run タイムアウト（秒・既定 3 日・最大 30 日）。
    #[serde(default = "default_run_timeout")]
    pub run_timeout_sec: u32,
    /// 同時実行 run 上限（既定 10）。
    #[serde(default = "default_max_parallel_runs")]
    pub max_parallel_runs: u32,
    /// トリガ溢れ時の扱い（queue 既定 / skip）。
    #[serde(default)]
    pub on_trigger_overflow: TriggerOverflow,
}

impl Default for Policies {
    fn default() -> Self {
        Policies {
            run_timeout_sec: default_run_timeout(),
            max_parallel_runs: default_max_parallel_runs(),
            on_trigger_overflow: TriggerOverflow::default(),
        }
    }
}

fn default_run_timeout() -> u32 {
    259_200 // 3 日
}
fn default_max_parallel_runs() -> u32 {
    10
}

/// run タイムアウトの最大（30 日）。
pub const MAX_RUN_TIMEOUT_SEC: u32 = 30 * 24 * 3600;

/// トリガ溢れの扱い。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum TriggerOverflow {
    /// queued のまま滞留（既定・バックプレッシャ）。
    #[default]
    Queue,
    /// run を作らず occurrence 記録のみ。
    Skip,
}

impl WorkflowIr {
    /// JSON からパースする（deny-unknown・スキーマ検証 V1 相当の serde 検証）。
    pub fn from_json(value: &serde_json::Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(value.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn minimal_ir_parses_with_defaults() {
        let ir = WorkflowIr::from_json(&json!({
            "ir_version": 1,
            "name": "daily-report",
            "nodes": [],
            "edges": []
        }))
        .unwrap();
        assert_eq!(ir.policies.run_timeout_sec, 259_200);
        assert_eq!(ir.policies.max_parallel_runs, 10);
        assert_eq!(ir.policies.on_trigger_overflow, TriggerOverflow::Queue);
    }

    #[test]
    fn deny_unknown_top_level() {
        let bad = WorkflowIr::from_json(&json!({
            "ir_version": 1,
            "name": "x",
            "surprise": true
        }));
        assert!(bad.is_err(), "未知トップレベルフィールドは拒否");
    }
}

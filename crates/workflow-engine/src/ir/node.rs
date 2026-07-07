//! ノード・リトライポリシ・エラーハンドリング（ir.md §4）。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// ノード id の形式（`^[a-z][a-z0-9_]{0,63}$`）。
pub const NODE_ID_RE: &str = r"^[a-z][a-z0-9_]{0,63}$";

/// 1 ノード（type ごとの params は検証時に vocab＋個別スキーマで照合）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct Node {
    /// ワークフロー内一意の id。
    pub id: String,
    /// ノード種（`storage.write` 等・vocab の閉集合へ V3 照合）。
    #[serde(rename = "type")]
    pub node_type: String,
    /// 表示名（省略可）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// map 領域内なら親 map の id、領域外は null（ir.md §5）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// ノード種ごとのパラメータ（codegen が正・検証時に個別照合）。
    #[serde(default)]
    #[ts(type = "unknown")]
    pub params: serde_json::Value,
    /// リトライポリシ（既定 max_attempts=1＝リトライなし）。
    #[serde(default)]
    pub retry: RetryPolicy,
    /// ステップタイムアウト（秒・省略時はノード種の既定）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_sec: Option<u32>,
    /// エラー時の扱い（既定 fail_run）。
    #[serde(default)]
    pub on_error: OnError,
}

/// リトライポリシ（ir.md §4・PIT-31 思想で既定リトライなし）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct RetryPolicy {
    /// 最大試行回数（既定 1＝リトライなし）。
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    /// バックオフ（省略時は指数の既定）。
    #[serde(default)]
    pub backoff: Backoff,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            max_attempts: default_max_attempts(),
            backoff: Backoff::default(),
        }
    }
}

fn default_max_attempts() -> u32 {
    1
}

/// 指数バックオフ（full jitter は実行時・engine.md §7）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct Backoff {
    #[serde(default = "default_base_sec")]
    pub base_sec: u32,
    #[serde(default = "default_max_sec")]
    pub max_sec: u32,
    #[serde(default = "default_jitter")]
    pub jitter: bool,
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff {
            base_sec: default_base_sec(),
            max_sec: default_max_sec(),
            jitter: default_jitter(),
        }
    }
}

fn default_base_sec() -> u32 {
    2
}
fn default_max_sec() -> u32 {
    300
}
fn default_jitter() -> bool {
    true
}

/// エラー時の扱い（ir.md §4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum OnError {
    /// run 全体を失敗させる（既定）。
    #[default]
    FailRun,
    /// `error` ポートへ流して継続する。
    Continue,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn node_defaults() {
        let n: Node = serde_json::from_value(json!({
            "id": "read_file",
            "type": "storage.read",
            "params": { "id": { "$from": "input", "path": "/file_id" } }
        }))
        .unwrap();
        assert_eq!(n.retry.max_attempts, 1);
        assert_eq!(n.on_error, OnError::FailRun);
        assert!(n.parent.is_none());
    }

    #[test]
    fn node_deny_unknown() {
        let bad: Result<Node, _> = serde_json::from_value(json!({
            "id": "x", "type": "storage.read", "bogus": 1
        }));
        assert!(bad.is_err());
    }

    #[test]
    fn retry_backoff_defaults() {
        let r = RetryPolicy::default();
        assert_eq!(r.max_attempts, 1);
        assert_eq!(r.backoff.base_sec, 2);
        assert!(r.backoff.jitter);
    }
}

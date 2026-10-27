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
    /// 結果のキャッシュ（step 属性・ir.md §4・既定は使わない）。いまは ai.decide の決定キャッシュだけ
    /// （ほかの種で書くと保存時に `ir.cache_unsupported`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<StepCache>,
}

/// step 属性 `cache`（ai.decide の決定キャッシュ・ir.md §7.11・D-52 の H6）。
///
/// 同じ決定器の版・同じ質問・同じ判定材料の答えを、同じワークフローの中で `ttl_sec` のあいだ使い回す
/// （範囲はテナント＋組織＋ワークフロー）。当たっても組織の許可は確かめ、会計はしない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct StepCache {
    /// 答えを使い回す期間（秒・省略は [`DEFAULT_CACHE_TTL_SEC`]・[`MIN_CACHE_TTL_SEC`]〜[`MAX_CACHE_TTL_SEC`]）。
    #[serde(default = "default_cache_ttl_sec")]
    pub ttl_sec: u32,
}

/// `cache.ttl_sec` の既定（1 日）。
pub const DEFAULT_CACHE_TTL_SEC: u32 = 86_400;
/// `cache.ttl_sec` の下限（1 分）。
pub const MIN_CACHE_TTL_SEC: u32 = 60;
/// `cache.ttl_sec` の上限（30 日。判定の答えを run の保持より長く溜め込まない目安）。
pub const MAX_CACHE_TTL_SEC: u32 = 30 * 86_400;

const fn default_cache_ttl_sec() -> u32 {
    DEFAULT_CACHE_TTL_SEC
}

/// step 属性 `cache` を付けられる種の決まり（ノードカタログの `step_cache`・web のフォームが読む）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
pub struct StepCacheLimits {
    pub default_ttl_sec: u32,
    pub min_ttl_sec: u32,
    pub max_ttl_sec: u32,
}

impl StepCacheLimits {
    /// 決定キャッシュの決まり（[`DEFAULT_CACHE_TTL_SEC`] ほか）。
    pub const DECISION: StepCacheLimits = StepCacheLimits {
        default_ttl_sec: DEFAULT_CACHE_TTL_SEC,
        min_ttl_sec: MIN_CACHE_TTL_SEC,
        max_ttl_sec: MAX_CACHE_TTL_SEC,
    };

    /// `ttl_sec` が範囲の内か。
    #[must_use]
    pub const fn admits(self, ttl_sec: u32) -> bool {
        ttl_sec >= self.min_ttl_sec && ttl_sec <= self.max_ttl_sec
    }
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
        assert!(n.cache.is_none(), "キャッシュは既定で使わない");
    }

    #[test]
    fn step_cache_defaults_its_ttl_and_rejects_unknown_keys() {
        let n: Node = serde_json::from_value(json!({
            "id": "judge", "type": "ai.decide", "cache": {}
        }))
        .unwrap();
        assert_eq!(
            n.cache,
            Some(StepCache {
                ttl_sec: DEFAULT_CACHE_TTL_SEC
            })
        );
        // 書かなければ直列化にも出さない（既存の IR の見た目を変えない）。
        let plain: Node =
            serde_json::from_value(json!({ "id": "x", "type": "ai.decide" })).unwrap();
        assert!(serde_json::to_value(&plain).unwrap().get("cache").is_none());
        let bad: Result<Node, _> = serde_json::from_value(json!({
            "id": "x", "type": "ai.decide", "cache": { "key": "mine" }
        }));
        assert!(bad.is_err(), "キーは選べない（版・質問・材料から決まる）");
        assert!(StepCacheLimits::DECISION.admits(MIN_CACHE_TTL_SEC));
        assert!(!StepCacheLimits::DECISION.admits(MAX_CACHE_TTL_SEC + 1));
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

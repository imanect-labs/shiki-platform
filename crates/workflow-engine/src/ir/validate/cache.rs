//! step 属性 `cache`（ai.decide の決定キャッシュ・T10・D-52 の H6）の保存時検証（V1 の形）。
//!
//! - 付けられるのはノードカタログの `step_cache` がある種だけ（いまは ai.decide）。ほかの種に書いたものは
//!   `ir.cache_unsupported`（黙って無視すると、使い回されると思い込んだまま費用が出る・逆も同じ）。
//! - `ttl_sec` はその種の決まり（[`crate::ir::StepCacheLimits`]）の範囲の内（外は `ir.schema_violation`）。

use super::ValidationError;
use crate::ir::WorkflowIr;
use crate::vocab::{step_cache, NodeType};

/// エラーのコード: キャッシュを付けられない種に `cache` を書いた。
pub const CACHE_UNSUPPORTED: &str = "ir.cache_unsupported";

/// 期間を人が読める単位で（秒・分・時間・日）。
fn human(secs: u32) -> String {
    match secs {
        s if s % 86_400 == 0 => format!("{} 日", s / 86_400),
        s if s % 3600 == 0 => format!("{} 時間", s / 3600),
        s if s % 60 == 0 => format!("{} 分", s / 60),
        s => format!("{s} 秒"),
    }
}

/// V1: `cache` の形。
pub(super) fn v1(ir: &WorkflowIr, errors: &mut Vec<ValidationError>) {
    for node in &ir.nodes {
        let Some(cache) = node.cache else {
            continue;
        };
        // 未知・予約の種は V3 が報告する。
        let Some(nt) = NodeType::parse(&node.node_type) else {
            continue;
        };
        let Some(limits) = step_cache(nt) else {
            errors.push(
                ValidationError::new(
                    CACHE_UNSUPPORTED,
                    format!(
                        "{} では結果を使い回せません（cache を書けるのは AI で判定〔ai.decide〕だけです）。cache を外してください",
                        node.node_type
                    ),
                )
                .at_node(&node.id)
                .at_path("/cache"),
            );
            continue;
        };
        if !limits.admits(cache.ttl_sec) {
            errors.push(
                ValidationError::new(
                    "ir.schema_violation",
                    format!(
                        "答えを使い回す期間（cache.ttl_sec）は {}〜{}（{}〜{} 秒）にしてください: {}",
                        human(limits.min_ttl_sec),
                        human(limits.max_ttl_sec),
                        limits.min_ttl_sec,
                        limits.max_ttl_sec,
                        cache.ttl_sec
                    ),
                )
                .at_node(&node.id)
                .at_path("/cache/ttl_sec"),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::needless_pass_by_value)]
    use serde_json::{json, Value};

    use super::*;
    use crate::ir::validate::{validate, Catalog};
    use crate::ir::{MAX_CACHE_TTL_SEC, MIN_CACHE_TTL_SEC};

    fn decide(cache: Value) -> Value {
        json!({ "id": "judge", "type": "ai.decide", "cache": cache, "params": {
            "model": "decide-stub",
            "state": "本文",
            "questions": [ { "key": "angry", "type": "boolean", "instructions": "不満か" } ] } })
    }

    fn ir(nodes: Value) -> Value {
        json!({ "ir_version": 1, "name": "wf", "declared_scopes": [], "nodes": nodes, "edges": [] })
    }

    fn errors(value: &Value) -> Vec<ValidationError> {
        validate(value, &Catalog::default())
            .err()
            .unwrap_or_default()
    }

    #[test]
    fn ai_decide_accepts_a_cache_within_the_limits() {
        for ttl in [MIN_CACHE_TTL_SEC, 3600, MAX_CACHE_TTL_SEC] {
            let v = ir(json!([decide(json!({ "ttl_sec": ttl }))]));
            assert!(errors(&v).is_empty(), "{ttl}: {:?}", errors(&v));
        }
        assert!(errors(&ir(json!([decide(json!({}))]))).is_empty());
    }

    #[test]
    fn the_ttl_outside_the_limits_is_rejected_at_its_path() {
        for ttl in [0, MIN_CACHE_TTL_SEC - 1, MAX_CACHE_TTL_SEC + 1] {
            let errs = errors(&ir(json!([decide(json!({ "ttl_sec": ttl }))])));
            assert_eq!(errs.len(), 1, "{ttl}: {errs:?}");
            assert_eq!(errs[0].code, "ir.schema_violation");
            assert_eq!(errs[0].node_id.as_deref(), Some("judge"));
            assert_eq!(errs[0].path.as_deref(), Some("/cache/ttl_sec"));
            assert!(
                errs[0].message.contains("1 分〜30 日"),
                "{}",
                errs[0].message
            );
        }
    }

    #[test]
    fn other_node_types_cannot_carry_a_cache() {
        let v = ir(
            json!([{ "id": "ask", "type": "llm.invoke", "cache": { "ttl_sec": 3600 },
                           "params": { "prompt": "こんにちは" } }]),
        );
        let errs = errors(&v);
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert_eq!(errs[0].code, CACHE_UNSUPPORTED);
        assert_eq!(errs[0].path.as_deref(), Some("/cache"));
        // 書かなければ従来どおり。
        let plain = ir(json!([{ "id": "ask", "type": "llm.invoke", "params": { "prompt": "x" } }]));
        assert!(errors(&plain).is_empty(), "{:?}", errors(&plain));
    }

    #[test]
    fn durations_read_in_the_largest_whole_unit() {
        assert_eq!(human(60), "1 分");
        assert_eq!(human(7200), "2 時間");
        assert_eq!(human(86_400 * 30), "30 日");
        assert_eq!(human(61), "61 秒");
    }
}

//! プロバイダの `usage` を中立形の [`Usage`] へ写す（#581）。
//!
//! 中立形の `prompt_tokens` は**キャッシュ分を含む入力の総数**に揃える（[`Usage`] の定義）。
//! プロバイダごとの違い:
//!
//! | 形式 | 入力の総数 | キャッシュ読み出し | キャッシュ書き込み |
//! | --- | --- | --- | --- |
//! | Chat Completions | `prompt_tokens`（含む） | `prompt_tokens_details.cached_tokens` | `prompt_tokens_details.cache_write_tokens`（OpenRouter） |
//! | Responses | `input_tokens`（含む） | `input_tokens_details.cached_tokens` | `input_tokens_details.cache_write_tokens`（報告があれば） |
//! | Anthropic Messages | `input_tokens`（**含まない**） | `cache_read_input_tokens` | `cache_creation_input_tokens` |
//!
//! Anthropic だけは総数を足し合わせて作る。

use serde_json::Value;

use crate::model::Usage;

fn u64_at(v: &Value, pointer: &str) -> Option<u64> {
    v.pointer(pointer).and_then(Value::as_u64)
}

/// Chat Completions（OpenAI 互換）の `usage`。
///
/// 読み出しは `prompt_tokens_details.cached_tokens`、無ければ DeepSeek の
/// `prompt_cache_hit_tokens`・Moonshot の `cached_tokens`（`usage` 直下）。どれも
/// `prompt_tokens` の内数。書き込みは OpenRouter の `prompt_tokens_details.cache_write_tokens`。
pub(crate) fn chat_completions(u: &Value) -> Usage {
    Usage {
        prompt_tokens: u64_at(u, "/prompt_tokens").unwrap_or(0),
        completion_tokens: u64_at(u, "/completion_tokens").unwrap_or(0),
        cache_read_tokens: u64_at(u, "/prompt_tokens_details/cached_tokens")
            .or_else(|| u64_at(u, "/prompt_cache_hit_tokens"))
            .or_else(|| u64_at(u, "/cached_tokens"))
            .unwrap_or(0),
        cache_write_tokens: u64_at(u, "/prompt_tokens_details/cache_write_tokens").unwrap_or(0),
    }
}

/// Responses API の `response.usage`（`input_tokens` はキャッシュ分を含む）。
pub(crate) fn responses(u: &Value) -> Usage {
    Usage {
        prompt_tokens: u64_at(u, "/input_tokens").unwrap_or(0),
        completion_tokens: u64_at(u, "/output_tokens").unwrap_or(0),
        cache_read_tokens: u64_at(u, "/input_tokens_details/cached_tokens").unwrap_or(0),
        cache_write_tokens: u64_at(u, "/input_tokens_details/cache_write_tokens").unwrap_or(0),
    }
}

/// Anthropic Messages の usage をストリームから積む。
///
/// `message_start` の `message.usage` に入力とキャッシュ、`message_delta` の `usage` に出力
/// （と、送られてくれば入力・キャッシュの累積値）が載る。どれも累積値で減らないので、
/// フィールドごとに大きい方を採る（互換プロキシが `message_delta` に `input_tokens: 0` を
/// 載せても、`message_start` の値を消さない）。
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct AnthropicUsage {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
}

impl AnthropicUsage {
    /// `usage` オブジェクト 1 つを取り込む（無いフィールド・小さい値は前の値のまま）。
    pub(crate) fn absorb(&mut self, u: &Value) {
        let fields: [(&str, &mut u64); 4] = [
            ("/input_tokens", &mut self.input),
            ("/output_tokens", &mut self.output),
            ("/cache_read_input_tokens", &mut self.cache_read),
            ("/cache_creation_input_tokens", &mut self.cache_write),
        ];
        for (pointer, slot) in fields {
            if let Some(n) = u64_at(u, pointer) {
                *slot = (*slot).max(n);
            }
        }
    }

    /// 中立形へ。`input_tokens` はキャッシュ分を含まないので足し合わせて総数にする。
    pub(crate) fn to_usage(self) -> Usage {
        Usage {
            prompt_tokens: self
                .input
                .saturating_add(self.cache_read)
                .saturating_add(self.cache_write),
            completion_tokens: self.output,
            cache_read_tokens: self.cache_read,
            cache_write_tokens: self.cache_write,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn usage(prompt: u64, completion: u64, read: u64, write: u64) -> Usage {
        Usage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            cache_read_tokens: read,
            cache_write_tokens: write,
        }
    }

    #[test]
    fn chat_completions_reads_openrouter_cache_breakdown() {
        // OpenRouter の実測（#581・2 回目の呼び出し）。
        let u = json!({
            "prompt_tokens": 2907,
            "completion_tokens": 12,
            "prompt_tokens_details": { "cached_tokens": 2893, "cache_write_tokens": 11 },
        });
        assert_eq!(chat_completions(&u), usage(2907, 12, 2893, 11));
    }

    #[test]
    fn chat_completions_reads_deepseek_and_moonshot_hits() {
        let moonshot = json!({ "prompt_tokens": 100, "completion_tokens": 1, "cached_tokens": 90 });
        assert_eq!(chat_completions(&moonshot), usage(100, 1, 90, 0));
        let deepseek = json!({
            "prompt_tokens": 100,
            "completion_tokens": 1,
            "prompt_cache_hit_tokens": 64,
            "prompt_cache_miss_tokens": 36,
        });
        assert_eq!(chat_completions(&deepseek), usage(100, 1, 64, 0));
    }

    #[test]
    fn chat_completions_without_details_has_no_cache() {
        let u = json!({ "prompt_tokens": 10, "completion_tokens": 5 });
        assert_eq!(chat_completions(&u), usage(10, 5, 0, 0));
        assert_eq!(chat_completions(&json!({})), Usage::default());
    }

    #[test]
    fn responses_reads_cached_input_tokens() {
        let u = json!({
            "input_tokens": 3000,
            "output_tokens": 40,
            "input_tokens_details": { "cached_tokens": 2048 },
            "output_tokens_details": { "reasoning_tokens": 30 },
        });
        assert_eq!(responses(&u), usage(3000, 40, 2048, 0));
        assert_eq!(responses(&json!({})), Usage::default());
    }

    #[test]
    fn anthropic_adds_cache_tokens_to_the_prompt_total() {
        let mut acc = AnthropicUsage::default();
        // message_start: input_tokens はキャッシュ分を含まない。
        acc.absorb(&json!({
            "input_tokens": 5,
            "cache_creation_input_tokens": 1000,
            "cache_read_input_tokens": 2000,
            "output_tokens": 1,
        }));
        // message_delta: 出力の累積値だけ来る。
        acc.absorb(&json!({ "output_tokens": 42 }));
        assert_eq!(acc.to_usage(), usage(3005, 42, 2000, 1000));
    }

    #[test]
    fn anthropic_delta_overrides_cumulative_fields() {
        let mut acc = AnthropicUsage::default();
        acc.absorb(&json!({ "input_tokens": 5, "output_tokens": 1 }));
        acc.absorb(&json!({
            "input_tokens": 7,
            "cache_read_input_tokens": 100,
            "output_tokens": 9,
        }));
        assert_eq!(acc.to_usage(), usage(107, 9, 100, 0));
    }

    #[test]
    fn anthropic_delta_zeros_do_not_erase_start_values() {
        let mut acc = AnthropicUsage::default();
        acc.absorb(&json!({ "input_tokens": 5, "cache_read_input_tokens": 100 }));
        acc.absorb(&json!({
            "input_tokens": 0,
            "cache_read_input_tokens": 0,
            "output_tokens": 3,
        }));
        assert_eq!(acc.to_usage(), usage(105, 3, 100, 0));
    }
}

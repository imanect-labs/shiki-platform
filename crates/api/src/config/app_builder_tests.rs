//! 作成系の道具を使う会話の予算（`chat.app_builder.*`・D-53 の HD-4・T03）の設定の読み込みのユニットテスト。

use figment::providers::{Format, Serialized, Toml};

use super::backends::AppBuilderChatConfig;
use super::*;

/// 未指定なら、ワーカーの既定（24 手・新規ぶん 60 万トークン・$2・1 応答 32768・effort medium）を使う。
#[test]
fn app_builder_defaults_follow_the_worker_config() {
    let chat = ChatConfig::default();
    assert_eq!(chat.app_builder, AppBuilderChatConfig::default());
    let resolved = chat.app_builder.resolve();
    assert_eq!(resolved, chat::WorkerConfig::default().app_builder);
    assert_eq!(resolved.max_steps, 24);
    assert_eq!(resolved.max_fresh_tokens, 600_000);
    assert_eq!(resolved.max_cost_usd_micros, 2_000_000);
    assert_eq!(resolved.max_response_tokens, 32_768);
    assert_eq!(resolved.effort, llm_gateway::Effort::Medium);
    // `[chat]` に app_builder が無い設定も読める。
    let partial: ChatConfig =
        serde_json::from_value(serde_json::json!({ "enabled": true })).unwrap();
    assert_eq!(partial.app_builder, AppBuilderChatConfig::default());
}

/// TOML の `[chat.app_builder]` で項目ごとに上書きでき、書かなかった項目は既定のまま。
#[test]
fn app_builder_items_override_one_by_one() {
    let chat: ChatConfig = figment::Figment::from(Toml::string(
        r#"
        [chat.app_builder]
        max_steps = 30
        budget_max_cost_usd_micros = 5000000
        effort = "high"
        "#,
    ))
    .extract_inner("chat")
    .expect("chat");
    let resolved = chat.app_builder.resolve();
    assert_eq!(resolved.max_steps, 30);
    assert_eq!(resolved.max_cost_usd_micros, 5_000_000);
    assert_eq!(resolved.effort, llm_gateway::Effort::High);
    let base = chat::WorkerConfig::default().app_builder;
    assert_eq!(resolved.max_fresh_tokens, base.max_fresh_tokens);
    assert_eq!(resolved.max_response_tokens, base.max_response_tokens);
}

/// 環境変数（`SHIKI__CHAT__APP_BUILDER__*` を `__` で割った形）と同じ入れ子の値でも読める。
#[test]
fn app_builder_reads_nested_values_like_env() {
    let chat: ChatConfig = figment::Figment::from(Serialized::defaults(serde_json::json!({
        "chat": { "app_builder": { "max_tokens": 16384, "budget_max_tokens": 900000 } }
    })))
    .extract_inner("chat")
    .expect("chat");
    let resolved = chat.app_builder.resolve();
    assert_eq!(resolved.max_response_tokens, 16_384);
    assert_eq!(resolved.max_fresh_tokens, 900_000);
}

/// 手数の 0 は 1 にする（0 手では何もできずに止まる）。書き間違えた項目は読み込みで断る。
#[test]
fn app_builder_rejects_typos_and_clamps_zero_steps() {
    let zero = AppBuilderChatConfig {
        max_steps: Some(0),
        ..Default::default()
    };
    assert_eq!(zero.resolve().max_steps, 1);
    let typo: Result<ChatConfig, _> = serde_json::from_value(serde_json::json!({
        "app_builder": { "max_step": 30 }
    }));
    assert!(typo.is_err(), "知らない項目は黙って捨てない");
    let bad_effort: Result<ChatConfig, _> = serde_json::from_value(serde_json::json!({
        "app_builder": { "effort": "max" }
    }));
    assert!(bad_effort.is_err());
}

//! 判定用のモデル（FEAT-1）の設定の読み込みのユニットテスト。

use figment::providers::{Format, Toml};

use super::*;

/// TOML の `modality`・`decision_via`・`[models.decision]` が gateway のカタログへ届き、省略は chat。
#[test]
fn decision_entries_reach_the_gateway_catalog() {
    let toml = r#"
        [[models]]
        id = "claude-haiku-5-5"
        provider = "anthropic"

        [[models]]
        id = "decide-haiku-vote"
        provider = "anthropic"
        modality = "decision"
        decision_via = "claude-haiku-5-5"
        processed_abroad = true
        [models.decision]
        samples = 5

        [[models]]
        id = "decide-stub"
        modality = "decision"
    "#;
    #[derive(serde::Deserialize)]
    struct Models {
        models: Vec<LlmModelEntry>,
    }
    let parsed: Models = figment::Figment::from(Toml::string(toml))
        .extract()
        .expect("toml");
    let llm = LlmConfig {
        models: parsed.models,
        ..LlmConfig::default()
    };
    let catalog = llm.model_catalog();
    let chat = catalog.get("claude-haiku-5-5").unwrap();
    assert_eq!(chat.modality, llm_gateway::Modality::Chat);
    let vote = catalog.get("decide-haiku-vote").unwrap();
    assert!(vote.is_decision());
    assert_eq!(vote.decision_via.as_deref(), Some("claude-haiku-5-5"));
    assert_eq!(
        vote.decision,
        Some(llm_gateway::DecisionTuning {
            samples: 5,
            effort: llm_gateway::Effort::Low,
        })
    );
    let stub = catalog.get("decide-stub").unwrap();
    assert!(stub.is_decision() && stub.decision_via.is_none() && stub.decision.is_none());
}

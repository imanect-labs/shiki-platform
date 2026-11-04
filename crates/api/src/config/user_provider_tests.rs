use super::LlmConfig;
use figment::providers::Format;

#[test]
fn user_provider_rules_parse_and_default_to_unrestricted() {
    let config: LlmConfig = figment::Figment::from(figment::providers::Toml::string(
        r#"
        backend = "stub"
        [[user_provider_rules]]
        tenant_id = "a-corp"
        user_sub = "subject"
        providers = ["openai-direct", "anthropic"]
    "#,
    ))
    .extract()
    .unwrap();
    assert_eq!(config.user_provider_rules.len(), 1);
    assert_eq!(config.user_provider_rules[0].tenant_id, "a-corp");
    assert_eq!(config.user_provider_rules[0].user_sub, "subject");
    assert_eq!(
        config.user_provider_rules[0].providers,
        ["openai-direct", "anthropic"]
    );
    assert!(LlmConfig::default().user_provider_rules.is_empty());
}

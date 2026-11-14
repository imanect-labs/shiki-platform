//! prompt caching の opt-in（T00）の設定の読み込みと、gateway の構築での検査のユニットテスト。

use figment::providers::{Format, Toml};
use llm_gateway::{GatewayConfig, LlmError, LlmGateway, ProviderConfig, ProviderKind};

use super::*;

/// 評価の版のサーバーの設定の見本（Haiku 5.5 を opt-in している）。
const EVAL_EXAMPLE: &str =
    include_str!("../../../../eval/app-builder/config/llm-eval.example.toml");

fn eval_llm() -> LlmConfig {
    figment::Figment::from(Toml::string(EVAL_EXAMPLE))
        .extract_inner("llm")
        .expect("llm-eval.example.toml")
}

/// `[[llm.providers]]` を gateway のプロバイダへ写した設定（既定のプロバイダは stub）。接続はしない。
fn gateway_config(llm: &LlmConfig) -> GatewayConfig {
    let extra_providers = llm
        .providers
        .iter()
        .map(|p| ProviderConfig {
            id: p.id.clone(),
            kind: match p.backend {
                LlmBackend::Anthropic => ProviderKind::Anthropic,
                LlmBackend::Stub => ProviderKind::Stub,
                _ => ProviderKind::Openai,
            },
            base_url: p.base_url.clone(),
            api_key: p.api_key.clone(),
            ..Default::default()
        })
        .collect();
    GatewayConfig {
        extra_providers,
        catalog: llm.model_catalog(),
        ..Default::default()
    }
}

fn build(llm: &LlmConfig) -> Result<LlmGateway, LlmError> {
    let db = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://unused:unused@127.0.0.1:1/none")
        .unwrap();
    LlmGateway::build(db, reqwest::Client::new(), gateway_config(llm))
}

fn config_error(res: Result<LlmGateway, LlmError>) -> String {
    match res {
        Err(LlmError::Config(m)) => m,
        Err(e) => panic!("Config でない誤り: {e}"),
        Ok(_) => panic!("構築が通った"),
    }
}

fn haiku(llm: &mut LlmConfig) -> &mut LlmModelEntry {
    llm.models
        .iter_mut()
        .find(|m| m.id == "claude-haiku-5-5")
        .expect("haiku")
}

/// TOML の `prompt_cache` が gateway のカタログへ届き、省略は false。見本の設定はそのまま構築できる。
// connect_lazy は tokio ランタイムを要する。
#[tokio::test]
async fn eval_example_opts_haiku_in_and_builds() {
    let llm = eval_llm();
    let catalog = llm.model_catalog();
    assert!(catalog.get("claude-haiku-5-5").unwrap().prompt_cache);
    assert!(!catalog.get("default").unwrap().prompt_cache);
    assert_eq!(
        catalog.prompt_cache_models("anthropic").collect::<Vec<_>>(),
        ["claude-haiku-5-5"]
    );
    build(&llm).unwrap();
}

/// 単価を省いた opt-in は、書き込みを入力の単価で数えて少なく記録するので、構築で止める。
#[tokio::test]
async fn opt_in_without_both_cache_prices_fails_to_build() {
    for missing_read in [true, false] {
        let mut llm = eval_llm();
        let m = haiku(&mut llm);
        if missing_read {
            m.cache_read_price_micros_per_mtok = None;
        } else {
            m.cache_write_price_micros_per_mtok = None;
        }
        let err = config_error(build(&llm));
        assert!(
            err.contains("claude-haiku-5-5") && err.contains("cache_write_price_micros_per_mtok"),
            "{err}"
        );
    }
}

/// Anthropic でない振り先（既定の stub・OpenAI 互換）への opt-in は、黙って効かないので構築で止める。
#[tokio::test]
async fn opt_in_routed_to_a_non_anthropic_provider_fails_to_build() {
    let mut llm = eval_llm();
    haiku(&mut llm).provider = None;
    let err = config_error(build(&llm));
    assert!(err.contains("default is stub"), "{err}");

    let mut llm = eval_llm();
    llm.providers.push(LlmProviderEntry {
        id: "vllm".into(),
        display_name: None,
        backend: LlmBackend::Vllm,
        base_url: Some("http://vllm:8000/v1".into()),
        api_key: None,
        user_agent: None,
        session_header: None,
    });
    haiku(&mut llm).provider = Some("vllm".into());
    let err = config_error(build(&llm));
    assert!(err.contains("vllm is openai"), "{err}");
}

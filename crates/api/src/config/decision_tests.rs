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

/// compose の `environment` の 1 行の値（`KEY: value` の value・前後の空白を除く）。
fn compose_env<'a>(compose: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{key}:");
    compose
        .lines()
        .find_map(|l| l.trim_start().strip_prefix(prefix.as_str()))
        .map(str::trim)
}

/// CI の compose（`deploy/compose/docker-compose.ci.yml`）が env で渡すモデルのカタログ（BUG-86）は、起動と同じ
/// `Env::prefixed(..).split("__")` で 3 件のモデルとして読め、gateway の構築（判定用のモデルの検査）を通る。
/// 既定のモデル（`docker-compose.yml` の `SHIKI__LLM__DEFAULT_MODEL` の既定）はカタログの chat モデルで、
/// `decide-llm-stub` の元（`decision_via`）もそれ。値は compose の行をそのまま読む（書き換えるとここで落ちる）。
// connect_lazy は tokio ランタイムを要する。
#[tokio::test]
async fn ci_compose_models_env_reaches_the_catalog_and_builds() {
    const CI: &str = include_str!("../../../../deploy/compose/docker-compose.ci.yml");
    const BASE: &str = include_str!("../../../../deploy/compose/docker-compose.yml");
    // YAML の一重引用符の中身（`''` の逃がしは使わない＝そのまま env の値になる）。
    let models = compose_env(CI, "SHIKI__LLM__MODELS")
        .and_then(|v| v.strip_prefix('\''))
        .and_then(|v| v.strip_suffix('\''))
        .filter(|v| !v.contains('\''))
        .expect("SHIKI__LLM__MODELS は一重引用符で囲み、中に一重引用符を書かない");
    let default_model = compose_env(BASE, "SHIKI__LLM__DEFAULT_MODEL")
        .and_then(|v| v.strip_prefix("${SHIKI__LLM__DEFAULT_MODEL:-"))
        .and_then(|v| v.strip_suffix('}'))
        .expect("SHIKI__LLM__DEFAULT_MODEL の既定");
    assert!(
        compose_env(CI, "SHIKI__LLM__DEFAULT_MODEL").is_none(),
        "CI の compose で既定のモデルを上書きするなら、このテストも合わせる"
    );
    // 環境変数はプロセス全体で共有されるので、このプロセスでは書き換えない（並列のテストが読むと
    // 競合する）。env を付けた子プロセスでこのテストだけを流し直し、子が下の検証を行う。
    if std::env::var_os("SHIKITESTBUG86_CHILD").is_none() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "config::decision_tests::ci_compose_models_env_reaches_the_catalog_and_builds",
                "--test-threads=1",
            ])
            .env("SHIKITESTBUG86_CHILD", "1")
            .env("SHIKITESTBUG86__LLM__BACKEND", "stub")
            .env("SHIKITESTBUG86__LLM__DEFAULT_MODEL", default_model)
            .env("SHIKITESTBUG86__LLM__MODELS", models)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        // 名前の絞り込みが外れて 0 件で成功する取り違えを防ぐため、1 件流れたことも確かめる。
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "子プロセスでの検証が失敗した: {stdout}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    let cfg: AppConfig = Figment::new()
        .merge(Serialized::defaults(super::tests::valid_config()))
        .merge(Env::prefixed("SHIKITESTBUG86__").split("__"))
        .extract()
        .expect("env の [{...}] を表の配列として読む");
    cfg.validate().unwrap();
    assert_eq!(cfg.llm.backend, LlmBackend::Stub);
    let catalog = cfg.llm.model_catalog();
    let ids: Vec<&str> = catalog.models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["shiki-default", "decide-stub", "decide-llm-stub"]);
    assert_eq!(catalog.default_model, default_model);
    assert!(catalog.get(default_model).unwrap().is_chat());
    let stub = catalog.get("decide-stub").unwrap();
    assert!(stub.is_decision() && stub.decision_via.is_none());
    let llm_stub = catalog.get("decide-llm-stub").unwrap();
    assert!(llm_stub.is_decision());
    assert_eq!(llm_stub.decision_via.as_deref(), Some(default_model));
    // provider を書かないモデルは既定のプロバイダ（compose の SHIKI__LLM__BACKEND=stub）へ振る。
    assert!(catalog
        .models
        .iter()
        .all(|m| m.provider_id() == llm_gateway::DEFAULT_PROVIDER_ID));
    // 既定のプロバイダが stub の gateway を組む（判定用のモデルの検査を通る。接続はしない）。
    let db = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://unused:unused@127.0.0.1:1/none")
        .unwrap();
    llm_gateway::LlmGateway::build(
        db,
        reqwest::Client::new(),
        llm_gateway::GatewayConfig {
            catalog,
            ..Default::default()
        },
    )
    .expect("CI のカタログで gateway を組める");
}

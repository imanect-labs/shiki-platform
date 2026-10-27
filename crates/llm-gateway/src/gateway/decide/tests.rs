//! 判定のチョークポイントの DB に届く前の検査と、構築時の検査（DB の要る経路は tests/decision_it.rs）。

use super::*;
use crate::config::{AutoConfig, DecisionTuning, Modality, ModelCatalog, ProviderConfig};
use crate::decision::{decide_error_kind, ChoiceOption, DecideErrorKind, Question, QuestionKind};
use crate::gateway::UsageSource;
use crate::model::{GenerateRequest, Message, Role};

fn lazy_db() -> sqlx::PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(500))
        .connect_lazy("postgres://unused:unused@127.0.0.1:1/none")
        .unwrap()
}

fn chat(id: &str, provider: Option<&str>) -> ModelEntry {
    ModelEntry {
        id: id.into(),
        provider: provider.map(str::to_string),
        ..Default::default()
    }
}

fn decision(id: &str, via: Option<&str>, provider: Option<&str>) -> ModelEntry {
    ModelEntry {
        id: id.into(),
        modality: Modality::Decision,
        decision_via: via.map(str::to_string),
        provider: provider.map(str::to_string),
        ..Default::default()
    }
}

/// 既定（stub）＋ OpenAI 互換の `vllm`。chat は fast（stub）・local（vllm）。
fn config(models: Vec<ModelEntry>) -> GatewayConfig {
    let mut all = vec![chat("fast", None), chat("local", Some("vllm"))];
    all.extend(models);
    GatewayConfig {
        extra_providers: vec![ProviderConfig {
            id: "vllm".into(),
            kind: ProviderKind::Openai,
            base_url: Some("http://vllm.internal:8000/v1".into()),
            ..Default::default()
        }],
        catalog: ModelCatalog {
            default_model: "fast".into(),
            models: all,
        },
        auto: AutoConfig {
            model: "fast".into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn build(models: Vec<ModelEntry>) -> Result<LlmGateway, LlmError> {
    LlmGateway::build(lazy_db(), reqwest::Client::new(), config(models))
}

fn config_error(res: Result<LlmGateway, LlmError>) -> String {
    match res {
        Err(LlmError::Config(m)) => m,
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("構築できてしまった"),
    }
}

// connect_lazy は tokio ランタイムを要する。
#[tokio::test]
async fn decision_entries_build_llm_and_stub_deciders() {
    let gw = build(vec![
        decision("decide-local", Some("local"), Some("vllm")),
        ModelEntry {
            decision: Some(DecisionTuning {
                samples: 5,
                ..Default::default()
            }),
            ..decision("decide-vote", Some("local"), Some("vllm"))
        },
        decision("decide-stub", None, None),
    ])
    .unwrap();
    let names: Vec<(&str, &str)> = ["decide-local", "decide-vote", "decide-stub"]
        .into_iter()
        .map(|id| (id, gw.inner.deciders[id].provider.name()))
        .collect();
    assert_eq!(
        names,
        [
            ("decide-local", "llm_decider"),
            ("decide-vote", "llm_decider"),
            ("decide-stub", "stub")
        ]
    );
    // 決定器の版（決定キャッシュのキーに入る）は構築時に決まり、決定器が答える版と同じ。
    let versions: Vec<&str> = ["decide-local", "decide-vote", "decide-stub"]
        .into_iter()
        .map(|id| gw.inner.deciders[id].version.as_str())
        .collect();
    assert_eq!(
        versions,
        [
            "local+llm_decider/v1/k1/none",
            "local+llm_decider/v1/k5/none",
            "stub-decider/v1"
        ]
    );
    assert!(!gw.inner.deciders.contains_key("fast"));
    // 出力と監査の `provider` は構築した決定器の名前（chat モデル・カタログ外は無い）。
    assert_eq!(gw.decider_name("decide-local"), Some("llm_decider"));
    assert_eq!(gw.decider_name("decide-stub"), Some("stub"));
    assert_eq!(gw.decider_name("fast"), None);
    assert_eq!(gw.decider_name("nope"), None);
    // 判定用のモデルに思考の深さは選ばせない（元のモデルへ送る深さはカタログで決まる）。
    let local = gw.catalog().get("decide-local").unwrap().clone();
    assert!(!gw.supports_effort(&local));
    let explicit = ModelEntry {
        supports_effort: Some(true),
        ..local
    };
    assert!(!gw.supports_effort(&explicit));
}

#[tokio::test]
async fn misconfigured_decision_entries_fail_to_build() {
    // 元のモデルがカタログに無い・判定用・provider が違う。
    assert!(
        config_error(build(vec![decision("d", Some("ghost"), Some("vllm"))]))
            .contains("not in the catalog")
    );
    assert!(config_error(build(vec![
        decision("d0", None, None),
        decision("d", Some("d0"), None)
    ]))
    .contains("must be a chat model"));
    assert!(
        config_error(build(vec![decision("d", Some("local"), None)]))
            .contains("provider must be the same")
    );
    // 国外処理の元のモデルに、国外処理と出さない判定用のモデル。
    let mut abroad = config(vec![decision("d", Some("local"), Some("vllm"))]);
    abroad.catalog.models[1].processed_abroad = true;
    assert!(
        config_error(LlmGateway::build(lazy_db(), reqwest::Client::new(), abroad))
            .contains("processed_abroad")
    );
    // 多数決の回数・LLM 以外のプロバイダで元のモデルが無い・chat モデルに判定用の項目。
    let three_or_more = ModelEntry {
        decision: Some(DecisionTuning {
            samples: 4,
            ..Default::default()
        }),
        ..decision("d", Some("local"), Some("vllm"))
    };
    assert!(config_error(build(vec![three_or_more])).contains("samples"));
    assert!(
        config_error(build(vec![decision("d", None, Some("vllm"))])).contains("needs decision_via")
    );
    let chat_with_via = ModelEntry {
        decision_via: Some("fast".into()),
        ..chat("c", None)
    };
    assert!(config_error(build(vec![chat_with_via])).contains("only for decision models"));
    // 既定と Auto の振り先は chat モデル。
    let mut default_decides = config(vec![decision("d", None, None)]);
    default_decides.catalog.default_model = "d".into();
    assert!(config_error(LlmGateway::build(
        lazy_db(),
        reqwest::Client::new(),
        default_decides
    ))
    .contains("default_model"));
    let mut auto_decides = config(vec![decision("d", None, None)]);
    auto_decides.auto.model = "d".into();
    assert!(config_error(LlmGateway::build(
        lazy_db(),
        reqwest::Client::new(),
        auto_decides
    ))
    .contains("auto.model"));
}

/// TypeSafe 互換（`typesafe`・判定専用）のプロバイダを足した構成。
fn typesafe_config(models: Vec<ModelEntry>, api_key: Option<&str>) -> GatewayConfig {
    let mut c = config(models);
    c.extra_providers.push(ProviderConfig {
        id: "typesafe".into(),
        kind: ProviderKind::TypesafeSystemone,
        api_key: api_key.map(str::to_string),
        ..Default::default()
    });
    c
}

/// 実験的なので既定は無効: 国外処理・組織の opt-in・版の固定を書いた判定用のモデルだけが組める。
fn jev(id: &str) -> ModelEntry {
    ModelEntry {
        real_id: Some("jev-1.13.0".into()),
        processed_abroad: true,
        requires_opt_in: true,
        ..decision(id, None, Some("typesafe"))
    }
}

#[tokio::test]
async fn typesafe_decision_entries_build_only_when_opted_in_and_pinned() {
    let gw = LlmGateway::build(
        lazy_db(),
        reqwest::Client::new(),
        typesafe_config(vec![jev("decide-jev")], Some("k")),
    )
    .unwrap();
    let slot = &gw.inner.deciders["decide-jev"];
    assert_eq!(slot.provider.name(), "typesafe");
    assert_eq!(slot.version, "jev-1.13.0");
    let build_with = |m: ModelEntry, key: Option<&str>| {
        config_error(LlmGateway::build(
            lazy_db(),
            reqwest::Client::new(),
            typesafe_config(vec![m], key),
        ))
    };
    for (m, needle) in [
        (
            ModelEntry {
                processed_abroad: false,
                ..jev("d")
            },
            "processed_abroad",
        ),
        (
            ModelEntry {
                requires_opt_in: false,
                ..jev("d")
            },
            "requires_opt_in",
        ),
        (
            ModelEntry {
                real_id: Some("jev-latest".into()),
                ..jev("d")
            },
            "pin a versioned real_id",
        ),
        (
            ModelEntry {
                decision: Some(DecisionTuning::default()),
                ..jev("d")
            },
            "answers natively",
        ),
    ] {
        assert!(build_with(m, Some("k")).contains(needle), "{needle}");
    }
    assert!(build_with(jev("d"), None).contains("api_key"));
    // 文章を生成するモデルは判定専用のプロバイダへ振れない。
    assert!(build_with(chat("c", Some("typesafe")), Some("k")).contains("decision models only"));
}

fn ctx() -> AuthContext {
    AuthContext::for_workflow("t".into(), "o".into(), "wf")
}

fn call() -> DecideCall {
    DecideCall {
        idempotency_key: "wf:t:r:decide".into(),
        trace_id: None,
        source: UsageSource::Workflow,
        app_id: None,
    }
}

fn request(model: &str) -> DecideRequest {
    DecideRequest {
        model: model.into(),
        state: serde_json::json!("材料"),
        questions: vec![Question {
            key: "category".into(),
            instructions: "種類は".into(),
            kind: QuestionKind::Choice {
                options: vec![
                    ChoiceOption {
                        key: "a".into(),
                        description: String::new(),
                    },
                    ChoiceOption {
                        key: "b".into(),
                        description: String::new(),
                    },
                ],
            },
        }],
    }
}

#[tokio::test]
async fn decide_rejects_unknown_chat_and_oversized_requests_before_the_database() {
    let gw = build(vec![decision("decide-stub", None, None)]).unwrap();
    let res = gw.decide(&ctx(), &request("ghost"), &call()).await;
    assert!(
        matches!(&res, Err(LlmError::Config(m)) if m.contains("unknown model")),
        "{res:?}"
    );
    let res = gw.decide(&ctx(), &request("fast"), &call()).await;
    assert!(
        matches!(&res, Err(LlmError::Config(m)) if m.contains("not a decision model")),
        "{res:?}"
    );
    let mut big = request("decide-stub");
    big.questions[0].kind = QuestionKind::Choice {
        options: vec![ChoiceOption {
            key: "only".into(),
            description: String::new(),
        }],
    };
    let err = gw.decide(&ctx(), &big, &call()).await.unwrap_err();
    assert_eq!(
        decide_error_kind(&err),
        Some(DecideErrorKind::InvalidRequest)
    );
    // 形の合う要求は組織の設定を引きに行く（DB が無いので Unavailable）。
    assert!(matches!(
        gw.decide(&ctx(), &request("decide-stub"), &call()).await,
        Err(LlmError::Unavailable(_))
    ));
}

#[tokio::test]
async fn stream_refuses_decision_models() {
    let gw = build(vec![decision("decide-stub", None, None)]).unwrap();
    let mut req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);
    req.model = Some("decide-stub".into());
    let res = gw.stream(&ctx(), req).await;
    assert!(
        matches!(&res, Err(LlmError::Config(m)) if m.contains("decision model")),
        "{:?}",
        res.err()
    );
}

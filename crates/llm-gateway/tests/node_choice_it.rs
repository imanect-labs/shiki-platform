//! ノード（ワークフロー・ミニアプリ）のモデル解決の結合テスト（BUG-12・実 Postgres）。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行（未設定はスキップ）。
//! - 省略は組織の既定（特定モデル・Auto の振り先）に従い、利用者でもワークフローの実行主体でも同じになる。
//! - 明示したモデルが組織で無効になったら既定へ戻さず拒否する（fail-closed）。選択を禁じた組織では
//!   既定と違うモデルを拒否する。
//! - 本人専用のモデル（ChatGPT プラン）は、連携した本人が呼んでも解決の候補から外れる
//!   （チャットの見え方 `settings_for` では本人に残る）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use authz::{AuthContext, Principal, PrincipalKind};
use llm_gateway::{
    AutoConfig, ChatGptAccount, ChatGptSettings, ChoiceError, Effort, GatewayConfig, LlmGateway,
    ModelCatalog, ModelEntry, ProviderConfig, ProviderKind, SettingsPatch, AUTO_MODEL,
    CHATGPT_PROVIDER_ID,
};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

async fn pool() -> Option<PgPool> {
    let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&db_url)
        .await
        .expect("DB 接続");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("マイグレーション適用");
    Some(pool)
}

/// 組織ごとに分けた tenant（テストどうしで組織の AI 設定を共有しない）。
fn tenant() -> String {
    format!("nc-{}", uuid::Uuid::new_v4().simple())
}

fn user(tenant: &str, id: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: id.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "org".into(),
        authz::TenantId::parse(tenant).unwrap(),
    )
}

/// schedule / event の run の実行主体（ワークフロー）。
fn workflow(tenant: &str) -> AuthContext {
    AuthContext::for_workflow(
        authz::TenantId::parse(tenant).unwrap(),
        "org".into(),
        "wf-1",
    )
}

fn entry(id: &str) -> ModelEntry {
    ModelEntry {
        id: id.into(),
        ..Default::default()
    }
}

/// stub に fast・smart・local の 3 モデル。Auto の振り先（デプロイ設定）は smart。
fn gateway(pool: PgPool) -> LlmGateway {
    LlmGateway::build(
        pool,
        reqwest::Client::new(),
        GatewayConfig {
            catalog: ModelCatalog {
                default_model: "fast".into(),
                models: vec![entry("fast"), entry("smart"), entry("local")],
            },
            auto: AutoConfig {
                model: "smart".into(),
                effort: Effort::Low,
            },
            ..Default::default()
        },
    )
    .expect("gateway")
}

async fn save(gw: &LlmGateway, admin: &AuthContext, patch: SettingsPatch) {
    gw.update_tenant_settings(admin, &patch)
        .await
        .unwrap()
        .unwrap();
}

async fn resolve(
    gw: &LlmGateway,
    ctx: &AuthContext,
    model: Option<&str>,
) -> Result<String, ChoiceError> {
    gw.resolve_node_choice(ctx, model, None)
        .await
        .unwrap()
        .map(|r| r.model)
}

#[tokio::test]
async fn omitted_models_follow_the_organization_default_for_every_principal() {
    let Some(pool) = pool().await else { return };
    let gw = gateway(pool);
    let t = tenant();
    let alice = user(&t, "alice");
    let flow = workflow(&t);

    // 未保存の組織は Auto（デプロイ設定の振り先 smart）。デプロイ既定（fast）ではない。
    for who in [&alice, &flow] {
        assert_eq!(resolve(&gw, who, None).await.unwrap(), "smart");
        assert_eq!(resolve(&gw, who, Some("")).await.unwrap(), "smart");
    }
    let r = gw
        .resolve_node_choice(&flow, None, None)
        .await
        .unwrap()
        .unwrap();
    assert!(r.auto);

    // 管理者が既定を local にすると、誰が呼んでも local。
    save(
        &gw,
        &alice,
        SettingsPatch {
            default_model: Some("local".into()),
            ..Default::default()
        },
    )
    .await;
    for who in [&alice, &flow] {
        assert_eq!(resolve(&gw, who, None).await.unwrap(), "local");
    }

    // Auto に戻して振り先を上書きすると、上書き先。
    save(
        &gw,
        &alice,
        SettingsPatch {
            default_model: Some(AUTO_MODEL.into()),
            auto_model: Some(Some("fast".into())),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(resolve(&gw, &flow, None).await.unwrap(), "fast");

    // 別の組織には効かない。
    let other = user(&tenant(), "alice");
    assert_eq!(resolve(&gw, &other, None).await.unwrap(), "smart");
}

#[tokio::test]
async fn explicit_models_fail_closed_once_disabled_or_locked() {
    let Some(pool) = pool().await else { return };
    let gw = gateway(pool);
    let t = tenant();
    let admin = user(&t, "admin");
    let flow = workflow(&t);

    assert_eq!(resolve(&gw, &flow, Some("local")).await.unwrap(), "local");
    // 管理者が local を無効にすると、組織の既定へ戻さずに拒否する。
    save(
        &gw,
        &admin,
        SettingsPatch {
            models: vec![("local".into(), false)],
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        resolve(&gw, &flow, Some("local")).await,
        Err(ChoiceError::ModelNotAllowed("local".into()))
    );
    assert_eq!(
        resolve(&gw, &flow, Some("ghost")).await,
        Err(ChoiceError::ModelNotAllowed("ghost".into()))
    );

    // 選択を禁じた組織（既定 fast）: 既定と違うモデルは拒否し、既定と同じ・省略は通す。
    save(
        &gw,
        &admin,
        SettingsPatch {
            default_model: Some("fast".into()),
            allow_user_choice: Some(false),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        resolve(&gw, &flow, Some("smart")).await,
        Err(ChoiceError::ChoiceDisabled)
    );
    assert_eq!(resolve(&gw, &flow, Some("fast")).await.unwrap(), "fast");
    assert_eq!(resolve(&gw, &flow, None).await.unwrap(), "fast");
}

/// 既定（stub の a）＋ ChatGPT プラン（chatgpt:gpt）。連携者は `owner`（このテストの tenant）。
fn gateway_with_plan(pool: PgPool, tenant: &str, dir: &std::path::Path) -> LlmGateway {
    let path = dir.join("chatgpt.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "version": 1,
            "client_id": "oaiapp_1",
            "owner": {"tenant_id": tenant, "org": "org", "principal_id": "owner"},
            "tokens": {
                "access_token": "at",
                "refresh_token": "rt",
                "expires_at": "2999-01-01T00:00:00Z"
            }
        })
        .to_string(),
    )
    .unwrap();
    let account = ChatGptAccount::open(reqwest::Client::new(), ChatGptSettings::new(path));
    LlmGateway::build(
        pool,
        reqwest::Client::new(),
        GatewayConfig {
            extra_providers: vec![ProviderConfig {
                id: CHATGPT_PROVIDER_ID.into(),
                kind: ProviderKind::ChatgptPlan,
                ..Default::default()
            }],
            catalog: ModelCatalog {
                default_model: "a".into(),
                models: vec![
                    entry("a"),
                    ModelEntry {
                        id: "chatgpt:gpt".into(),
                        real_id: Some("gpt".into()),
                        provider: Some(CHATGPT_PROVIDER_ID.into()),
                        ..Default::default()
                    },
                ],
            },
            auto: AutoConfig {
                model: "chatgpt:gpt".into(),
                effort: Effort::Low,
            },
            chatgpt: Some(Arc::new(account)),
            ..Default::default()
        },
    )
    .expect("gateway")
}

#[tokio::test]
async fn personal_models_are_left_out_even_for_their_owner() {
    let Some(pool) = pool().await else { return };
    let t = tenant();
    let dir = std::env::temp_dir().join(format!("shiki-nc-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let gw = gateway_with_plan(pool, &t, &dir);
    let owner = user(&t, "owner");

    // 管理者が既定を本人専用のモデルにした組織。
    save(
        &gw,
        &owner,
        SettingsPatch {
            default_model: Some("chatgpt:gpt".into()),
            ..Default::default()
        },
    )
    .await;
    // チャットの見え方では、連携者本人には本人専用のモデルが既定のまま残る。
    let chat = gw.settings_for(&owner).await.unwrap();
    assert_eq!(chat.default_model, "chatgpt:gpt");
    // ノードの解決では本人でも外れ、Auto（振り先の chatgpt:gpt も外れるので次の候補の a）になる。
    let node = gw.settings_without_personal(&owner).await.unwrap();
    assert_eq!(node.default_model, AUTO_MODEL);
    assert!(!node.is_enabled(gw.catalog(), "chatgpt:gpt"));
    assert_eq!(resolve(&gw, &owner, None).await.unwrap(), "a");
    // 明示しても使えない。
    assert_eq!(
        resolve(&gw, &owner, Some("chatgpt:gpt")).await,
        Err(ChoiceError::ModelNotAllowed("chatgpt:gpt".into()))
    );

    // 本人専用のモデルしか許可していなければ、使えるモデルは無い。
    save(
        &gw,
        &owner,
        SettingsPatch {
            models: vec![("a".into(), false)],
            ..Default::default()
        },
    )
    .await;
    assert_eq!(resolve(&gw, &owner, None).await, Err(ChoiceError::NoModel));
    std::fs::remove_dir_all(dir).unwrap();
}

/// 組織がモデルの選択を禁じているとき、チャット・ワークフロー・ミニアプリの AI のどの解決でも、既定と違うモデルを
/// 明示した呼び出しを断り、選べる組織では通す（規則は 1 か所・D-30・D-117・BUG-140）。禁じた組織で既定と同じモデルを
/// 明示したノードとミニアプリの AI は、呼び出し側の思考の深さを使わず組織の既定の思考強度で動く（D-75）。
#[tokio::test]
async fn explicit_models_are_refused_on_every_path_while_choice_is_locked() {
    let Some(pool) = pool().await else { return };
    // fast・smart は思考強度を受ける（禁じた組織で、呼び出し側の思考の深さが使われないことを見る）。
    let effort_entry = |id: &str| ModelEntry {
        id: id.into(),
        supports_effort: Some(true),
        ..Default::default()
    };
    let gw = LlmGateway::build(
        pool.clone(),
        reqwest::Client::new(),
        GatewayConfig {
            catalog: ModelCatalog {
                default_model: "fast".into(),
                models: vec![effort_entry("fast"), effort_entry("smart")],
            },
            ..Default::default()
        },
    )
    .expect("gateway");
    let t = tenant();
    let alice = user(&t, "alice");
    let flow = workflow(&t);
    let chat = |model: &str| llm_gateway::ModelChoice {
        model: Some(model.into()),
        effort: Some(Effort::Low),
    };

    for allow in [false, true] {
        save(
            &gw,
            &alice,
            SettingsPatch {
                default_model: Some("fast".into()),
                allow_user_choice: Some(allow),
                default_effort: Some(Effort::High),
                ..Default::default()
            },
        )
        .await;
        let chat_got = gw.resolve_choice(&alice, &chat("smart")).await.unwrap();
        // ミニアプリの AI（app-gateway）とワークフローの llm.invoke は同じ解決を使う。
        let app_got = gw
            .resolve_workflow_choice(&alice, Some("smart"), Some(Effort::Low))
            .await
            .unwrap();
        let flow_got = gw
            .resolve_workflow_choice(&flow, Some("smart"), None)
            .await
            .unwrap();
        if allow {
            assert_eq!(chat_got.unwrap().model, "smart");
            let app = app_got.unwrap();
            assert_eq!(
                (app.model.as_str(), app.effort),
                ("smart", Some(Effort::Low))
            );
            assert_eq!(flow_got.unwrap().model, "smart");
        } else {
            for got in [chat_got, app_got, flow_got] {
                assert_eq!(got, Err(ChoiceError::ChoiceDisabled));
            }
            assert_eq!(ChoiceError::ChoiceDisabled.code(), "model_choice_disabled");
            // 既定と同じモデルの明示は通り、呼び出し側の思考の深さ（Low）ではなく組織の既定（High）で動く。
            let same = gw
                .resolve_workflow_choice(&alice, Some("fast"), Some(Effort::Low))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                (same.model.as_str(), same.effort),
                ("fast", Some(Effort::High))
            );
            let chat_same = gw.resolve_choice(&alice, &chat("fast")).await.unwrap();
            assert_eq!(chat_same.unwrap().model, "fast");
        }
    }

    // 作った組織の設定を片付ける。
    for table in ["tenant_llm_model", "tenant_llm_settings"] {
        sqlx::query(&format!("DELETE FROM {table} WHERE tenant_id = $1"))
            .bind(&t)
            .execute(&pool)
            .await
            .unwrap();
    }
}

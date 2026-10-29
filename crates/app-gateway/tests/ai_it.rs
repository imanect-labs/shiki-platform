//! ミニアプリ内 AI（llm.invoke / agent.invoke）の結合テスト（Task 9.9 受け入れ条件）。
//!
//! 実 Postgres＋**実 LlmGateway（Stub プロバイダ・有償単価カタログ）**で検証する:
//! - scope なし 403 ／ モデル allowlist 403・未指定 400 ／ 日次予算超過 429
//! - llm.invoke の SSE ストリーミングと `llm_usage` への (app_id×user) 会計
//! - agent.invoke がインストール時ピンの宣言ツール・日次残額を port へ渡すこと
//! - モデルの省略は組織の既定で解決し、明示モデルは置き換えない（BUG-12）
//! - モデルを省略したときの思考強度は、組織が選択を禁じていればアプリの指定を使わない（D-75・BUG-116）

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use app_gateway::{build_gateway_router, AiPin, AppInstallationStore, NewAppInstallation};
use axum::http::StatusCode;
use common::{ctx, setup, state, token};
use http_body_util::BodyExt;
use llm_gateway::SettingsPatch;
use serde_json::json;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const AI_SCOPES: &str = "llm.invoke agent.invoke";

/// AI ピン付きインストール（B1 client）を作る。
async fn install(pool: &PgPool, tenant: &str, ai: AiPin) -> (Uuid, String) {
    let app_id = Uuid::new_v4();
    let client = format!("app-{}", Uuid::new_v4());
    AppInstallationStore::new(pool.clone())
        .upsert(
            &ctx(tenant),
            NewAppInstallation {
                app_id,
                app_name: "ai-app",
                installed_version: "1.0.0",
                granted_scopes: &AI_SCOPES
                    .split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<_>>(),
                client_id_b1: Some(&client),
                client_id_b2: None,
                ai,
                frontend_bundle: None,
                server_bundle: None,
                server_spec: None,
            },
        )
        .await
        .expect("install");
    (app_id, client)
}

/// SSE レスポンスの本文を最後まで読む（Stub プロバイダは有限ストリーム）。
async fn post_sse(
    app: &axum::Router,
    path: &str,
    bearer: &str,
    body: &serde_json::Value,
) -> (StatusCode, String) {
    let req = axum::http::Request::builder()
        .method("POST")
        .uri(path)
        .header("authorization", format!("Bearer {bearer}"))
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    if status != StatusCode::OK {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        return (status, String::from_utf8_lossy(&bytes).to_string());
    }
    let bytes = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        resp.into_body().collect(),
    )
    .await
    .expect("SSE 本文がタイムアウト内に完了")
    .unwrap()
    .to_bytes();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

#[tokio::test]
async fn llm_invoke_streams_and_accounts_per_app_user() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let (app_id, client) = install(&pool, &tenant, AiPin::default()).await;
    let app = build_gateway_router(state(pool.clone(), &tenant));

    // scope なし → 403（granted にあってもトークンに無い）。
    let narrow = token(&client, "openid", &tenant);
    let (s, _) = post_sse(&app, "/gw/ai/llm/invoke", &narrow, &json!({"messages": []})).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    let tok = token(&client, AI_SCOPES, &tenant);

    // messages 空 → 400。
    let (s, _) = post_sse(&app, "/gw/ai/llm/invoke", &tok, &json!({ "messages": [] })).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // 正常系: Stub プロバイダが user 本文を語単位でストリームして返す。
    let body = json!({
        "messages": [
            { "role": "user", "content": [ { "type": "text", "text": "こんにちは 世界" } ] }
        ]
    });
    let (s, sse) = post_sse(&app, "/gw/ai/llm/invoke", &tok, &body).await;
    assert_eq!(s, StatusCode::OK, "{sse}");
    assert!(sse.contains("event: text"), "{sse}");
    assert!(sse.contains("こんにちは"), "{sse}");
    assert!(sse.contains("event: done"), "{sse}");

    // 会計: driver task が Done 後に記録する（非同期・最大 5 秒待つ）。
    let mut found = None;
    for _ in 0..50 {
        let row: Option<(String, i64)> = sqlx::query_as(
            "SELECT user_sub, prompt_tokens FROM llm_usage \
             WHERE tenant_id = $1 AND app_id = $2",
        )
        .bind(&tenant)
        .bind(app_id)
        .fetch_optional(&pool)
        .await
        .unwrap();
        if row.is_some() {
            found = row;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let (user_sub, prompt_tokens) = found.expect("llm_usage に app_id 付きで計上される");
    assert_eq!(user_sub, "alice");
    assert!(prompt_tokens > 0);
}

#[tokio::test]
async fn model_allowlist_and_budget_are_enforced() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());

    // allowlist: 許可外モデル 403・未指定 400（fail-closed）。
    let (_a, client) = install(
        &pool,
        &tenant,
        AiPin {
            budget_models: vec!["allowed-model".into()],
            ..AiPin::default()
        },
    )
    .await;
    let app = build_gateway_router(state(pool.clone(), &tenant));
    let tok = token(&client, AI_SCOPES, &tenant);
    let msg = json!([{ "role": "user", "content": [ { "type": "text", "text": "hi" } ] }]);

    let (s, _) = post_sse(
        &app,
        "/gw/ai/llm/invoke",
        &tok,
        &json!({ "model": "stub-m", "messages": msg }),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = post_sse(&app, "/gw/ai/llm/invoke", &tok, &json!({ "messages": msg })).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // agent.invoke にも同じ allowlist が効く。
    let (s, _) = post_sse(
        &app,
        "/gw/ai/agent/invoke",
        &tok,
        &json!({ "prompt": "調査して", "model": "stub-m" }),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // 日次予算 0 → 429（llm/agent 両方）。
    let (_b, client2) = install(
        &pool,
        &tenant,
        AiPin {
            budget_daily_usd_micros: Some(0),
            ..AiPin::default()
        },
    )
    .await;
    let tok2 = token(&client2, AI_SCOPES, &tenant);
    let (s, body) = post_sse(
        &app,
        "/gw/ai/llm/invoke",
        &tok2,
        &json!({ "messages": msg }),
    )
    .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{body}");
    let (s, _) = post_sse(
        &app,
        "/gw/ai/agent/invoke",
        &tok2,
        &json!({ "prompt": "調査して" }),
    )
    .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn agent_invoke_passes_pinned_tools_and_remaining_budget() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let (_app_id, client) = install(
        &pool,
        &tenant,
        AiPin {
            agent_tools: vec!["doc_search".into()],
            budget_daily_usd_micros: Some(1_000),
            ..AiPin::default()
        },
    )
    .await;
    let app = build_gateway_router(state(pool.clone(), &tenant));
    let tok = token(&client, AI_SCOPES, &tenant);

    // prompt 空 → 400。
    let (s, _) = post_sse(&app, "/gw/ai/agent/invoke", &tok, &json!({ "prompt": " " })).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    let (s, sse) = post_sse(
        &app,
        "/gw/ai/agent/invoke",
        &tok,
        &json!({ "prompt": "経費規程を調べて" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{sse}");
    // スタブ port が spec を echo する: 宣言ツールと日次残額（=1000）が渡っている。
    assert!(sse.contains("doc_search"), "{sse}");
    assert!(sse.contains("\"max_cost_usd_micros\":1000"), "{sse}");
    assert!(sse.contains("event: done"), "{sse}");

    // ツールの提示条件に使うスコープは granted ∩ トークン（granted に llm.invoke があっても、
    // トークンに無ければ渡さない・rag.query は granted に無いので渡らない）。
    let tok = token(&client, "agent.invoke", &tenant);
    let (s, sse) = post_sse(
        &app,
        "/gw/ai/agent/invoke",
        &tok,
        &json!({ "prompt": "経費規程を調べて" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{sse}");
    assert!(sse.contains("\"scopes\":[\"agent.invoke\"]"), "{sse}");
}

/// 組織の AI 設定を変える（管理画面の保存と同じ経路・state の gateway を使う）。
async fn set_llm_settings(st: &app_gateway::GatewayState, tenant: &str, patch: SettingsPatch) {
    st.caps
        .llm
        .as_ref()
        .unwrap()
        .update_tenant_settings(&ctx(tenant), &patch)
        .await
        .unwrap()
        .unwrap();
}

/// このアプリの会計の行のモデル名（driver task が Done 後に記録するので、`want` 件まで最大 5 秒待つ）。
async fn usage_models(pool: &PgPool, tenant: &str, app_id: Uuid, want: usize) -> Vec<String> {
    let mut models = Vec::new();
    for _ in 0..50 {
        models = sqlx::query_scalar(
            "SELECT model FROM llm_usage WHERE tenant_id = $1 AND app_id = $2 ORDER BY id",
        )
        .bind(tenant)
        .bind(app_id)
        .fetch_all(pool)
        .await
        .unwrap();
        if models.len() >= want {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    models
}

/// モデルを省略した llm.invoke は、デプロイ既定（stub-m）ではなく組織の既定で生成し、会計にも同じ名前が残る。
/// 明示したモデルはそのまま使う（組織の既定に置き換えない）。
#[tokio::test]
async fn llm_invoke_without_a_model_follows_the_organization_default() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let (app_id, client) = install(&pool, &tenant, AiPin::default()).await;
    let st = state(pool.clone(), &tenant);
    set_llm_settings(
        &st,
        &tenant,
        SettingsPatch {
            default_model: Some("stub-n".into()),
            ..Default::default()
        },
    )
    .await;
    let app = build_gateway_router(st);
    let tok = token(&client, AI_SCOPES, &tenant);
    let msg = json!([{ "role": "user", "content": [ { "type": "text", "text": "hi" } ] }]);

    for body in [
        json!({ "messages": msg }),
        json!({ "model": "", "messages": msg }),
    ] {
        let (s, sse) = post_sse(&app, "/gw/ai/llm/invoke", &tok, &body).await;
        assert_eq!(s, StatusCode::OK, "{sse}");
    }
    assert_eq!(
        usage_models(&pool, &tenant, app_id, 2).await,
        ["stub-n", "stub-n"]
    );

    let (s, sse) = post_sse(
        &app,
        "/gw/ai/llm/invoke",
        &tok,
        &json!({ "model": "stub-m", "messages": msg }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{sse}");
    assert_eq!(
        usage_models(&pool, &tenant, app_id, 3).await,
        ["stub-n", "stub-n", "stub-m"]
    );
}

/// インストール時に許可したモデルを明示し、組織がそのモデルを無効にすると 400 になる。組織の既定（許可モデルの
/// 外）へ振り替えて生成することも、会計の行が残ることもない（fail-closed）。
#[tokio::test]
async fn an_explicit_model_disabled_by_the_organization_is_not_replaced() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let (app_id, client) = install(
        &pool,
        &tenant,
        AiPin {
            budget_models: vec!["stub-m".into()],
            ..AiPin::default()
        },
    )
    .await;
    let st = state(pool.clone(), &tenant);
    set_llm_settings(
        &st,
        &tenant,
        SettingsPatch {
            default_model: Some("stub-n".into()),
            models: vec![("stub-m".into(), false)],
            ..Default::default()
        },
    )
    .await;
    let app = build_gateway_router(st);
    let tok = token(&client, AI_SCOPES, &tenant);
    let msg = json!([{ "role": "user", "content": [ { "type": "text", "text": "hi" } ] }]);
    let (s, body) = post_sse(
        &app,
        "/gw/ai/llm/invoke",
        &tok,
        &json!({ "model": "stub-m", "messages": msg }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
    // 許可モデルがあるアプリは省略できない（組織の既定へ落とさない・従来どおり）。
    let (s, _) = post_sse(&app, "/gw/ai/llm/invoke", &tok, &json!({ "messages": msg })).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(usage_models(&pool, &tenant, app_id, 0).await.is_empty());
}

/// SSE の本文から、流れてきた文字（`text_delta`）をつないだもの（stub は語ごとに分けて流す）。
fn streamed_text(sse: &str) -> String {
    sse.lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter_map(|d| serde_json::from_str::<serde_json::Value>(d).ok())
        .filter(|v| v["type"] == "text_delta")
        .filter_map(|v| v["text"].as_str().map(str::to_string))
        .collect()
}

/// モデルを省略した llm.invoke の思考強度（D-75・BUG-116）: 組織がモデルの選択を禁じているときは、アプリが書いた
/// effort を使わず組織の既定の思考強度で動く。選べる組織では、アプリの effort を使う。
#[tokio::test]
async fn an_omitted_model_ignores_the_apps_effort_when_the_organization_disallows_choice() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let (app_id, client) = install(&pool, &tenant, AiPin::default()).await;
    let st = state(pool.clone(), &tenant);
    let tok = token(&client, AI_SCOPES, &tenant);
    let body = json!({
        "messages": [{ "role": "user", "content": [ { "type": "text", "text": "echoeffort:" } ] }],
        "effort": "high",
    });

    for (allow, want) in [(false, "effort: low"), (true, "effort: high")] {
        set_llm_settings(
            &st,
            &tenant,
            SettingsPatch {
                default_model: Some("stub-n".into()),
                allow_user_choice: Some(allow),
                default_effort: Some(llm_gateway::Effort::Low),
                ..Default::default()
            },
        )
        .await;
        let app = build_gateway_router(st.clone());
        let (s, sse) = post_sse(&app, "/gw/ai/llm/invoke", &tok, &body).await;
        assert_eq!(s, StatusCode::OK, "{sse}");
        assert_eq!(streamed_text(&sse), want, "選択を許す={allow}: {sse}");
    }
    // 作った行を片付ける（会計は Done の後に非同期で書くので、2 件そろうのを待ってから消す）。
    assert_eq!(usage_models(&pool, &tenant, app_id, 2).await.len(), 2);
    for table in [
        "llm_usage",
        "app_capability_usage",
        "app_installation",
        "tenant_llm_model",
        "tenant_llm_settings",
    ] {
        sqlx::query(&format!("DELETE FROM {table} WHERE tenant_id = $1"))
            .bind(&tenant)
            .execute(&pool)
            .await
            .unwrap();
    }
}

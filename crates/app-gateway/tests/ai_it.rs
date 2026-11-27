//! ミニアプリ内 AI（llm.invoke / agent.invoke）の結合テスト（Task 9.9 受け入れ条件）。
//!
//! 実 Postgres＋**実 LlmGateway（Stub プロバイダ・有償単価カタログ）**で検証する:
//! - scope なし 403 ／ モデル allowlist 403・未指定 400 ／ 日次予算超過 429
//! - llm.invoke の SSE ストリーミングと `llm_usage` への (app_id×user) 会計
//! - agent.invoke がインストール時ピンの宣言ツール・日次残額を port へ渡すこと
//! - モデルの省略は組織の既定で解決し、明示モデルは置き換えない（BUG-12）
//! - モデルを省略したときの思考強度は、組織が選択を禁じていればアプリの指定を使わない（D-75・BUG-116）
//! - 組織が選択を禁じていれば、既定と違うモデルを明示した呼び出しを分かる誤り（code）で断る（D-117・BUG-140）

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
    // 送り直しの待ち（合わせて 7.5 秒）を挟む応答もあるので、余裕を取る。
    let bytes = tokio::time::timeout(
        std::time::Duration::from_secs(30),
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
    // 理由はコードで分かる（ワークフローの step の誤りと同じ語彙・D-117）。
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["code"], "model_unavailable", "{body}");
    assert_eq!(
        v["error"],
        "AI のモデル「stub-m」はこの組織では使えません。組織で使えるモデルを指定するか、モデルを指定しないで呼び出してください。"
    );
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

/// 組織がモデルの選択を禁じているとき（D-117・BUG-140）: 既定と違うモデルを明示した llm.invoke・agent.invoke は、
/// 生成せず、会計の行も残さずに 400 と `model_choice_disabled` で断る（黙って既定へ置き換えない）。既定と同じモデルの
/// 明示は通り、アプリが書いた effort を使わず組織の既定の思考強度で動く（D-75）。選べる組織では明示したモデルと
/// アプリの effort をそのまま使う。
#[tokio::test]
async fn explicit_models_are_refused_while_the_organization_disallows_choice() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let (app_id, client) = install(&pool, &tenant, AiPin::default()).await;
    let st = state(pool.clone(), &tenant);
    let tok = token(&client, AI_SCOPES, &tenant);
    let ask = |model: &str| {
        json!({
            "model": model,
            "messages": [{ "role": "user", "content": [ { "type": "text", "text": "echoeffort:" } ] }],
            "effort": "high",
        })
    };
    let lock = |allow: bool, default_model: &str| SettingsPatch {
        default_model: Some(default_model.into()),
        allow_user_choice: Some(allow),
        default_effort: Some(llm_gateway::Effort::Low),
        ..Default::default()
    };

    // 禁じた組織（既定 stub-m）: stub-n の明示は断る。agent.invoke も同じ。
    set_llm_settings(&st, &tenant, lock(false, "stub-m")).await;
    let app = build_gateway_router(st.clone());
    let (s, body) = post_sse(&app, "/gw/ai/llm/invoke", &tok, &ask("stub-n")).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["code"], "model_choice_disabled", "{body}");
    assert_eq!(v["status"], 400, "{body}");
    assert_eq!(
        v["error"],
        "この組織では AI のモデルを選べないため、「stub-n」は使えません。モデルを指定しないで呼び出すと、組織の既定のモデルで動きます。"
    );
    let (s, body) = post_sse(
        &app,
        "/gw/ai/agent/invoke",
        &tok,
        &json!({ "prompt": "調査して", "model": "stub-n" }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["code"], "model_choice_disabled", "{body}");
    // 断った呼び出しは生成しないので、会計の行は残らない。
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(usage_models(&pool, &tenant, app_id, 0).await.is_empty());

    // 禁じた組織（既定 stub-n）: 既定と同じモデルの明示は通り、アプリの high ではなく組織の既定の low で動く。
    set_llm_settings(&st, &tenant, lock(false, "stub-n")).await;
    let app = build_gateway_router(st.clone());
    let (s, sse) = post_sse(&app, "/gw/ai/llm/invoke", &tok, &ask("stub-n")).await;
    assert_eq!(s, StatusCode::OK, "{sse}");
    assert_eq!(streamed_text(&sse), "effort: low", "{sse}");
    let (s, sse) = post_sse(
        &app,
        "/gw/ai/agent/invoke",
        &tok,
        &json!({ "prompt": "調査して", "model": "stub-n" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{sse}");

    // 選べる組織（既定 stub-m）: 明示した stub-n とアプリの high をそのまま使う。
    set_llm_settings(&st, &tenant, lock(true, "stub-m")).await;
    let app = build_gateway_router(st.clone());
    let (s, sse) = post_sse(&app, "/gw/ai/llm/invoke", &tok, &ask("stub-n")).await;
    assert_eq!(s, StatusCode::OK, "{sse}");
    assert_eq!(streamed_text(&sse), "effort: high", "{sse}");
    assert_eq!(
        usage_models(&pool, &tenant, app_id, 2).await,
        ["stub-n", "stub-n"]
    );

    // 作った行を片付ける（会計は Done の後に非同期で書くので、2 件そろうのを待ってから消す）。
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

/// モデルの一覧（allowlist）を宣言したアプリと、モデルの選択を禁じた組織（D-117 の続き）: 省略は組織の既定で解決し、
/// その既定が一覧に入っていれば動く（一覧の照合は解決した後のモデルで行う）。既定が一覧の外なら、明示も省略も
/// `model_choice_disabled` で断り、どう呼んでも使えないことと直し方を言う（「省略すると動く」と案内しない）。選べる
/// 組織で既定が一覧の外なら、省略は断り（fail-closed）、一覧のモデルの明示は通る。
#[tokio::test]
async fn apps_with_a_model_list_follow_the_organization_default_when_it_is_listed() {
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
    let tok = token(&client, AI_SCOPES, &tenant);
    let msg = json!([{ "role": "user", "content": [ { "type": "text", "text": "hi" } ] }]);
    let lock = |allow: bool, default_model: &str| SettingsPatch {
        default_model: Some(default_model.into()),
        allow_user_choice: Some(allow),
        ..Default::default()
    };
    let refused = |s: StatusCode, body: &str| -> serde_json::Value {
        assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
        serde_json::from_str(body).unwrap()
    };

    // 禁じた組織で既定（stub-m）が一覧に入っている: 省略は動く（以前は「model を明示してください」で断っていた）。
    set_llm_settings(&st, &tenant, lock(false, "stub-m")).await;
    let app = build_gateway_router(st.clone());
    let (s, sse) = post_sse(&app, "/gw/ai/llm/invoke", &tok, &json!({ "messages": msg })).await;
    assert_eq!(s, StatusCode::OK, "{sse}");
    let (s, sse) = post_sse(
        &app,
        "/gw/ai/agent/invoke",
        &tok,
        &json!({ "prompt": "調査して" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{sse}");

    // 禁じた組織で既定（stub-n）が一覧の外: 一覧の stub-m の明示も、省略も、直し方を言って断る。
    set_llm_settings(&st, &tenant, lock(false, "stub-n")).await;
    let app = build_gateway_router(st.clone());
    let explicit = json!({ "model": "stub-m", "messages": msg });
    for body in [explicit, json!({ "messages": msg })] {
        let (s, resp) = post_sse(&app, "/gw/ai/llm/invoke", &tok, &body).await;
        let v = refused(s, &resp);
        assert_eq!(v["code"], "model_choice_disabled", "{resp}");
        let error = v["error"].as_str().unwrap();
        assert!(error.contains("AI を使えません"), "{error}");
        assert!(!error.contains("指定しないで呼び出すと"), "{error}");
    }
    let (s, resp) = post_sse(
        &app,
        "/gw/ai/agent/invoke",
        &tok,
        &json!({ "prompt": "調査して" }),
    )
    .await;
    assert_eq!(refused(s, &resp)["code"], "model_choice_disabled", "{resp}");

    // 選べる組織で既定（stub-n）が一覧の外: 省略は断り（既定へ落とさない）、一覧の stub-m の明示は通る。
    set_llm_settings(&st, &tenant, lock(true, "stub-n")).await;
    let app = build_gateway_router(st.clone());
    let (s, resp) = post_sse(&app, "/gw/ai/llm/invoke", &tok, &json!({ "messages": msg })).await;
    let v = refused(s, &resp);
    assert!(
        v["error"].as_str().unwrap().contains("「stub-n」"),
        "{resp}"
    );
    let (s, sse) = post_sse(
        &app,
        "/gw/ai/llm/invoke",
        &tok,
        &json!({ "model": "stub-m", "messages": msg }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{sse}");
    // 動いた 2 回の llm.invoke は、どちらも一覧の stub-m で生成した（既定の stub-n へ落ちていない）。
    assert_eq!(
        usage_models(&pool, &tenant, app_id, 2).await,
        ["stub-m", "stub-m"]
    );

    // 作った行を片付ける。
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

/// プロバイダ（OpenAI 互換）を試しのサーバーに向けた実 LlmGateway。`body` を `status` で返す。
async fn provider_llm(
    pool: PgPool,
    status: StatusCode,
    body: &'static str,
) -> std::sync::Arc<llm_gateway::LlmGateway> {
    use axum::response::IntoResponse;
    let app = axum::Router::new().fallback(move || async move {
        (status, [("content-type", "text/event-stream")], body).into_response()
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    llm_at(pool, base)
}

/// プロバイダ（OpenAI 互換）を `base` に向けた実 LlmGateway。
fn llm_at(pool: PgPool, base: String) -> std::sync::Arc<llm_gateway::LlmGateway> {
    use llm_gateway::{GatewayConfig, ModelCatalog, ModelEntry, ProviderConfig, ProviderKind};
    let cfg = GatewayConfig {
        provider: ProviderConfig {
            kind: ProviderKind::Openai,
            base_url: Some(base),
            api_key: Some("k".into()),
            ..Default::default()
        },
        catalog: ModelCatalog {
            default_model: "m1".into(),
            models: vec![ModelEntry {
                id: "m1".into(),
                ..Default::default()
            }],
        },
        langfuse: None,
        ..Default::default()
    };
    std::sync::Arc::new(
        llm_gateway::LlmGateway::build(pool, reqwest::Client::new(), cfg).expect("llm"),
    )
}

/// プロバイダの 5xx（529 を含む）の本文（要求の ID・組織の ID）は、AI の能力の応答（502）にも、流し始めた後の
/// SSE の error イベントにも載せない（誤りの種類の語だけ・BUG-210）。
#[tokio::test]
async fn provider_failures_do_not_reach_the_app_with_their_bodies() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let (_app_id, client) = install(&pool, &tenant, AiPin::default()).await;
    let tok = token(&client, AI_SCOPES, &tenant);
    let msg = json!({ "messages": [{ "role": "user", "content": [ { "type": "text", "text": "hi" } ] }] });
    const SECRETS: [&str; 3] = ["org-secret", "req_secret_5", "project-secret"];

    // 1. 529（接続の前の失敗・送り直しても 529）: 502 の本文に載せない。
    let mut st = state(pool.clone(), &tenant);
    st.caps.llm = Some(
        provider_llm(
            pool.clone(),
            StatusCode::from_u16(529).unwrap(),
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded for org-secret"},"request_id":"req_secret_5"}"#,
        )
        .await,
    );
    let app = build_gateway_router(st);
    let (s, body) = post_sse(&app, "/gw/ai/llm/invoke", &tok, &msg).await;
    assert_eq!(s, StatusCode::BAD_GATEWAY, "{body}");
    for leaked in SECRETS {
        assert!(!body.contains(leaked), "{leaked} が漏れた: {body}");
    }
    assert!(body.contains("overloaded_error"), "{body}");

    // 2. 流し始めた後に素の誤りの JSON で切れた（Gemini API で実測）: SSE の error イベントに本文を載せない。
    let mut st = state(pool.clone(), &tenant);
    st.caps.llm = Some(
        provider_llm(
            pool.clone(),
            StatusCode::OK,
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"途中\"}}]}\n\n",
                "[{\"error\":{\"code\":500,\"message\":\"Internal error for project-secret\",\"status\":\"INTERNAL\"}}]\n",
            ),
        )
        .await,
    );
    let app = build_gateway_router(st);
    let (s, sse) = post_sse(&app, "/gw/ai/llm/invoke", &tok, &msg).await;
    assert_eq!(s, StatusCode::OK, "{sse}");
    assert!(sse.contains("event: error"), "{sse}");
    for leaked in SECRETS {
        assert!(!sse.contains(leaked), "{leaked} が漏れた: {sse}");
    }
    assert!(sse.contains("INTERNAL"), "{sse}");

    for table in ["llm_usage", "app_capability_usage", "app_installation"] {
        sqlx::query(&format!("DELETE FROM {table} WHERE tenant_id = $1"))
            .bind(&tenant)
            .execute(&pool)
            .await
            .unwrap();
    }
}

/// 最初の 1 回だけ要求を受けて、流し始めてから終わりの合図の無いまま閉じ（gateway が送り直す）、その後は接続を
/// 受け付けない送り先。返すのは base_url（userinfo 付き）と、送り先のポート。
async fn cut_once_then_gone() -> (String, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let Ok((mut sock, _)) = listener.accept().await else {
            return;
        };
        // 以後の接続（送り直し）は断られる。
        drop(listener);
        // 要求をヘッダと content-length の本文まで読み切ってから答える。
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = sock.read(&mut chunk).await.unwrap_or(0);
            if n == 0 {
                return;
            }
            buf.extend_from_slice(&chunk[..n]);
            let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                continue;
            };
            let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
            let len: usize = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            if buf.len() >= end + 4 + len {
                break;
            }
        }
        let _ = sock
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
            )
            .await;
        let _ = sock.shutdown().await;
    });
    (
        format!("http://user:pw-secret@{addr}/llm-base"),
        addr.port().to_string(),
    )
}

/// プロバイダに接続できないときの誤り（接続の前の失敗の 502 の本文・送り直しに失敗した SSE の error イベント）に、
/// 送り先の URL（ホスト・ポート・パス・base_url の userinfo）を載せない（誤りの種類だけ・BUG-218）。
#[tokio::test]
async fn unreachable_providers_are_not_named_to_the_app() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let (_app_id, client) = install(&pool, &tenant, AiPin::default()).await;
    let tok = token(&client, AI_SCOPES, &tenant);
    let msg = json!({ "messages": [{ "role": "user", "content": [ { "type": "text", "text": "hi" } ] }] });
    let assert_hidden = |shown: &str, port: &str| {
        for leaked in ["127.0.0.1", port, "pw-secret", "/llm-base", "http://"] {
            assert!(!shown.contains(leaked), "{leaked} が漏れた: {shown}");
        }
        assert!(
            shown.contains("プロバイダに接続できませんでした"),
            "{shown}"
        );
    };

    // 1. 接続を断られる（送り直しても断られる）: 502 の本文に載せない。
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = closed.local_addr().unwrap();
    drop(closed);
    let mut st = state(pool.clone(), &tenant);
    st.caps.llm = Some(llm_at(
        pool.clone(),
        format!("http://user:pw-secret@{addr}/llm-base"),
    ));
    let app = build_gateway_router(st);
    let (s, body) = post_sse(&app, "/gw/ai/llm/invoke", &tok, &msg).await;
    assert_eq!(s, StatusCode::BAD_GATEWAY, "{body}");
    assert_hidden(&body, &addr.port().to_string());

    // 2. 流し始めてから切れ、送り直しで接続を断られる: SSE の error イベントに載せない。
    let (base, port) = cut_once_then_gone().await;
    let mut st = state(pool.clone(), &tenant);
    st.caps.llm = Some(llm_at(pool.clone(), base));
    let app = build_gateway_router(st);
    let (s, sse) = post_sse(&app, "/gw/ai/llm/invoke", &tok, &msg).await;
    assert_eq!(s, StatusCode::OK, "{sse}");
    assert!(sse.contains("event: error"), "{sse}");
    assert_hidden(&sse, &port);

    for table in ["llm_usage", "app_capability_usage", "app_installation"] {
        sqlx::query(&format!("DELETE FROM {table} WHERE tenant_id = $1"))
            .bind(&tenant)
            .execute(&pool)
            .await
            .unwrap();
    }
}

/// モデルが返したツールの引数の JSON が読めないとき、アプリへ `input: {}` の tool_use_stop を渡さない（BUG-225）。アプリは
/// 空の引数と壊れた引数を見分けられないので、壊れた呼び出しを渡さずに誤りの error イベントで終える。引数の無い呼び出し
/// （空の引数）は `{}` で渡す。
#[tokio::test]
async fn unreadable_tool_arguments_are_not_handed_to_the_app() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let (_app_id, client) = install(&pool, &tenant, AiPin::default()).await;
    let tok = token(&client, AI_SCOPES, &tenant);
    let msg = json!({
        "messages": [{ "role": "user", "content": [ { "type": "text", "text": "a1 を消して" } ] }],
        "tools": [{ "name": "delete_item", "description": "消す", "input_schema": { "type": "object" } }]
    });
    let call = |args: &str| {
        let line = json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"delete_item","arguments":args}}]}}]});
        let body = format!(
            "data: {line}\n\ndata: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}]}}\n\ndata: [DONE]\n\n"
        );
        &*Box::leak(body.into_boxed_str())
    };

    // 1. 途中までの引数: tool_use_start の後に error で終わり、tool_use_stop と done は来ない。
    let mut st = state(pool.clone(), &tenant);
    st.caps.llm = Some(provider_llm(pool.clone(), StatusCode::OK, call("{\"id\":\"a1\",")).await);
    let app = build_gateway_router(st);
    let (s, sse) = post_sse(&app, "/gw/ai/llm/invoke", &tok, &msg).await;
    assert_eq!(s, StatusCode::OK, "{sse}");
    assert!(sse.contains("event: tool_use_start"), "{sse}");
    assert!(!sse.contains("event: tool_use_stop"), "{sse}");
    assert!(!sse.contains("event: done"), "{sse}");
    assert!(sse.contains("event: error"), "{sse}");
    assert!(
        sse.contains("モデルがツールに渡す値を読める形で返しませんでした"),
        "{sse}"
    );

    // 2. 引数の無い呼び出し: `{}` の tool_use_stop を渡す。
    let mut st = state(pool.clone(), &tenant);
    st.caps.llm = Some(provider_llm(pool.clone(), StatusCode::OK, call("")).await);
    let app = build_gateway_router(st);
    let (s, sse) = post_sse(&app, "/gw/ai/llm/invoke", &tok, &msg).await;
    assert_eq!(s, StatusCode::OK, "{sse}");
    assert!(sse.contains("event: tool_use_stop"), "{sse}");
    assert!(sse.contains(r#""input":{}"#), "{sse}");
    assert!(sse.contains("event: done"), "{sse}");

    for table in ["llm_usage", "app_capability_usage", "app_installation"] {
        sqlx::query(&format!("DELETE FROM {table} WHERE tenant_id = $1"))
            .bind(&tenant)
            .execute(&pool)
            .await
            .unwrap();
    }
}

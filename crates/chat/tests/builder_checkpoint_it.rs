//! 作成系の道具を呼んだ通常の会話の、チェックポイントの保存と境界からの再開（T03・R1・D-204）の結合テスト。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）で、テストごとに専用の DB を作る（生成ジョブの待ち行列を、ほかの
//! テストのワーカーと共有しない）。試運転のテストは実 OpenFGA（`OPENFGA_TEST_URL`）も使う。
//!
//! 429 の代わりに、gateway の利用上限（`llm.limits` のユーザー単位）で `LlmError::RateLimited` を起こす。単価を
//! 付けた stub は 1 回の生成で上限を越えるので、run の 2 回目の生成が必ず断られ、ジョブは再試行の待ちに戻る
//! （Anthropic の 429 と同じ種別の失敗）。テストは会計の額を 0 に戻し、リースと待ちを短くして、同じ run を
//! 引き継がせる。
//!
//! - emit_workflow（作成系）を呼んだステップの終わりから保存が始まる。再開した run は境界から続き、作成を
//!   やり直さない。広げた予算（元の 1 手を越えて続ける）と 1 応答の上限も、再開の後まで保たれる。
//! - try_workflow（試運転）の次の生成で断られて再開しても、承認カードは 1 回だけで、試運転は二重に走らない。
//! - 作成系の道具を使わない通常の会話はチェックポイントを書かない。

#![allow(
    clippy::pedantic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::too_many_lines
)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use chat::worker::AppBuilderConfig;
use chat::{ChatStore, ChatWorker, TryWaitLimits, WorkerConfig, WorkerDeps};
use llm_gateway::{
    GatewayConfig, LlmGateway, ModelCatalog, ModelEntry, ProviderConfig, ProviderKind,
    UsageLimitsConfig,
};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use uuid::Uuid;
use workflow_engine::{Catalog, DelegationStore, RunStore, WorkflowRunLauncher, WorkflowStore};

/// 全許可の認可（作成の経路だけを見るテスト用）。
struct AllowAll;

#[async_trait]
impl AuthzClient for AllowAll {
    async fn check(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
        _: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn write_tuple(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn delete_tuple(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn read_tuples(
        &self,
        _: &FgaObject,
        _: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        Ok(vec![])
    }
    async fn list_objects(
        &self,
        _: &Subject,
        _: Relation,
        _: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
    async fn delete_object_tuples(&self, _: &FgaObject) -> Result<u32, AuthzError> {
        Ok(0)
    }
    async fn read_subject_objects(
        &self,
        _: &Subject,
        _: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
}

/// emit_workflow の保存と同じ形の材料（モデルは stub の "m" だけ）。
struct TestCatalog;

#[async_trait]
impl chat::WorkflowCatalogSource for TestCatalog {
    async fn catalog(&self, _ctx: &AuthContext) -> Result<Catalog, String> {
        Ok(Catalog {
            models: Some(vec!["m".into()]),
            ..Catalog::default()
        })
    }
}

/// テスト専用の DB（終わったら消す）。
struct TestDb {
    admin_url: String,
    name: String,
    pool: PgPool,
}

impl TestDb {
    async fn create(prefix: &str) -> Option<Self> {
        let Ok(admin_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
            eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
            return None;
        };
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .expect("Postgres へ接続できること");
        let name = format!("{prefix}_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE DATABASE {name}"))
            .execute(&admin)
            .await
            .expect("テスト用 DB を作れること");
        admin.close().await;
        let mut url = reqwest::Url::parse(&admin_url).expect("DB URL");
        url.set_path(&name);
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect(url.as_str())
            .await
            .expect("テスト用 DB へ接続できること");
        sqlx::migrate!("../../migrations")
            .run(&pool)
            .await
            .expect("マイグレーション適用");
        Some(TestDb {
            admin_url,
            name,
            pool,
        })
    }

    async fn drop_db(&self) {
        self.pool.close().await;
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.admin_url)
            .await
            .unwrap();
        let _ = sqlx::query(&format!(
            "DROP DATABASE IF EXISTS {} WITH (FORCE)",
            self.name
        ))
        .execute(&admin)
        .await;
    }
}

/// テストの本体を別のタスクで走らせ、途中の assert が落ちても DB を片付けてから落とす。
async fn run_in_db<F, Fut>(prefix: &str, body: F)
where
    F: FnOnce(PgPool) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let Some(db) = TestDb::create(prefix).await else {
        return;
    };
    let outcome = tokio::spawn(body(db.pool.clone())).await;
    db.drop_db().await;
    if let Err(e) = outcome {
        std::panic::resume_unwind(e.into_panic());
    }
}

fn user_ctx(tenant: &str, user: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: user.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        authz::TenantId::parse(tenant).unwrap(),
    )
}

/// 1 回の生成で利用上限（ユーザー単位 1 マイクロ USD）を越える stub。2 回目の生成から `RateLimited` で断られる。
fn limited_gateway(pool: PgPool) -> LlmGateway {
    let config = GatewayConfig {
        provider: ProviderConfig {
            kind: ProviderKind::Stub,
            ..Default::default()
        },
        catalog: ModelCatalog {
            default_model: "m".into(),
            models: vec![ModelEntry {
                id: "m".into(),
                // 1 トークン 1000 マイクロ USD（prompt は少なくとも数語あるので、1 回で上限を越える）。
                prompt_price_micros_per_mtok: 1_000_000_000,
                ..Default::default()
            }],
        },
        limits: UsageLimitsConfig {
            enabled: true,
            per_user_usd_micros: Some(1),
            providers: Vec::new(),
        },
        ..Default::default()
    };
    LlmGateway::build(pool, reqwest::Client::new(), config).expect("gateway")
}

/// 通常の会話の手数を `max_steps` にしたワーカーの設定。作成系の道具を呼んだら 4 手・1 応答 16384 まで広げる。
fn worker_config(max_steps: usize) -> WorkerConfig {
    WorkerConfig {
        system_prompt: "あなたはアシスタントです。".into(),
        model: Some("m".into()),
        // 失敗の後の引き継ぎを待たせない（ハートビートは 2 秒ごと）。
        lease_secs: 6,
        max_steps,
        // stub は提示した定義から道具を探す（遅延の定義は読み込まれるまで載らない）。
        tool_search: false,
        try_wait: TryWaitLimits {
            max_wait: Duration::from_millis(500),
            first_poll: Duration::from_millis(100),
            max_poll: Duration::from_millis(200),
        },
        app_builder: AppBuilderConfig {
            max_steps: 4,
            max_response_tokens: 16_384,
            ..AppBuilderConfig::default()
        },
        ..Default::default()
    }
}

fn deps(
    gateway: LlmGateway,
    workflows: Arc<WorkflowStore>,
    catalog: bool,
    artifacts: Option<Arc<artifact::ArtifactStore>>,
) -> WorkerDeps {
    WorkerDeps {
        gateway,
        search: None,
        sandbox: None,
        artifacts: None,
        web_search: None,
        parser: None,
        storage: None,
        workflow_resolver: None,
        skill_artifacts: artifacts,
        skill_catalog: None,
        workflow_store: Some(workflows),
        workflow_catalog: catalog
            .then(|| Arc::new(TestCatalog) as Arc<dyn chat::WorkflowCatalogSource>),
        collab: None,
        tabular: None,
        office: None,
        office_live: None,
        office_creator: None,
        office_engine: None,
    }
}

/// エージェントモードの会話を作る（自動タイトルは付けない＝会話の生成だけが gateway を呼ぶ）。
async fn conversation(pool: &PgPool, store: &ChatStore, who: &AuthContext) -> Uuid {
    let thread = store
        .create_thread(who, "作成", true, None, None)
        .await
        .unwrap();
    sqlx::query("UPDATE thread SET title_pending = false WHERE tenant_id = $1 AND id = $2")
        .bind(who.tenant_id())
        .bind(thread.id)
        .execute(pool)
        .await
        .unwrap();
    thread.id
}

/// 自律でない（通常の会話の）発話を送り、(run, 回答のメッセージ) を返す。
async fn post(store: &ChatStore, who: &AuthContext, thread: Uuid, text: &str) -> (Uuid, Uuid) {
    let res = store
        .post_message(who, thread, text, &[], None, Some(true), false, &[], None)
        .await
        .unwrap();
    (res.run_id, res.assistant_message_id)
}

/// 生成の run の (状態, fencing, 承認待ちのツール呼び出し id)。
async fn run_row(pool: &PgPool, tenant: &str, run_id: Uuid) -> (String, i64, Option<String>) {
    sqlx::query_as(
        "SELECT status, fencing_token, pending_approval->>'tool_call_id' FROM generation_run \
         WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(tenant)
    .bind(run_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// 保存済みのチェックポイント（封筒）と、その JSON の大きさ（バイト）。
async fn saved_checkpoint(pool: &PgPool, tenant: &str, run_id: Uuid) -> Option<(Value, i32)> {
    let row: (Option<Value>, Option<i32>) = sqlx::query_as(
        "SELECT checkpoint, octet_length(checkpoint::text) FROM generation_run \
         WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(tenant)
    .bind(run_id)
    .fetch_one(pool)
    .await
    .unwrap();
    row.0.zip(row.1)
}

/// 生成が断られてジョブが再試行の待ち（30 秒の backoff）に戻るまで待つ。処理中の可視期限（12 秒）とは
/// 長さで見分ける。
async fn wait_rate_limited(pool: &PgPool, tenant: &str, run_id: Uuid) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let backing_off: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM job_queue WHERE tenant_id = $1 AND payload->>'run_id' = $2 \
             AND attempts >= 1 AND visible_at > now() + interval '20 seconds')",
        )
        .bind(tenant)
        .bind(run_id.to_string())
        .fetch_one(pool)
        .await
        .unwrap();
        if backing_off {
            return;
        }
        let (status, ..) = run_row(pool, tenant, run_id).await;
        assert!(
            Instant::now() < deadline,
            "生成が断られて再試行の待ちに戻らない（{status}）"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// 会計の額を 0 に戻し（上限を外し）、リースを切らし、ジョブをすぐ見えるようにする（同じ run を引き継がせる）。
async fn let_it_retry(pool: &PgPool, tenant: &str, run_id: Uuid) {
    sqlx::query("UPDATE llm_usage SET cost_usd_micros = 0 WHERE tenant_id = $1")
        .bind(tenant)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE generation_run SET lease_until = now() - interval '1 second' \
         WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(tenant)
    .bind(run_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE job_queue SET visible_at = now() WHERE tenant_id = $1 AND payload->>'run_id' = $2",
    )
    .bind(tenant)
    .bind(run_id.to_string())
    .execute(pool)
    .await
    .unwrap();
}

/// 引き継いだ run が終わるまで待つ。承認待ちにもう一度入ったら落とす（済んだ呼び出しをやり直している）。
async fn wait_done_without_asking_again(pool: &PgPool, tenant: &str, run_id: Uuid) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (status, ..) = run_row(pool, tenant, run_id).await;
        assert_ne!(
            status, "waiting_approval",
            "再開した run が承認をもう一度求めた"
        );
        if status == "done" {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "再開した生成が終わらない（{status}）"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// run のイベントのうち `type` のものの payload（seq 順）。
async fn events(pool: &PgPool, tenant: &str, run_id: Uuid, kind: &str) -> Vec<Value> {
    sqlx::query_scalar(
        "SELECT payload FROM generation_event WHERE tenant_id = $1 AND run_id = $2 AND type = $3 \
         ORDER BY seq",
    )
    .bind(tenant)
    .bind(run_id)
    .bind(kind)
    .fetch_all(pool)
    .await
    .unwrap()
}

/// 回答のメッセージの (content, stop_reason)。
async fn answer(pool: &PgPool, tenant: &str, message_id: Uuid) -> (Value, Option<String>) {
    sqlx::query_as("SELECT content, stop_reason FROM message WHERE tenant_id = $1 AND id = $2")
        .bind(tenant)
        .bind(message_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// content のうち、名前が `tool` の tool_call ブロックの数。
fn tool_call_blocks(content: &Value, tool: &str) -> usize {
    content
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["type"] == "tool_call" && b["name"] == tool)
        .count()
}

/// チェックポイントの履歴に `tool` の呼び出し（tool_use）があるか。
fn checkpoint_calls(envelope: &Value, tool: &str) -> bool {
    envelope["checkpoint"]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["content"].as_array().cloned().unwrap_or_default())
        .any(|b| b["type"] == "tool_use" && b["name"] == tool)
}

/// emit_workflow を呼んだステップの終わりから保存が始まり、断られた run は境界から再開する。作成はやり直さず、
/// 広げた予算（元の 1 手を越えて続ける）と 1 応答の上限は再開の後まで保たれる（チェックポイントに欄は無い）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn emit_starts_checkpoints_and_the_resumed_run_keeps_the_extended_budget() {
    run_in_db("chat_builder_cp_emit", |pool| async move {
        let tenant = format!("t-{}", Uuid::new_v4());
        let alice = user_ctx(&tenant, "alice");
        let store = ChatStore::connect(pool.clone(), Arc::new(AllowAll), None)
            .await
            .unwrap();
        let artifacts = Arc::new(artifact::ArtifactStore::new(
            pool.clone(),
            Arc::new(AllowAll),
        ));
        let workflows = Arc::new(WorkflowStore::new(Arc::clone(&artifacts)));
        // 通常の会話は 1 手。作成系の道具を呼ばなければ、2 手目に入れず上限で止まる。
        let worker = ChatWorker::new(
            pool.clone(),
            store.clone(),
            deps(
                limited_gateway(pool.clone()),
                Arc::clone(&workflows),
                true,
                Some(Arc::clone(&artifacts)),
            ),
            worker_config(1),
        );
        let handles = worker.spawn(1);
        let thread = conversation(&pool, &store, &alice).await;
        let name = format!("cp-{}", Uuid::new_v4().simple());
        let (run_id, message_id) = post(&store, &alice, thread, &format!("emitwf:ok {name}")).await;

        // 1 手目で作成し、広げた予算で 2 手目へ進んで断られる。保存は 1 手目の終わりの境界。
        wait_rate_limited(&pool, &tenant, run_id).await;
        let (envelope, size) = saved_checkpoint(&pool, &tenant, run_id)
            .await
            .expect("作成系の道具を呼んだ通常の会話はチェックポイントを保存する");
        eprintln!("チェックポイントの大きさ（emit_workflow の 1 手の後）: {size} バイト");
        assert_eq!(envelope["checkpoint"]["spent"]["steps"], 1, "{envelope}");
        assert!(checkpoint_calls(&envelope, "emit_workflow"), "{envelope}");
        assert!(
            envelope["checkpoint"].get("budget_extension").is_none(),
            "広げたことはチェックポイントに書かない"
        );
        assert!(envelope["event_seq"].as_i64().unwrap() > 0, "{envelope}");
        let created: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM artifact WHERE tenant_id = $1 AND kind = 'workflow' AND name = $2",
        )
        .bind(&tenant)
        .bind(&name)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(created, 1);

        // 引き継いだ run は境界から続け、2 手目の回答で自然に終わる（元の 1 手なら手数の上限で止まる）。
        let_it_retry(&pool, &tenant, run_id).await;
        wait_done_without_asking_again(&pool, &tenant, run_id).await;
        let (_, fencing, _) = run_row(&pool, &tenant, run_id).await;
        assert_eq!(fencing, 2, "同じ run を引き継いだ");
        assert!(
            saved_checkpoint(&pool, &tenant, run_id).await.is_none(),
            "確定でチェックポイントは消える"
        );
        let (content, stop_reason) = answer(&pool, &tenant, message_id).await;
        assert_eq!(
            stop_reason, None,
            "再開の後も広げた予算のまま（手数の上限で止まらない）: {content}"
        );
        assert_eq!(
            tool_call_blocks(&content, "emit_workflow"),
            1,
            "作成をやり直さない: {content}"
        );
        assert!(
            content
                .as_array()
                .unwrap()
                .iter()
                .any(|b| b["type"] == "text" && b["text"].as_str().unwrap().contains("回答:")),
            "{content}"
        );
        assert_eq!(
            events(&pool, &tenant, run_id, "tool_call").await.len(),
            1,
            "呼び出しのイベントも 1 回だけ"
        );
        let (version,): (i64,) = sqlx::query_as(
            "SELECT current_version FROM artifact WHERE tenant_id = $1 AND kind = 'workflow' AND name = $2",
        )
        .bind(&tenant)
        .bind(&name)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(version, 1, "版は増えない");
        // 1 手目は元の 1 応答の上限（8192）、再開した 2 手目は広げた上限（16384）。
        let caps: Vec<Value> = events(&pool, &tenant, run_id, "context_usage")
            .await
            .into_iter()
            .map(|e| e["usage"]["reserved_output_tokens"].clone())
            .collect();
        assert_eq!(caps, [json!(8192), json!(16_384)], "{caps:?}");
        for h in handles {
            h.abort();
        }
    })
    .await;
}

/// 作成系の道具を使わない通常の会話は、断られてもチェックポイントを書かない（既定の振る舞いは変えない）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plain_conversations_do_not_save_checkpoints() {
    run_in_db("chat_builder_cp_plain", |pool| async move {
        let tenant = format!("t-{}", Uuid::new_v4());
        let alice = user_ctx(&tenant, "alice");
        let store = ChatStore::connect(pool.clone(), Arc::new(AllowAll), None)
            .await
            .unwrap();
        let artifacts = Arc::new(artifact::ArtifactStore::new(
            pool.clone(),
            Arc::new(AllowAll),
        ));
        let workflows = Arc::new(WorkflowStore::new(Arc::clone(&artifacts)));
        let worker = ChatWorker::new(
            pool.clone(),
            store.clone(),
            deps(limited_gateway(pool.clone()), workflows, true, None),
            worker_config(4),
        );
        let handles = worker.spawn(1);
        let thread = conversation(&pool, &store, &alice).await;
        let (run_id, _) = post(
            &store,
            &alice,
            thread,
            r#"calltool:list_workflows {"query":"経費"}"#,
        )
        .await;
        wait_rate_limited(&pool, &tenant, run_id).await;
        assert_eq!(
            events(&pool, &tenant, run_id, "tool_call").await.len(),
            1,
            "1 手目で読み取りの道具を呼んでから断られた"
        );
        assert!(
            saved_checkpoint(&pool, &tenant, run_id).await.is_none(),
            "作成系の道具を呼ばない会話はチェックポイントを書かない"
        );
        for h in handles {
            h.abort();
        }
    })
    .await;
}

/// 試運転（try_workflow）の次の生成で断られて再開しても、承認カードは 1 回だけで、試運転は二重に走らない
/// （T13 の受け入れの先取り・R1 の残り）。試運転はステップの終わりの境界から保存を始める。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_trial_is_not_run_twice_when_the_next_generation_is_rate_limited() {
    let Ok(fga_url) = shiki_testenv::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return;
    };
    run_in_db("chat_builder_cp_try", |pool| async move {
        let fga = Arc::new(
            OpenFgaClient::connect(
                reqwest::Client::new(),
                &OpenFgaConfig {
                    base_url: fga_url.clone(),
                    store_name: format!("shiki-chat-builder-cp-{}", Uuid::new_v4()),
                },
                &authz::model::default_model(),
            )
            .await
            .expect("OpenFGA 接続"),
        );
        let authz: Arc<dyn AuthzClient> = fga.clone();
        let tenant = format!("t-{}", Uuid::new_v4());
        let alice = user_ctx(&tenant, "alice");
        let artifacts = Arc::new(artifact::ArtifactStore::new(pool.clone(), Arc::clone(&authz)));
        let workflows = Arc::new(WorkflowStore::new(Arc::clone(&artifacts)));
        let runs = Arc::new(RunStore::new(
            pool.clone(),
            workflow_engine::DEFAULT_LEASE_SECS,
        ));
        let launcher = Arc::new(WorkflowRunLauncher::new(
            DelegationStore::new(pool.clone(), Arc::clone(&authz)),
            (*workflows).clone(),
            (*runs).clone(),
        ));
        let store = ChatStore::connect(pool.clone(), Arc::clone(&authz), None)
            .await
            .unwrap();
        let ir = json!({
            "ir_version": 1,
            "name": format!("try-{}", Uuid::new_v4().simple()),
            "display_name": "経費の確認",
            "nodes": [ { "id": "ask", "type": "llm.invoke", "label": "まとめる", "params": { "prompt": "まとめて" } } ],
            "edges": []
        });
        let (wf, _) = workflows
            .create(&alice, &ir, &Catalog::default(), None)
            .await
            .expect("save");
        let worker = ChatWorker::new(
            pool.clone(),
            store.clone(),
            deps(limited_gateway(pool.clone()), Arc::clone(&workflows), false, None),
            worker_config(4),
        )
        .with_workflow_tries(Arc::clone(&launcher), Arc::clone(&runs));
        let handles = worker.spawn(2);
        let thread = conversation(&pool, &store, &alice).await;
        let (run_id, message_id) = post(
            &store,
            &alice,
            thread,
            &format!(r#"calltool:try_workflow {{"workflow_id":"{wf}","version":1}}"#),
        )
        .await;

        // 承認カード → 承認 → 試運転が 1 本起動し、そのステップの終わりで保存し、次の生成で断られる。
        let deadline = Instant::now() + Duration::from_secs(60);
        let call_id = loop {
            if let (status, _, Some(id)) = run_row(&pool, &tenant, run_id).await {
                if status == "waiting_approval" {
                    break id;
                }
            }
            assert!(Instant::now() < deadline, "承認待ちに入らない");
            tokio::time::sleep(Duration::from_millis(200)).await;
        };
        assert!(store
            .submit_approval(&alice, thread, run_id, &call_id, "try_workflow", true, None)
            .await
            .unwrap());
        wait_rate_limited(&pool, &tenant, run_id).await;
        let (envelope, size) = saved_checkpoint(&pool, &tenant, run_id)
            .await
            .expect("試運転を呼んだ通常の会話はチェックポイントを保存する");
        eprintln!("チェックポイントの大きさ（try_workflow の 1 手の後）: {size} バイト");
        assert!(checkpoint_calls(&envelope, "try_workflow"), "{envelope}");
        let count = |sql: &'static str| {
            let (pool, tenant) = (pool.clone(), tenant.clone());
            async move {
                sqlx::query_scalar::<_, i64>(sql)
                    .bind(&tenant)
                    .bind(wf)
                    .fetch_one(&pool)
                    .await
                    .unwrap()
            }
        };
        let trial_runs = "SELECT count(*) FROM workflow_run WHERE tenant_id = $1 AND workflow_id = $2";
        let ledger =
            "SELECT count(*) FROM workflow_trial_run WHERE tenant_id = $1 AND workflow_id = $2";
        assert_eq!(count(trial_runs).await, 1);

        // 引き継いだ run は境界から続け、承認を求め直さず、試運転を起動し直さずに終わる。
        let_it_retry(&pool, &tenant, run_id).await;
        wait_done_without_asking_again(&pool, &tenant, run_id).await;
        let (_, fencing, _) = run_row(&pool, &tenant, run_id).await;
        assert_eq!(fencing, 2, "同じ run を引き継いだ");
        assert_eq!(count(trial_runs).await, 1, "試運転は二重に走らない");
        assert_eq!(count(ledger).await, 1);
        let allows: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_log WHERE tenant_id = $1 \
             AND action = 'workflow.try_run' AND decision = 'allow'",
        )
        .bind(&tenant)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(allows, 1);
        assert_eq!(
            events(&pool, &tenant, run_id, "approval_requested")
                .await
                .len(),
            1,
            "承認カードは 1 回だけ"
        );
        assert_eq!(
            events(&pool, &tenant, run_id, "tool_call").await.len(),
            1,
            "試運転の呼び出しは 1 回だけ"
        );
        let (content, stop_reason) = answer(&pool, &tenant, message_id).await;
        assert_eq!(stop_reason, None, "{content}");
        assert_eq!(tool_call_blocks(&content, "try_workflow"), 1, "{content}");
        assert!(
            content.as_array().unwrap().iter().any(|b| b["type"]
                == "tool_result"
                && b["content"].as_str().unwrap().starts_with("試運転")),
            "試運転の観測は会話に残る: {content}"
        );
        for h in handles {
            h.abort();
        }
        let _ = reqwest::Client::new()
            .delete(format!("{fga_url}/stores/{}", fga.store_id()))
            .send()
            .await;
    })
    .await;
}

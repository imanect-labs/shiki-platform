//! 承認待ちの run がプール接続を握らないことの結合テスト（検証 P1: 承認待ちでプールが尽きる）。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）＋実 OpenFGA（`OPENFGA_TEST_URL`）で、**プールの大きさ
//! （3）より多い** `data_record_write` の承認待ちを作り、次を確かめる。
//!
//! - 承認待ちの間、ワーカーは接続を 1 本も持たない（プールの全接続をテストが借りられる）。他の
//!   ユーザーのチャットは返り、承認の送信もすぐ通る。承認した run は再開して完了する。
//! - 承認待ちの間ジョブは再配信されない（可視期限をハートビートが延ばす。attempts も fencing も
//!   増えない）。
//! - ワーカーが落ちて（再起動を模して全タスクを止めて）別のワーカーが再配信を拾っても、再び承認
//!   待ちに入った run は接続を持たない。承認すれば完了する。
//!
//! 以前は claim に使った接続を ack まで握っていたため、承認待ちが 3 件でプールが尽き、残りの run は
//! claim も承認待ちのポーリングもできず止まった。

#![allow(
    clippy::pedantic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal};
use chat::{ChatStore, ChatWorker, WorkerConfig};
use data::{DataStore, FieldDef, FieldType, NewDataTable, RefResolver, TableSchema};
use llm_gateway::{
    GatewayConfig, LlmGateway, ModelCatalog, ModelEntry, ProviderConfig, ProviderKind,
};
use sqlx::{postgres::PgPoolOptions, PgPool};
use uuid::Uuid;

/// ワーカー・ストア・データが共有するプールの大きさ（承認待ちの件数より小さく）。
const POOL: u32 = 3;
/// 承認待ちにする run の数（> POOL）。
const PARKED: usize = 5;
/// run のリース（ジョブの可視期限はこの 2 倍）。
const LEASE_SECS: i64 = 4;

struct AllowRefs;
#[async_trait]
impl RefResolver for AllowRefs {
    async fn user_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
        Ok(true)
    }
    async fn role_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
        Ok(true)
    }
    async fn file_readable(&self, _: &AuthContext, _: Uuid) -> Result<bool, String> {
        Ok(true)
    }
}

/// テスト専用 DB（生成ジョブの待ち行列を他のテストのワーカーと共有しない）と、
/// 小さいプール（ワーカー用）・観測用プールを返す。
async fn setup() -> Option<(PgPool, PgPool, Arc<dyn AuthzClient>)> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let Ok(fga_url) = std::env::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return None;
    };
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&db_url)
        .await
        .expect("Postgres へ接続できること");
    let name = format!("chat_approval_pool_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&admin)
        .await
        .expect("テスト用 DB を作れること");
    let mut url = reqwest::Url::parse(&db_url).expect("DB URL");
    url.set_path(&name);
    let observe = PgPoolOptions::new()
        .max_connections(2)
        .connect(url.as_str())
        .await
        .expect("テスト用 DB へ接続できること");
    sqlx::migrate!("../../migrations")
        .run(&observe)
        .await
        .expect("マイグレーション適用");
    let pool = PgPoolOptions::new()
        .max_connections(POOL)
        .acquire_timeout(Duration::from_secs(10))
        .connect(url.as_str())
        .await
        .expect("小さいプール");
    let fga = OpenFgaClient::connect(
        reqwest::Client::new(),
        &OpenFgaConfig {
            base_url: fga_url,
            store_name: format!("shiki-chat-approval-pool-{}", Uuid::new_v4()),
        },
        &authz::model::default_model(),
    )
    .await
    .expect("OpenFGA 接続");
    Some((pool, observe, Arc::new(fga)))
}

fn ctx(tenant: &str, user: &str) -> AuthContext {
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
        tenant.into(),
    )
}

fn stub_gateway(pool: PgPool) -> LlmGateway {
    let config = GatewayConfig {
        provider: ProviderConfig {
            kind: ProviderKind::Stub,
            ..Default::default()
        },
        catalog: ModelCatalog {
            default_model: "m".into(),
            models: vec![ModelEntry {
                id: "m".into(),
                ..Default::default()
            }],
        },
        ..Default::default()
    };
    LlmGateway::build(pool, reqwest::Client::new(), config).expect("gateway")
}

fn worker(pool: &PgPool, store: &ChatStore, data: &DataStore) -> ChatWorker {
    ChatWorker::new(
        pool.clone(),
        store.clone(),
        chat::WorkerDeps {
            gateway: stub_gateway(pool.clone()),
            search: None,
            sandbox: None,
            artifacts: None,
            web_search: None,
            parser: None,
            storage: None,
            workflow_resolver: None,
            skill_artifacts: None,
            skill_catalog: None,
            workflow_store: None,
            workflow_catalog: None,
            collab: None,
            tabular: None,
            office: None,
            office_live: None,
            office_creator: None,
            office_engine: None,
        },
        WorkerConfig {
            system_prompt: "あなたはアシスタントです。".into(),
            model: Some("m".into()),
            lease_secs: LEASE_SECS,
            max_steps: 4,
            ..Default::default()
        },
    )
    .with_data(data.clone())
}

/// run の (status, fencing_token, 承認待ちのツール呼び出し id)。
async fn run_row(observe: &PgPool, run_id: Uuid) -> (String, i64, Option<String>) {
    sqlx::query_as(
        "SELECT status, fencing_token, pending_approval->>'tool_call_id' \
         FROM generation_run WHERE run_id = $1",
    )
    .bind(run_id)
    .fetch_one(observe)
    .await
    .unwrap()
}

/// 全 run が fencing `fencing` で承認待ちに入るまで待ち、承認待ちのツール呼び出し id を返す。
async fn wait_parked(
    observe: &PgPool,
    runs: &[Uuid],
    fencing: i64,
    within: Duration,
) -> Vec<String> {
    let deadline = Instant::now() + within;
    loop {
        let mut ids = Vec::new();
        for r in runs {
            let (status, f, tc) = run_row(observe, *r).await;
            if status == "waiting_approval" && f == fencing {
                if let Some(tc) = tc {
                    ids.push(tc);
                }
            }
        }
        if ids.len() == runs.len() {
            return ids;
        }
        assert!(
            Instant::now() < deadline,
            "承認待ちに入らない（{}/{} 件・fencing {fencing}）",
            ids.len(),
            runs.len()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn wait_status(observe: &PgPool, run_id: Uuid, want: &str, within: Duration) {
    let deadline = Instant::now() + within;
    loop {
        let (status, _, _) = run_row(observe, run_id).await;
        if status == want {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "run {run_id} が {want} にならない（{status}）"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// ワーカーが接続を握っていない＝プールの全接続をこちらが同時に借りられる。
async fn pool_is_not_held(pool: &PgPool) {
    let mut held = Vec::new();
    for i in 0..POOL {
        let c = tokio::time::timeout(Duration::from_secs(8), pool.acquire())
            .await
            .unwrap_or_else(|_| {
                panic!("{i} 本目の接続を借りられない（承認待ちの run が握っている）")
            })
            .expect("acquire");
        held.push(c);
    }
    drop(held);
}

/// 承認待ちでない別ユーザーのチャットが返る。
async fn other_user_chat_completes(store: &ChatStore, observe: &PgPool, c: &AuthContext) {
    let thread = store
        .create_thread(c, "別ユーザー", false, None, None)
        .await
        .unwrap();
    let res = store
        .post_message(
            c,
            thread.id,
            "hello",
            &[],
            None,
            Some(false),
            false,
            &[],
            None,
        )
        .await
        .unwrap();
    wait_status(observe, res.run_id, "done", Duration::from_secs(30)).await;
}

/// 承認の送信が（プールが尽きていないので）すぐ通る。
async fn approve(store: &ChatStore, c: &AuthContext, thread: Uuid, run: Uuid, tool_call_id: &str) {
    let t = Instant::now();
    let accepted = store
        .submit_approval(
            c,
            thread,
            run,
            tool_call_id,
            "data_record_write",
            true,
            None,
        )
        .await
        .expect("承認を送れる");
    assert!(accepted, "決定が採用される");
    assert!(
        t.elapsed() < Duration::from_secs(5),
        "承認の送信が待たされない: {:?}",
        t.elapsed()
    );
}

/// ジョブの (attempts, 件数)。
async fn jobs_of(observe: &PgPool, runs: &[Uuid]) -> Vec<i32> {
    let ids: Vec<String> = runs.iter().map(Uuid::to_string).collect();
    sqlx::query_scalar(
        "SELECT attempts FROM job_queue WHERE queue = 'chat_generation' \
         AND payload->>'run_id' = ANY($1) ORDER BY id",
    )
    .bind(&ids)
    .fetch_all(observe)
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parked_approvals_beyond_pool_size_do_not_starve_the_server() {
    let Some((pool, observe, fga)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let data = DataStore::new(pool.clone(), Arc::clone(&fga), Arc::new(AllowRefs));
    let table = data
        .create_table(
            &alice,
            NewDataTable {
                name: "経費".into(),
                schema: TableSchema {
                    fields: vec![
                        FieldDef::new("title", FieldType::Text),
                        FieldDef::new("amount", FieldType::Number),
                    ],
                    status_field: None,
                    row_policy: None,
                    field_policy: vec![],
                    aggregate_min_rows: None,
                    fsm_ref: None,
                    rag: None,
                },
            },
            None,
        )
        .await
        .expect("table");
    let store = ChatStore::connect(pool.clone(), Arc::clone(&fga), None)
        .await
        .unwrap();
    let first = worker(&pool, &store, &data).spawn(2);

    // プールより多い承認待ち（各スレッド 1 本・承認必須の data_record_write）。
    let mut threads = Vec::new();
    let mut runs = Vec::new();
    for i in 0..PARKED {
        let thread = store
            .create_thread(&alice, &format!("承認 {i}"), true, None, None)
            .await
            .unwrap();
        let text = format!(
            r#"calltool:data_record_write {{"table":"経費","op":"create","data":{{"title":"申請 {i}","amount":{i}}}}}"#
        );
        let res = store
            .post_message(
                &alice,
                thread.id,
                &text,
                &[],
                None,
                Some(true),
                false,
                &[],
                None,
            )
            .await
            .unwrap();
        threads.push(thread.id);
        runs.push(res.run_id);
    }
    let parked_at = Instant::now();
    let tool_calls = wait_parked(&observe, &runs, 1, Duration::from_secs(60)).await;

    // 承認待ちの間、ワーカーは接続を持たない。他のユーザーのチャットは返る。
    pool_is_not_held(&pool).await;
    other_user_chat_completes(&store, &observe, &bob).await;

    // 可視期限（リースの 2 倍）を十分に過ぎても、ジョブは再配信されない（attempts・fencing が増えない）。
    let vt = Duration::from_secs(u64::try_from(LEASE_SECS * 2).unwrap());
    if let Some(rest) = (vt * 2 + Duration::from_secs(2)).checked_sub(parked_at.elapsed()) {
        tokio::time::sleep(rest).await;
    }
    assert_eq!(
        jobs_of(&observe, &runs).await,
        vec![1; PARKED],
        "承認待ちの間ジョブは残り、再配信されない"
    );
    for r in &runs {
        let (status, fencing, _) = run_row(&observe, *r).await;
        assert_eq!((status.as_str(), fencing), ("waiting_approval", 1), "{r}");
    }

    // 1 件目を承認すると再開して完了する（他は承認待ちのまま）。
    approve(&store, &alice, threads[0], runs[0], &tool_calls[0]).await;
    wait_status(&observe, runs[0], "done", Duration::from_secs(30)).await;

    // 再起動を模す: ワーカーの全タスクを止め、別のワーカーを起動する。残りの run はリースと可視期限が
    // 切れたあと再配信され、取り直して（fencing 2）また承認待ちに入る。
    for h in &first {
        h.abort();
    }
    let rest = &runs[1..];
    // うち 1 件はジョブまで失われた（以前の実装で、承認待ちの途中に再配信されたジョブが ack されて
    // 消えていた状態）。リースが切れたあと sweeper がジョブを積み直し、他と同じく再開する。
    sqlx::query("DELETE FROM job_queue WHERE payload->>'run_id' = $1")
        .bind(rest[0].to_string())
        .execute(&observe)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(u64::try_from(LEASE_SECS).unwrap() + 1)).await;
    assert_eq!(
        store.reap_orphaned_runs(0).await.unwrap(),
        1,
        "ジョブを失った承認待ちだけを積み直す（ジョブの残る run は再配信に任せる）"
    );
    assert_eq!(jobs_of(&observe, rest).await.len(), rest.len());
    let _second = worker(&pool, &store, &data).spawn(2);
    let tool_calls = wait_parked(&observe, rest, 2, Duration::from_secs(60)).await;
    assert!(
        rest.len() > POOL as usize,
        "再配信後もプールより多く承認待ちにする"
    );
    pool_is_not_held(&pool).await;
    other_user_chat_completes(&store, &observe, &bob).await;

    // 承認すれば全部完了する。
    for (i, r) in rest.iter().enumerate() {
        approve(&store, &alice, threads[i + 1], *r, &tool_calls[i]).await;
    }
    for r in rest {
        wait_status(&observe, *r, "done", Duration::from_secs(30)).await;
    }
    assert!(
        jobs_of(&observe, &runs).await.is_empty(),
        "全ジョブが ack される"
    );
    let created: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM data_record WHERE tenant_id = $1 AND table_id = $2",
    )
    .bind(&tenant)
    .bind(table.id)
    .fetch_one(&observe)
    .await
    .unwrap_or(-1);
    assert_eq!(created, PARKED as i64, "承認した書込が 1 件ずつ実行される");
}

/// ジョブを失った run の回収（sweeper）。承認待ちは以前は対象外で、再起動の前に承認待ちだった run が
/// ジョブも有効なリースも無いまま永久に承認待ちで残った（検証 P1）。
///
/// - 承認待ち・ジョブ無し・承認の期限内 → ジョブを積み直す（ワーカーが取り直して承認を求め直す）。
/// - 承認待ち・ジョブ無し・最後に生きていた時刻から承認の期限切れ → failed（承認待ちも消す）。
/// - ジョブの残る run（承認待ち・生成中）は再配信に任せて触らない。
/// - 生成中・ジョブ無し → 従来どおり failed。
/// - 二度目の回収で二重に積まない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sweeper_recovers_approval_runs_that_lost_their_job() {
    use chat::RunStatus;

    let Some((pool, observe, fga)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let store = ChatStore::connect(pool.clone(), Arc::clone(&fga), None)
        .await
        .unwrap();
    // run を作って（ジョブが積まれる）リース 1 秒で取り、必要なら承認待ちにする。
    let make = |waiting: bool, keep_job: bool| {
        let (store, alice, observe) = (store.clone(), alice.clone(), observe.clone());
        async move {
            let thread = store
                .create_thread(&alice, "回収", true, None, None)
                .await
                .unwrap();
            let res = store
                .post_message(
                    &alice,
                    thread.id,
                    "hello",
                    &[],
                    None,
                    Some(true),
                    false,
                    &[],
                    None,
                )
                .await
                .unwrap();
            let run = store
                .claim_run(res.run_id, "dead-worker", 1)
                .await
                .unwrap()
                .unwrap();
            if waiting {
                store
                    .set_pending_approval(
                        run.run_id,
                        run.fencing_token,
                        "tc-1",
                        "data_record_write",
                    )
                    .await
                    .unwrap();
                store
                    .set_run_status_fenced(
                        run.run_id,
                        run.fencing_token,
                        RunStatus::WaitingApproval,
                    )
                    .await
                    .unwrap();
            }
            if !keep_job {
                sqlx::query("DELETE FROM job_queue WHERE payload->>'run_id' = $1")
                    .bind(run.run_id.to_string())
                    .execute(&observe)
                    .await
                    .unwrap();
            }
            run.run_id
        }
    };
    let requeue = make(true, false).await;
    let expired = make(true, false).await;
    let waiting_with_job = make(true, true).await;
    let running_lost = make(false, false).await;
    let running_with_job = make(false, true).await;
    // 最後に生きていたのが承認の期限（30 分）より前。
    sqlx::query(
        "UPDATE generation_run SET lease_until = now() - interval '31 minutes' WHERE run_id = $1",
    )
    .bind(expired)
    .execute(&observe)
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await; // リース（1 秒）を切らす

    assert_eq!(store.reap_orphaned_runs(0).await.unwrap(), 3);
    let pending: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT pending_approval FROM generation_run WHERE run_id = $1")
            .bind(expired)
            .fetch_one(&observe)
            .await
            .unwrap();
    assert_eq!(run_row(&observe, requeue).await.0, "waiting_approval");
    assert_eq!(
        jobs_of(&observe, &[requeue]).await,
        vec![0],
        "ジョブを積み直す"
    );
    assert_eq!(run_row(&observe, expired).await.0, "failed");
    assert!(pending.is_none(), "期限切れの承認待ちは消す");
    assert!(jobs_of(&observe, &[expired]).await.is_empty());
    assert_eq!(
        run_row(&observe, waiting_with_job).await.0,
        "waiting_approval"
    );
    assert_eq!(jobs_of(&observe, &[waiting_with_job]).await, vec![0]);
    assert_eq!(run_row(&observe, running_lost).await.0, "failed");
    assert_eq!(
        run_row(&observe, running_with_job).await.0,
        "running",
        "ジョブの残る run は再配信に任せる"
    );
    // 期限切れは会話にも定型文で出る（承認の期限切れと分かる）。
    let last_error: Option<String> =
        sqlx::query_scalar("SELECT last_error FROM generation_run WHERE run_id = $1")
            .bind(expired)
            .fetch_one(&observe)
            .await
            .unwrap();
    assert!(last_error.unwrap_or_default().contains("approval deadline"));
    let shown: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT payload FROM generation_event WHERE run_id = $1 AND type = 'error'",
    )
    .bind(expired)
    .fetch_all(&observe)
    .await
    .unwrap();
    assert!(
        shown
            .iter()
            .any(|p| p.to_string().contains("承認の期限が切れた")),
        "{shown:?}"
    );

    // 二度目は何もしない（積み直したジョブがあるので二重に積まない）。
    assert_eq!(store.reap_orphaned_runs(0).await.unwrap(), 0);
    assert_eq!(jobs_of(&observe, &[requeue]).await, vec![0]);
}

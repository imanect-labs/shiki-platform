//! 自動実行の起動の見送りの記録（GAP-67・実 Postgres＋live OpenFGA）。
//!
//! 起動口（WorkflowRunLauncher）が見送った理由（実行の前に止める設定・定義を読めない・委譲の不成立）を、ワークフロー
//! ごとに 1 行で記録し、続く見送りの回数と時刻を進める。自動実行が run を作れたら消え、オンにし直す・オフにすると
//! 消える。登録の口（RegistrationService::launch_skip）は、いまオンにしている版の記録だけを返す。返す文は作成者
//! 向けで、委譲元の id や内部の理由の文を含まない。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use artifact::ArtifactStore;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal, PrincipalKind, Relation};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;
use workflow_engine::{
    Catalog, DelegationStore, GrantRequest, LaunchOutcome, RegistrationService, RunLauncher,
    RunStore, SkipReason, WorkflowRunLauncher, WorkflowStore,
};

async fn setup() -> Option<(PgPool, Arc<OpenFgaClient>)> {
    let (Ok(db), Ok(fga)) = (
        std::env::var("STORAGE_TEST_DATABASE_URL"),
        std::env::var("OPENFGA_TEST_URL"),
    ) else {
        eprintln!("STORAGE_TEST_DATABASE_URL / OPENFGA_TEST_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db)
        .await
        .expect("db");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    let model: Value =
        serde_json::from_str(include_str!("../../authz/model/authorization-model.json")).unwrap();
    let cfg = OpenFgaConfig {
        base_url: fga,
        store_name: format!("shiki-launch-skip-{}", Uuid::new_v4()),
    };
    let client = Arc::new(
        OpenFgaClient::connect(reqwest::Client::new(), &cfg, &model)
            .await
            .expect("fga"),
    );
    Some((pool, client))
}

fn user_ctx(tenant: &str, user: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
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

/// テストの道具（ワークフロー・委譲・起動口・登録の口）。
struct Env {
    pool: PgPool,
    fga: Arc<OpenFgaClient>,
    tenant: String,
    alice: AuthContext,
    artifacts: Arc<ArtifactStore>,
    workflows: WorkflowStore,
    delegation: DelegationStore,
    launcher: WorkflowRunLauncher,
    registration: RegistrationService,
    /// 作ったワークフロー（片付けで消す）。
    created: std::sync::Mutex<Vec<Uuid>>,
}

impl Env {
    async fn new() -> Option<Self> {
        let (pool, fga) = setup().await?;
        let tenant = format!("t-{}", Uuid::new_v4());
        let alice = user_ctx(&tenant, "alice");
        let artifacts = Arc::new(ArtifactStore::new(
            pool.clone(),
            fga.clone() as Arc<dyn AuthzClient>,
        ));
        let workflows = WorkflowStore::new(artifacts.clone());
        let delegation = DelegationStore::new(pool.clone(), fga.clone() as Arc<dyn AuthzClient>);
        let runs = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
        let launcher = WorkflowRunLauncher::new(delegation.clone(), workflows.clone(), runs);
        let registration = RegistrationService::new(pool.clone(), delegation.clone());
        Some(Env {
            pool,
            fga,
            tenant,
            alice,
            artifacts,
            workflows,
            delegation,
            launcher,
            registration,
            created: std::sync::Mutex::new(Vec::new()),
        })
    }

    /// ワークフローを保存する（片付けの対象に入れる）。
    async fn create(&self, ir: &Value) -> Uuid {
        let (wf, _) = self
            .workflows
            .create(&self.alice, ir, &Catalog::default(), None)
            .await
            .expect("save");
        self.created.lock().unwrap().push(wf);
        wf
    }

    /// スケジュールの起動（schedule）。
    async fn fire(&self, wf: Uuid) -> LaunchOutcome {
        self.launcher
            .launch(
                &self.tenant,
                wf,
                "schedule",
                &format!("{wf}:0"),
                &Value::Null,
                None,
            )
            .await
    }

    /// ワークフローの一覧の要約が、見送り続けていると言うかを確かめる（登録の API の `launch_skip` と同じ条件・
    /// GAP-67）。
    async fn expect_listed_as_skipping(&self, wf: Uuid, skipping: bool, why: &str) {
        let items = workflow_engine::WorkflowSummaryStore::new(self.pool.clone())
            .list(&self.tenant, &[wf])
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].launch_skipped, skipping, "{why}");
    }

    /// 記録の行の数（版を問わず・消えたかを確かめる）。
    async fn rows(&self, wf: Uuid) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM workflow_launch_skip WHERE tenant_id = $1 AND workflow_id = $2",
        )
        .bind(&self.tenant)
        .bind(wf)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    /// 片付け（run・ワークフロー・登録〔見送りの記録・委譲・トリガは外部キーで消える〕・OpenFGA のストア）。
    async fn cleanup(&self) {
        let workflows = self.created.lock().unwrap().clone();
        for wf in workflows {
            let _ = self.artifacts.delete(&self.alice, wf, None).await;
        }
        for table in [
            "workflow_run",
            "workflow_registration",
            "artifact_version",
            "artifact",
        ] {
            sqlx::query(&format!("DELETE FROM {table} WHERE tenant_id = $1"))
                .bind(&self.tenant)
                .execute(&self.pool)
                .await
                .unwrap();
        }
        let fga_url = std::env::var("OPENFGA_TEST_URL").unwrap();
        let deleted = reqwest::Client::new()
            .delete(format!("{fga_url}/stores/{}", self.fga.store_id()))
            .send()
            .await
            .unwrap();
        assert!(deleted.status().is_success(), "{}", deleted.status());
    }
}

/// テストの本体を走らせ、途中の assert が落ちても片付けてから落とす（行と OpenFGA のストアを残さない）。
async fn run<F, Fut>(body: F)
where
    F: FnOnce(Arc<Env>) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let Some(env) = Env::new().await else { return };
    let env = Arc::new(env);
    let outcome = tokio::spawn(body(Arc::clone(&env))).await;
    env.cleanup().await;
    if let Err(e) = outcome {
        std::panic::resume_unwind(e.into_panic());
    }
}

fn ask_ir(prompt: &str) -> Value {
    json!({
        "ir_version": 1,
        "name": format!("skip-{}", Uuid::new_v4().simple()),
        "nodes": [ { "id": "ask", "type": "llm.invoke", "params": { "prompt": prompt } } ],
        "edges": [],
        "triggers": [ { "kind": "schedule", "cron": "0 9 * * *", "tz": "Asia/Tokyo" } ]
    })
}

/// 実行の前に止める設定がある版・定義を読めない版の見送りを、理由と回数で記録する。run を作れたら消え、オンにし
/// 直す・オフにすると消える（GAP-67）。
#[tokio::test]
async fn skipped_launches_are_recorded_until_they_run_or_are_switched() {
    run(skipped_launches_body).await;
}

async fn skipped_launches_body(env: Arc<Env>) {
    // AI に聞くの「AI への指示」が空の v1（保存は警告だけで通る）を、有効化の前の確認を通さずにオンにする（前の版で
    // オンにした定義を模す・BUG-51）。
    let mut ir = ask_ir("");
    let wf = env.create(&ir).await;
    env.delegation
        .enable(&env.alice, wf, 1, &[], &[])
        .await
        .expect("enable");
    for _ in 0..2 {
        let out = env.fire(wf).await;
        assert!(
            matches!(
                out,
                LaunchOutcome::Skipped(SkipReason::DelegationInvalid(_))
            ),
            "{out:?}"
        );
    }
    let skip = env
        .registration
        .launch_skip(&env.tenant, wf)
        .await
        .unwrap()
        .expect("見送りを記録する");
    assert_eq!(
        (skip.code.as_str(), skip.version, skip.count),
        ("not_ready", 1, 2)
    );
    assert_eq!(
        skip.message,
        "オンにしている v1 に、空の欄など実行できない設定があります。"
    );
    assert!(skip
        .fix()
        .starts_with("エディターで設定を直して保存してから"));
    assert!(skip.first_at <= skip.last_at);
    env.expect_listed_as_skipping(wf, true, "一覧も見送り中と言う")
        .await;

    // 直した v2 を保存しただけでは消えない（オンにしているのは v1）。オンにし直すと消える。
    ir["nodes"][0]["params"]["prompt"] = json!("今日の予定をまとめてください");
    let (v2, ir2) = env
        .workflows
        .update(&env.alice, wf, &ir, &Catalog::default(), Some(1), None)
        .await
        .expect("update");
    assert_eq!(v2, 2);
    assert_eq!(env.rows(wf).await, 1);
    env.registration
        .enable(&env.alice, wf, 2, &ir2, &[])
        .await
        .expect("enable v2");
    assert_eq!(env.rows(wf).await, 0, "オンにし直すと消える");
    env.expect_listed_as_skipping(wf, false, "オンにし直すと消える")
        .await;
    assert!(env
        .registration
        .launch_skip(&env.tenant, wf)
        .await
        .unwrap()
        .is_none());

    later_versions_and_causes(&env, wf).await;
}

/// 見送りの記録の続き: 別の版の記録は返さない・定義を読めない見送りは数え直す・オフにすると消える。
async fn later_versions_and_causes(env: &Env, wf: Uuid) {
    // v1 の見送りが残ったまま v2 をオンにした状態（記録を消す経路を通らない書き換え）でも、いまオンにしている版の
    // 記録だけを返す。v2 の自動実行が run を作れたら記録は消える。
    sqlx::query("UPDATE workflow_registration SET enabled_version = 1 WHERE tenant_id = $1")
        .bind(&env.tenant)
        .execute(&env.pool)
        .await
        .unwrap();
    assert!(matches!(env.fire(wf).await, LaunchOutcome::Skipped(_)));
    sqlx::query("UPDATE workflow_registration SET enabled_version = 2 WHERE tenant_id = $1")
        .bind(&env.tenant)
        .execute(&env.pool)
        .await
        .unwrap();
    assert!(
        env.registration
            .launch_skip(&env.tenant, wf)
            .await
            .unwrap()
            .is_none(),
        "別の版の記録は返さない"
    );
    env.expect_listed_as_skipping(wf, false, "一覧も別の版の記録で見送り中と言わない")
        .await;
    assert!(matches!(env.fire(wf).await, LaunchOutcome::Started(_)));
    assert_eq!(env.rows(wf).await, 0, "run を作れたら消える");

    // 自動実行の主体が定義を読めない（BUG-146）。理由が変わったら数え直す。
    let ns = env.alice.ns();
    env.fga
        .delete_tuple(
            &ns.workflow_principal(&wf.to_string()),
            Relation::Viewer,
            &ns.artifact(&wf.to_string()),
        )
        .await
        .unwrap();
    assert!(matches!(env.fire(wf).await, LaunchOutcome::Skipped(_)));
    let skip = env
        .registration
        .launch_skip(&env.tenant, wf)
        .await
        .unwrap()
        .expect("定義を読めない見送り");
    assert_eq!(
        (skip.code.as_str(), skip.version, skip.count),
        ("definition_unreadable", 2, 1)
    );
    assert_eq!(
        skip.message,
        "自動実行のとき、このワークフローを読み込めませんでした。"
    );
    assert_eq!(
        skip.fix(),
        "「自動実行の設定」を開いて、もう一度オンにしてください。"
    );
    env.expect_listed_as_skipping(wf, true, "定義を読めない見送り")
        .await;

    // オフにすると消える。
    env.registration.disable(&env.tenant, wf).await.unwrap();
    assert_eq!(env.rows(wf).await, 0, "オフにすると消える");
    env.expect_listed_as_skipping(wf, false, "オフにすると消える")
        .await;
}

/// 委譲の不成立（オンにした人の権限が外れた）で見送ったときは、自動実行が停止中になっても記録を返す。文に委譲元の
/// id や対象を出さない（ほかの人の情報を出さない・GAP-67）。
#[tokio::test]
async fn delegation_skips_do_not_name_the_delegator() {
    run(delegation_skips_body).await;
}

async fn delegation_skips_body(env: Arc<Env>) {
    let ir = json!({
        "ir_version": 1,
        "name": format!("skip-deleg-{}", Uuid::new_v4().simple()),
        "declared_scopes": ["storage.read"],
        "nodes": [ { "id": "r", "type": "storage.list", "params": {} } ],
        "edges": [],
        "triggers": [ { "kind": "schedule", "cron": "0 9 * * *", "tz": "Asia/Tokyo" } ]
    });
    let wf = env.create(&ir).await;
    let folder = env.alice.ns().folder("reports");
    env.fga
        .write_tuple(&env.alice.subject(), Relation::Viewer, &folder)
        .await
        .unwrap();
    env.delegation
        .enable(
            &env.alice,
            wf,
            1,
            &["storage.read".into()],
            &[GrantRequest {
                scope: "storage.read".into(),
                object: folder.clone(),
                relation: Relation::Viewer,
            }],
        )
        .await
        .expect("enable");
    // オンにした人がフォルダーを見られなくなる → 委譲の不成立。
    env.fga
        .delete_tuple(&env.alice.subject(), Relation::Viewer, &folder)
        .await
        .unwrap();
    assert!(matches!(
        env.fire(wf).await,
        LaunchOutcome::Skipped(SkipReason::DelegationInvalid(_))
    ));
    let status: String = sqlx::query_scalar(
        "SELECT status FROM workflow_registration WHERE tenant_id = $1 AND workflow_id = $2",
    )
    .bind(&env.tenant)
    .bind(wf)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(status, "suspended_reconsent");
    let skip = env
        .registration
        .launch_skip(&env.tenant, wf)
        .await
        .unwrap()
        .expect("停止中でも見送りを返す");
    assert_eq!((skip.code.as_str(), skip.count), ("delegation_invalid", 1));
    assert_eq!(
        skip.message,
        "オンにした人の権限が外れたか、使ってよい操作が増えました。"
    );
    for text in [skip.message.as_str(), skip.fix()] {
        assert!(
            !text.contains("alice") && !text.contains("reports") && !text.contains("委譲"),
            "{text}"
        );
    }
}

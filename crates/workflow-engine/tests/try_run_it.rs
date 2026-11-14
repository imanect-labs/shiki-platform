//! 会話の AI の試運転の下回り（D-10・D-197・実 Postgres＋live OpenFGA）。
//!
//! - 起動の入口（`WorkflowRunLauncher::start_version_as_editor`）: owner と editor は版を固定して起動でき、viewer は
//!   閲覧のみで断られて `workflow.try_run` の deny が監査に 1 行残る。見えない・無いもの・ワークフローでないものは Ir。
//!   試す値と、呼び出し側が決めた run の id が run に載る。
//!   入口の検証の順（実行の前に止める設定 → 入力）は手動の実行と同じで、入力の誤りは run を作らない。
//! - 許可の監査（`record_try_run_allowed`）は拒否と同じ対象で書く。
//! - run の要約（`RunStore::run_digest`）: 本人が起動した run だけ・map の要素は件数に畳む・出力は先頭だけ・
//!   通らなかった理由は上流の出口から・失敗は分類と参照番号（生の文を返さない）・作ったレコードは id だけ。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use artifact::{ArtifactKind, ArtifactRole, ArtifactStore, NewArtifact};
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal, PrincipalKind};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use storage::audit::AuditRecorder;
use storage::ShareTarget;
use uuid::Uuid;
use workflow_engine::run::graph::RunGraph;
use workflow_engine::run::{
    record_try_run_allowed, LauncherError, RecordChange, RunTrial, DIGEST_OUTPUT_CHARS,
    MAX_DIGEST_RECORDS,
};
use workflow_engine::{
    Catalog, DelegationStore, RunStatus, RunStore, StepStatus, WorkflowIr, WorkflowRunLauncher,
    WorkflowStore,
};

async fn setup() -> Option<(PgPool, Arc<OpenFgaClient>)> {
    let (Ok(db), Ok(fga)) = (
        shiki_testenv::var("STORAGE_TEST_DATABASE_URL"),
        shiki_testenv::var("OPENFGA_TEST_URL"),
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
        store_name: format!("shiki-try-run-{}", Uuid::new_v4()),
    };
    let client = Arc::new(
        OpenFgaClient::connect(reqwest::Client::new(), &cfg, &model)
            .await
            .expect("fga"),
    );
    Some((pool, client))
}

fn user_ctx(tenant: &str, org: &str, user: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: user.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        org.into(),
        authz::TenantId::parse(tenant).unwrap(),
    )
}

/// テストの道具（持ち主 alice・editor の bob・viewer の carol・共有されていない dave）。
struct Env {
    pool: PgPool,
    fga: Arc<OpenFgaClient>,
    tenant: String,
    alice: AuthContext,
    bob: AuthContext,
    carol: AuthContext,
    dave: AuthContext,
    artifacts: Arc<ArtifactStore>,
    workflows: WorkflowStore,
    launcher: WorkflowRunLauncher,
    runs: RunStore,
}

impl Env {
    async fn new() -> Option<Self> {
        let (pool, fga) = setup().await?;
        let tenant = format!("t-{}", Uuid::new_v4());
        let artifacts = Arc::new(ArtifactStore::new(
            pool.clone(),
            fga.clone() as Arc<dyn AuthzClient>,
        ));
        let workflows = WorkflowStore::new(artifacts.clone());
        let delegation = DelegationStore::new(pool.clone(), fga.clone() as Arc<dyn AuthzClient>);
        let runs = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
        let launcher = WorkflowRunLauncher::new(delegation, workflows.clone(), runs.clone());
        Some(Env {
            alice: user_ctx(&tenant, "acme", "alice"),
            bob: user_ctx(&tenant, "acme", "bob"),
            carol: user_ctx(&tenant, "acme", "carol"),
            dave: user_ctx(&tenant, "acme", "dave"),
            pool,
            fga,
            tenant,
            artifacts,
            workflows,
            launcher,
            runs,
        })
    }

    /// alice がワークフローを保存し、bob に編集・carol に閲覧を共有する。
    async fn create_shared(&self, ir: &Value) -> Uuid {
        let (wf, _) = self
            .workflows
            .create(&self.alice, ir, &Catalog::default(), None)
            .await
            .expect("save");
        for (who, role) in [
            ("bob", ArtifactRole::Editor),
            ("carol", ArtifactRole::Viewer),
        ] {
            self.artifacts
                .share(
                    &self.alice,
                    wf,
                    &ShareTarget::User { id: who.into() },
                    role,
                    None,
                )
                .await
                .expect("share");
        }
        wf
    }

    async fn run_version(&self, run_id: Uuid) -> i64 {
        sqlx::query_scalar("SELECT version FROM workflow_run WHERE tenant_id = $1 AND run_id = $2")
            .bind(&self.tenant)
            .bind(run_id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn runs_of(&self, wf: Uuid) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM workflow_run WHERE tenant_id = $1 AND workflow_id = $2",
        )
        .bind(&self.tenant)
        .bind(wf)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    /// 片付け（run・監査・ワークフロー・OpenFGA のストア）。
    async fn cleanup(&self) {
        for table in ["workflow_run", "audit_log", "artifact_version", "artifact"] {
            sqlx::query(&format!("DELETE FROM {table} WHERE tenant_id = $1"))
                .bind(&self.tenant)
                .execute(&self.pool)
                .await
                .unwrap();
        }
        let fga_url = shiki_testenv::var("OPENFGA_TEST_URL").unwrap();
        let deleted = reqwest::Client::new()
            .delete(format!("{fga_url}/stores/{}", self.fga.store_id()))
            .send()
            .await
            .unwrap();
        assert!(deleted.status().is_success(), "{}", deleted.status());
    }
}

/// テストの本体を走らせ、途中の assert が落ちても片付けてから落とす。
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

/// AI に聞くブロック 1 つ（`prompt` が空なら実行の前に止める・D-43）。`required` を渡すと入力の契約を付ける。
fn ask_ir(prompt: &str, required: Option<&str>) -> Value {
    let mut ir = json!({
        "ir_version": 1,
        "name": format!("try-{}", Uuid::new_v4().simple()),
        "nodes": [ { "id": "ask", "type": "llm.invoke", "params": { "prompt": prompt } } ],
        "edges": []
    });
    if let Some(key) = required {
        ir["input_schema"] = json!({ "type": "object", "required": [key], "properties": {
            key: { "type": "integer", "minimum": 1, "title": "金額" } } });
    }
    ir
}

/// 監査の行（actor・object_type・object_id・metadata・trace_id）。
type AuditRow = (String, String, String, Value, Option<String>);

/// owner と editor は版を固定して起動でき、viewer は閲覧のみで断られて deny が 1 行残る。見えない・無いものは Ir。
/// 許可の監査は拒否と同じ対象で書く。
#[tokio::test]
async fn editors_start_the_pinned_version_and_viewers_are_read_only() {
    run(editors_body).await;
}

async fn editors_body(env: Arc<Env>) {
    let mut ir = ask_ir("v1 の指示", None);
    let wf = env.create_shared(&ir).await;
    ir["nodes"][0]["params"]["prompt"] = json!("v2 の指示");
    let (v2, _) = env
        .workflows
        .update(&env.alice, wf, &ir, &Catalog::default(), Some(1), None)
        .await
        .expect("v2");
    assert_eq!(v2, 2);
    let trace = Uuid::new_v4().simple().to_string();
    let start = |ctx: &AuthContext, wf: Uuid, version: i64| {
        let (launcher, ctx, trace) = (env.launcher.clone(), ctx.clone(), trace.clone());
        async move {
            launcher
                .start_version_as_editor(
                    &ctx,
                    wf,
                    version,
                    &json!({}),
                    RunTrial::default(),
                    Uuid::new_v4(),
                    Some(&trace),
                )
                .await
        }
    };

    // 最新は v2 でも、渡した v1 を固定して起動する（owner・editor）。
    for ctx in [&env.alice, &env.bob] {
        let run_id = start(ctx, wf, 1).await.expect("start").expect("run");
        assert_eq!(env.run_version(run_id).await, 1);
    }
    let latest = start(&env.bob, wf, 2).await.unwrap().unwrap();
    assert_eq!(env.run_version(latest).await, 2);

    // viewer は閲覧のみ（存在は viewer で読めた後なので漏れない）。
    let err = start(&env.carol, wf, 1).await.unwrap_err();
    assert!(matches!(err, LauncherError::ReadOnly), "{err:?}");
    assert_eq!(
        err.to_string(),
        "閲覧のみのワークフローは試運転できません。画面から実行してください"
    );
    // 見えない・無いワークフロー・無い版は Ir（呼び出し側は存在を秘匿する）。
    for (ctx, id, version) in [
        (&env.dave, wf, 1),
        (&env.alice, Uuid::new_v4(), 1),
        (&env.alice, wf, 99),
    ] {
        let err = start(ctx, id, version).await.unwrap_err();
        assert!(matches!(err, LauncherError::Ir(_)), "{err:?}");
    }
    assert_eq!(env.runs_of(wf).await, 3, "断ったものは run を作らない");

    // ワークフローでない種類（汎用の artifact の口で作った・本文は IR の形）は、持ち主でも見つからない（Ir）。入口が
    // pub なので、道具の resolve_ref を通らない呼び出し側でも同じに断る。見えない人も同じ Ir。
    let other_ir = ask_ir("保存の検証を通っていない", None);
    let other = env
        .artifacts
        .create(
            &env.alice,
            NewArtifact {
                kind: ArtifactKind::Skill,
                name: format!("not-a-workflow-{}", Uuid::new_v4().simple()),
                body: other_ir,
            },
            None,
        )
        .await
        .expect("別の種類を作る")
        .id;
    for ctx in [&env.alice, &env.bob] {
        let err = start(ctx, other, 1).await.unwrap_err();
        assert!(matches!(err, LauncherError::Ir(_)), "{err:?}");
    }
    let made: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM workflow_run WHERE tenant_id = $1 AND workflow_id = $2",
    )
    .bind(&env.tenant)
    .bind(other)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(made, 0, "ワークフローでないものは run を作らない");

    // deny は viewer の 1 回だけ（見えない人は viewer の確かめで止まり、試運転の語彙では残らない）。
    let denies: Vec<AuditRow> = sqlx::query_as(
        "SELECT actor, object_type, object_id, metadata, trace_id FROM audit_log \
         WHERE tenant_id = $1 AND action = 'workflow.try_run' AND decision = 'deny'",
    )
    .bind(&env.tenant)
    .fetch_all(&env.pool)
    .await
    .unwrap();
    assert_eq!(denies.len(), 1, "{denies:?}");
    assert_eq!(denies[0].0, "carol");
    assert_eq!(denies[0].4.as_deref(), Some(trace.as_str()));

    // 許可は会話の側が承認の後に書く。対象と relation の項目は拒否と同じ。
    record_try_run_allowed(
        &AuditRecorder::new(env.pool.clone()),
        &env.bob,
        wf,
        Some(&trace),
        json!({ "chat_run_id": "c-1", "version": 1, "run_id": latest.to_string() }),
    )
    .await
    .expect("allow");
    let allow: (String, String, Value) = sqlx::query_as(
        "SELECT object_type, object_id, metadata FROM audit_log \
         WHERE tenant_id = $1 AND action = 'workflow.try_run' AND decision = 'allow'",
    )
    .bind(&env.tenant)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!((&allow.0, &allow.1), (&denies[0].1, &denies[0].2));
    assert_eq!(allow.1, wf.to_string());
    assert_eq!(allow.2["relation"], denies[0].3["relation"]);
    assert_eq!(allow.2["relation"], "editor");
    assert_eq!(allow.2["chat_run_id"], "c-1");
}

/// 試す値（きっかけの内容・予定の日時）が run に載る。入口の検証の順は手動の実行と同じ（実行の前に止める設定 →
/// 入力）で、入力の誤りは run を作らない。
#[tokio::test]
async fn trial_values_and_entry_checks() {
    run(trial_body).await;
}

async fn trial_body(env: Arc<Env>) {
    let wf = env.create_shared(&ask_ir("まとめて", None)).await;
    let trigger = json!({ "record": { "id": "r-1" } });
    let at = chrono::DateTime::parse_from_rfc3339("2026-11-02T09:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let planned = Uuid::new_v4();
    let run_id = env
        .launcher
        .start_version_as_editor(
            &env.bob,
            wf,
            1,
            &json!({ "a": 1 }),
            RunTrial {
                trigger: Some(&trigger),
                scheduled_at: Some(at),
                rerun_of: None,
            },
            planned,
            None,
        )
        .await
        .unwrap()
        .unwrap();
    // run は呼び出し側が決めた id で作る（台帳に書く前に落ちても、この id で引き直せる）。同じ id では 2 本目を作らない。
    assert_eq!(run_id, planned);
    let again = env
        .launcher
        .start_version_as_editor(
            &env.bob,
            wf,
            1,
            &json!({ "a": 1 }),
            RunTrial::default(),
            planned,
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(again, LauncherError::Run(_)), "{again:?}");
    assert_eq!(env.runs_of(wf).await, 1);
    let (payload, scheduled, input, principal): (
        Option<Value>,
        Option<chrono::DateTime<chrono::Utc>>,
        Value,
        String,
    ) = sqlx::query_as(
        "SELECT trigger_payload, scheduled_at, input, principal FROM workflow_run \
         WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(&env.tenant)
    .bind(run_id)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(payload, Some(trigger));
    assert_eq!(scheduled, Some(at));
    assert_eq!(input, json!({ "a": 1 }));
    assert_eq!(principal, "bob", "run の主体は起動した本人");

    // 空の指示（実行の前に止める）と入力の契約の両方を持つ版: 止める設定が先に返る。
    let blocked = env.create_shared(&ask_ir("", Some("amount"))).await;
    let err = env
        .launcher
        .start_version_as_editor(
            &env.bob,
            blocked,
            1,
            &json!({}),
            RunTrial::default(),
            Uuid::new_v4(),
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, LauncherError::NotReady(_)), "{err:?}");
    // 指示を埋めた版は入力の誤りで断り、run を作らない。
    let typed = env.create_shared(&ask_ir("まとめて", Some("amount"))).await;
    let err = env
        .launcher
        .start_version_as_editor(
            &env.bob,
            typed,
            1,
            &json!({}),
            RunTrial::default(),
            Uuid::new_v4(),
            None,
        )
        .await
        .unwrap_err();
    let LauncherError::Input(issues) = err else {
        panic!("入力の誤り: {err:?}");
    };
    assert_eq!(issues[0].key, "amount");
    assert_eq!(env.runs_of(blocked).await, 0);
    assert_eq!(env.runs_of(typed).await, 0);
}

/// 要約に使う IR（分岐・通らなかった道・レコードの追加・繰り返しの中の upsert・失敗するブロック）。
fn digest_ir() -> Value {
    json!({
        "ir_version": 1, "name": "digest",
        "nodes": [
            { "id": "check", "type": "control.branch", "params": {} },
            { "id": "yes", "type": "storage.read", "params": {} },
            { "id": "after", "type": "storage.read", "params": {} },
            { "id": "add", "type": "data.record.create", "params": {} },
            { "id": "m", "type": "control.map", "params": {} },
            { "id": "inner", "type": "data.record.upsert", "parent": "m", "params": {} },
            { "id": "boom", "type": "storage.read", "params": {} }
        ],
        "edges": [
            { "from": "check", "from_port": "true", "to": "yes" },
            { "from": "yes", "to": "after" },
            { "from": "check", "from_port": "false", "to": "add" },
            { "from": "add", "to": "m" },
            { "from": "m", "to": "boom" }
        ]
    })
}

/// run を直に作る（要約は保存時の検証に依らない・主体と種類を変えて「本人の run だけ」を確かめる）。
async fn create_run(
    env: &Env,
    tenant: &str,
    org: &str,
    trigger_kind: &str,
    principal: (&str, &str),
    wf: Uuid,
) -> Uuid {
    let ir_json = digest_ir();
    let ir = WorkflowIr::from_json(&ir_json).unwrap();
    env.runs
        .create_run(
            tenant,
            org,
            wf,
            3,
            trigger_kind,
            None,
            principal.0,
            principal.1,
            &json!({}),
            &ir_json,
            &RunGraph::build(&ir),
        )
        .await
        .unwrap()
        .unwrap()
}

async fn set_step(env: &Env, run_id: Uuid, path: &str, status: &str, ports: &[&str], out: Value) {
    let error = (status == "failed")
        .then(|| json!({ "code": "forbidden", "message": "書けません", "retryable": false }));
    sqlx::query(
        "INSERT INTO step_execution \
         (tenant_id, run_id, step_path, node_id, status, idempotency_key, taken_ports, output, error) \
         VALUES ($1, $2, $3, regexp_replace($3, '^.*\\.', ''), $4, $2::text || ':' || $3, $5, $6, $7) \
         ON CONFLICT (tenant_id, run_id, step_path) DO UPDATE \
         SET status = $4, taken_ports = $5, output = $6, error = $7",
    )
    .bind(&env.tenant)
    .bind(run_id)
    .bind(path)
    .bind(status)
    .bind(ports.iter().map(|p| (*p).to_string()).collect::<Vec<_>>())
    .bind((!out.is_null()).then_some(out))
    .bind(error)
    .execute(&env.pool)
    .await
    .unwrap();
}

fn record(table: Uuid, created: Option<bool>) -> Value {
    let mut r = json!({ "id": Uuid::new_v4(), "table_id": table, "rev": 1, "data": {} });
    if let Some(c) = created {
        r["created"] = json!(c);
    }
    r
}

/// 本人の run だけ・map の要素は件数に畳む・出力は先頭だけ・通らなかった理由は上流の出口から・失敗は分類と参照番号・
/// 作ったレコードは id だけ（上限つき）。
#[tokio::test]
async fn digest_is_own_run_only_and_folds_map_items() {
    run(digest_body).await;
}

async fn digest_body(env: Arc<Env>) {
    let wf = Uuid::new_v4();
    let table = Uuid::new_v4();
    let (run_id, added, items) = seed_digest_run(&env, wf, table).await;
    let ir = WorkflowIr::from_json(&digest_ir()).unwrap();
    let d = env
        .runs
        .run_digest(&env.alice, run_id, &ir)
        .await
        .unwrap()
        .expect("本人の run");
    assert_eq!((d.run_id, d.workflow_id, d.version), (run_id, wf, 3));
    assert_eq!(d.status, RunStatus::Failed);
    let failure = d.failure.clone().expect("失敗の分類");
    assert_eq!(failure.reference, run_id.simple().to_string()[..8]);
    assert_eq!(
        failure.category,
        workflow_engine::run::FailureCategory::Permission
    );
    let text = serde_json::to_string(&d).unwrap();
    assert!(
        !text.contains("raw-secret-reason"),
        "生の失敗の文を返さない"
    );
    check_digest_steps(&d);
    check_digest_items(&d, table, &added, items);
    check_digest_owner(&env, wf, run_id, &ir).await;
}

/// 要約に使う run を組む（分岐で通らなかった道・長い出力・繰り返しの中のレコード・失敗）。作ったレコードと、要素の
/// 成功の数を返す。
async fn seed_digest_run(env: &Env, wf: Uuid, table: Uuid) -> (Uuid, Value, i64) {
    let run_id = create_run(
        env,
        &env.tenant,
        "acme",
        "interactive",
        ("alice", "user"),
        wf,
    )
    .await;
    set_step(
        env,
        run_id,
        "check",
        "succeeded",
        &["false"],
        json!({ "ok": false }),
    )
    .await;
    set_step(env, run_id, "yes", "skipped", &[], Value::Null).await;
    set_step(env, run_id, "after", "skipped", &[], Value::Null).await;
    let mut added = record(table, None);
    added["data"] = json!({ "note": "あ".repeat(2000) });
    set_step(env, run_id, "add", "succeeded", &["out"], added.clone()).await;
    set_step(
        env,
        run_id,
        "m",
        "succeeded",
        &["out"],
        json!({ "items": [] }),
    )
    .await;
    let items = MAX_DIGEST_RECORDS + 2;
    for i in 0..items {
        let path = format!("m[{i}].inner");
        set_step(
            env,
            run_id,
            &path,
            "succeeded",
            &["out"],
            record(table, Some(i % 2 == 0)),
        )
        .await;
    }
    set_step(
        env,
        run_id,
        &format!("m[{items}].inner"),
        "failed",
        &[],
        Value::Null,
    )
    .await;
    set_step(env, run_id, "boom", "failed", &[], Value::Null).await;
    sqlx::query(
        "UPDATE workflow_run SET status = 'failed', fail_reason = 'raw-secret-reason' \
         WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(&env.tenant)
    .bind(run_id)
    .execute(&env.pool)
    .await
    .unwrap();

    (run_id, added, items)
}

/// 要素でない step だけ（実行の順）・出力は先頭だけ・通らなかった理由は上流の出口から。
fn check_digest_steps(d: &workflow_engine::run::RunDigest) {
    let nodes: Vec<&str> = d.steps.iter().map(|s| s.node_id.as_str()).collect();
    let mut sorted = nodes.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, vec!["add", "after", "boom", "check", "m", "yes"]);
    assert_eq!(nodes[0], "check");
    let step = |id: &str| d.steps.iter().find(|s| s.node_id == id).unwrap();
    assert_eq!(step("check").node_type, Some("control.branch"));
    assert_eq!(
        step("check").output_head.as_deref(),
        Some(r#"{"ok": false}"#)
    );
    assert!(!step("check").output_truncated);
    let add = step("add");
    let head = add.output_head.as_deref().unwrap();
    assert_eq!(
        head.chars().count(),
        usize::try_from(DIGEST_OUTPUT_CHARS).unwrap()
    );
    assert!(add.output_truncated);
    // 通らなかった理由は上流の出口から。
    let yes = &step("yes").skipped_because;
    assert_eq!(yes.len(), 1);
    assert_eq!(yes[0].upstream, "check");
    assert_eq!(yes[0].upstream_taken, vec!["false".to_string()]);
    assert!(yes[0].describe().contains("出口 false"));
    assert_eq!(
        step("after").skipped_because[0].upstream_status,
        StepStatus::Skipped
    );
    assert!(step("check").skipped_because.is_empty());
    let boom = step("boom").error.clone().unwrap();
    assert_eq!(boom.code.as_deref(), Some("forbidden"));
}

/// map の要素は件数に畳み、作ったレコードは id だけ（要素でない step が先・上限を超えた分は数だけ）。
fn check_digest_items(d: &workflow_engine::run::RunDigest, table: Uuid, added: &Value, items: i64) {
    // map の要素は件数に畳む。
    let counts: Vec<(&str, &str, StepStatus, i64)> = d
        .map_counts
        .iter()
        .map(|c| (c.map_node.as_str(), c.node_id.as_str(), c.status, c.count))
        .collect();
    assert_eq!(counts.len(), 2, "{counts:?}");
    assert!(counts.contains(&("m", "inner", StepStatus::Succeeded, items)));
    assert!(counts.contains(&("m", "inner", StepStatus::Failed, 1)));

    // 作ったレコード: 要素でない step が先・上限を超えた分は数だけ。
    assert_eq!(
        d.records.len(),
        usize::try_from(MAX_DIGEST_RECORDS).unwrap()
    );
    assert_eq!(d.records_omitted, items + 1 - MAX_DIGEST_RECORDS);
    assert_eq!(d.records[0].node_id, "add");
    assert_eq!(
        d.records[0].record_id.to_string(),
        added["id"].as_str().unwrap()
    );
    assert_eq!(d.records[0].change, RecordChange::Created);
    assert!(d.records.iter().all(|r| r.table_id == table));
    let inner: Vec<&RecordChange> = d.records[1..].iter().map(|r| &r.change).collect();
    assert!(inner.contains(&&RecordChange::Created) && inner.contains(&&RecordChange::Updated));
    assert!(d.records[1..].iter().all(|r| r.step_path.starts_with("m[")));
}

/// 本人の run でなければ None（別の人・別の組織・別のテナント・自動実行）。動いている run は失敗の分類を持たない。
async fn check_digest_owner(env: &Env, wf: Uuid, run_id: Uuid, ir: &WorkflowIr) {
    assert!(env
        .runs
        .run_digest(&env.bob, run_id, ir)
        .await
        .unwrap()
        .is_none());
    let other_org = user_ctx(&env.tenant, "beta", "alice");
    assert!(env
        .runs
        .run_digest(&other_org, run_id, ir)
        .await
        .unwrap()
        .is_none());
    let other_tenant = user_ctx(&format!("t-{}", Uuid::new_v4()), "acme", "alice");
    assert!(env
        .runs
        .run_digest(&other_tenant, run_id, ir)
        .await
        .unwrap()
        .is_none());
    let scheduled = create_run(
        env,
        &env.tenant,
        "acme",
        "schedule",
        (&wf.to_string(), "workflow"),
        wf,
    )
    .await;
    assert!(env
        .runs
        .run_digest(&env.alice, scheduled, ir)
        .await
        .unwrap()
        .is_none());
    // 本人の、まだ動いている run（失敗の分類なし・レコードなし）。
    let fresh = create_run(
        env,
        &env.tenant,
        "acme",
        "interactive",
        ("alice", "user"),
        wf,
    )
    .await;
    let d = env
        .runs
        .run_digest(&env.alice, fresh, ir)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(d.status, RunStatus::Running);
    assert!(d.failure.is_none() && d.records.is_empty() && d.map_counts.is_empty());
    assert_eq!(d.records_omitted, 0);
}

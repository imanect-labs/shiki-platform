//! 会話の AI がミニアプリを読む・作る・直す道具（miniapp_read / miniapp_create / miniapp_update・D-10 の T08）の
//! 結合テスト。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）＋実 OpenFGA（`OPENFGA_TEST_URL`）で次を確かめる。
//!
//! - 作る: 部品を名前で受け、版を省くと最新の版に固定する。持ち主は話した本人で、下書き（未公開）。観測に開く場所・
//!   画面の要約・参照の行・参照カードが付く。部品と入力の食い違い・書き損じは全件の誤りで、何も作らない。
//! - 作成の冪等（R1）と上限（1 回の run で 3 件）。見えない部品は束ねられない（名前を当てても同じ文言）。
//! - 読む: 固定した版と最新の版を並べる。持ち主でないアプリの画面は封筒（`<shared_artifact>`）で包む。
//! - 直す: 承認カードを経る。id と版で直し、名前なら id を示す。版が違えば直さない。今のピンは版を保ち、新しく
//!   束ねる部品は版の番号を書かせる。閲覧だけの人は直せない。

#![allow(
    clippy::pedantic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use agent_core::{Tool, ToolOutcome};
use artifact::{ArtifactKind, ArtifactRole, ArtifactStore, NewArtifact};
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal};
use chat::{BuilderRun, MiniappBuilderDeps, MiniappCreateTool, MiniappReadTool, MiniappUpdateTool};
use chrono::Utc;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

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
        authz::TenantId::parse(tenant).unwrap(),
    )
}

struct Env {
    artifacts: Arc<ArtifactStore>,
    deps: MiniappBuilderDeps,
    tenant: String,
}

impl Env {
    async fn new() -> Option<Self> {
        let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
            eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
            return None;
        };
        let Ok(base_url) = shiki_testenv::var("OPENFGA_TEST_URL") else {
            eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
            return None;
        };
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect(&db_url)
            .await
            .expect("Postgres へ接続できること");
        sqlx::migrate!("../../migrations")
            .run(&pool)
            .await
            .expect("マイグレーション適用");
        let fga: Arc<dyn AuthzClient> = Arc::new(
            OpenFgaClient::connect(
                reqwest::Client::new(),
                &OpenFgaConfig {
                    base_url,
                    store_name: format!("shiki-chat-miniapp-builder-{}", Uuid::new_v4()),
                },
                &authz::model::default_model(),
            )
            .await
            .expect("OpenFGA 接続"),
        );
        let artifacts = Arc::new(ArtifactStore::new(pool.clone(), fga));
        let deps = MiniappBuilderDeps {
            apps: Arc::new(gui::MiniAppStore::new(Arc::clone(&artifacts), pool)),
            artifacts: Arc::clone(&artifacts),
        };
        Some(Env {
            artifacts,
            deps,
            tenant: format!("t{}", Uuid::new_v4().simple()),
        })
    }

    fn user(&self, name: &str) -> AuthContext {
        ctx(&self.tenant, name)
    }

    fn run() -> BuilderRun {
        BuilderRun {
            since: Utc::now() - chrono::Duration::seconds(1),
        }
    }

    fn create_tool(&self) -> MiniappCreateTool {
        MiniappCreateTool::new(self.deps.clone(), Self::run())
    }

    /// 経費の申請のワークフロー（件名・金額が必須）を `who` の持ち物として作る。
    async fn expense_workflow(&self, who: &AuthContext, name: &str) -> Uuid {
        self.artifacts
            .create(
                who,
                NewArtifact {
                    kind: ArtifactKind::Workflow,
                    name: name.into(),
                    body: json!({ "ir_version": 1, "name": name, "input_schema": {
                        "type": "object", "required": ["title", "amount"],
                        "properties": {
                            "title": { "type": "string", "title": "件名" },
                            "amount": { "type": "integer", "title": "金額" }
                        } } }),
                },
                None,
            )
            .await
            .expect("workflow")
            .id
    }

    /// ワークフローに版を 1 つ積む（本文は同じ）。
    async fn bump(&self, who: &AuthContext, id: Uuid) -> i64 {
        let current = self.artifacts.get(who, id, None).await.unwrap();
        let body = self
            .artifacts
            .get_version(who, id, current.current_version, None)
            .await
            .unwrap()
            .body;
        self.artifacts
            .append_version(who, id, body, Some(current.current_version), None)
            .await
            .unwrap()
            .version
    }
}

const SCREEN: &str = "```ui\napp 経費\n## 経費の申請\n件名 = text 必須 as title\n金額 = number 必須 as amount\nbutton 申請する -> workflow 申請\n```\n";

fn create_input(name: &str, workflow: &str) -> Value {
    json!({
        "name": name,
        "description": "経費を申請する",
        "screen": SCREEN,
        "workflows": [{ "alias": "申請", "workflow": workflow }]
    })
}

fn ref_of(o: &ToolOutcome) -> Value {
    let line = o
        .content
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("参照: "))
        .unwrap_or_else(|| panic!("参照の行が無い: {}", o.content));
    serde_json::from_str(line).unwrap()
}

async fn call(tool: &dyn Tool, who: &AuthContext, input: Value) -> ToolOutcome {
    tool.call(who, input, Some("trace-miniapp-builder"))
        .await
        .expect("道具の誤りは観測で返る")
}

#[tokio::test]
async fn creates_a_draft_app_pinned_to_the_latest_versions() {
    let Some(env) = Env::new().await else { return };
    let alice = env.user("alice");
    let wf = env.expense_workflow(&alice, "expense").await;
    assert_eq!(env.bump(&alice, wf).await, 2);
    let tool = env.create_tool();
    assert!(!tool.requires_confirmation(), "作成は確認なし（HD-1 (c)）");

    let o = call(&tool, &alice, create_input("経費アプリ", "expense")).await;
    assert!(!o.is_error, "{}", o.content);
    let r = ref_of(&o);
    assert_eq!(r["kind"], "mini_app");
    assert_eq!(r["version"], 1);
    assert_eq!(o.builder_refs, std::slice::from_ref(&r));
    let id: Uuid = r["id"].as_str().unwrap().parse().unwrap();
    assert!(
        o.content.contains(&format!("開く: /apps/{id}")),
        "{}",
        o.content
    );
    assert!(o.content.contains("まだ公開していない"), "{}", o.content);
    assert!(
        o.content.contains(
            "機能「経費の申請」 入力: title=text*, amount=number* / ボタン: 申請する → 申請"
        ),
        "{}",
        o.content
    );
    // 版を省いた部品は、最新の版（v2）に固定する。持ち主は alice で、まだ公開していない。
    let meta = env.artifacts.get(&alice, id, None).await.unwrap();
    assert_eq!(
        (meta.owner.as_str(), meta.published_version),
        ("alice", None)
    );
    let body = env
        .artifacts
        .get_version(&alice, id, 1, None)
        .await
        .unwrap()
        .body;
    assert_eq!(body["workflows"][0]["artifact_id"], json!(wf));
    assert_eq!(body["workflows"][0]["version"], 2);
}

#[tokio::test]
async fn mismatched_inputs_and_broken_screens_are_all_returned_and_nothing_is_created() {
    let Some(env) = Env::new().await else { return };
    let alice = env.user("alice");
    env.expense_workflow(&alice, "expense").await;
    let tool = env.create_tool();
    let mut input = create_input("食い違い", "expense");
    // 金額が文字・必須の件名が無い・読めない行がある。
    input["screen"] = json!(
        "```ui\napp 経費\n金額 = text as amount\nこれは読めない行\nbutton 申請する -> workflow 申請\n```\n"
    );
    let o = call(&tool, &alice, input).await;
    assert!(o.is_error, "{}", o.content);
    assert!(
        o.content.contains("[miniapp.broken_block]"),
        "{}",
        o.content
    );
    // 書き損じがあれば先にそれを返す（入力の突き合わせは書き損じを直した後）。
    let mut input = create_input("食い違い", "expense");
    input["screen"] =
        json!("```ui\napp 経費\n金額 = text as amount\nbutton 申請する -> workflow 申請\n```\n");
    let o = call(&tool, &alice, input).await;
    assert!(o.is_error, "{}", o.content);
    for code in [
        "miniapp.missing_input",
        "miniapp.input_type",
        "miniapp.input_required",
    ] {
        assert!(
            o.content.contains(&format!("[{code}]")),
            "{code}: {}",
            o.content
        );
    }
    // 何も作っていない（同じ名前で作れる）。
    let o = call(&tool, &alice, create_input("食い違い", "expense")).await;
    assert!(!o.is_error, "{}", o.content);
    // 束ねていない別名のボタン。
    let mut input = create_input("別名違い", "expense");
    input["workflows"][0]["alias"] = json!("other");
    let o = call(&tool, &alice, input).await;
    assert!(
        o.content.contains("[miniapp.workflow_not_bundled]"),
        "{}",
        o.content
    );
}

#[tokio::test]
async fn creating_again_is_idempotent_capped_and_hidden_parts_are_refused() {
    let Some(env) = Env::new().await else { return };
    let alice = env.user("alice");
    let bob = env.user("bob");
    let wf = env.expense_workflow(&alice, "expense").await;
    let tool = env.create_tool();
    let first = call(&tool, &alice, create_input("経費アプリ", "expense")).await;
    assert!(!first.is_error, "{}", first.content);
    // 同じ名前・同じ内容は作成済み（版は動かさない）。
    let again = call(&tool, &alice, create_input("経費アプリ", &wf.to_string())).await;
    assert!(!again.is_error, "{}", again.content);
    assert!(
        again.content.contains("すでに作っています"),
        "{}",
        again.content
    );
    assert_eq!(ref_of(&again)["id"], ref_of(&first)["id"]);
    // 違う内容なら保存せず、直し方を言う。
    let mut differs = create_input("経費アプリ", "expense");
    differs["description"] = json!("別の説明");
    let o = call(&tool, &alice, differs).await;
    assert!(
        o.is_error && o.content.contains("miniapp_update"),
        "{}",
        o.content
    );

    // bob には alice のワークフローが見えない（名前でも id でも同じ文言）。
    for target in ["expense".to_string(), wf.to_string()] {
        let o = call(&tool, &bob, create_input("bob のアプリ", &target)).await;
        assert!(o.is_error, "{}", o.content);
        assert!(
            o.content.contains("見つからないか、見る権限がありません"),
            "{}",
            o.content
        );
    }
    // bob が同じ名前で作ろうとしても、名前が使えないとだけ言う。
    let bob_wf = env.expense_workflow(&bob, "bob-expense").await;
    let o = call(&tool, &bob, create_input("経費アプリ", &bob_wf.to_string())).await;
    assert!(
        o.is_error && o.content.contains("別の名前"),
        "{}",
        o.content
    );

    // 1 回の run で作れるのは 3 件まで（alice はもう 1 件作った）。
    for n in 2..=4 {
        let o = call(
            &tool,
            &alice,
            create_input(&format!("アプリ{n}"), "expense"),
        )
        .await;
        if n <= 3 {
            assert!(!o.is_error, "{}", o.content);
        } else {
            assert!(o.is_error && o.content.contains("3 件"), "{}", o.content);
        }
    }
}

#[tokio::test]
async fn reads_pins_with_latest_versions_and_envelopes_shared_apps() {
    let Some(env) = Env::new().await else { return };
    let alice = env.user("alice");
    let bob = env.user("bob");
    let wf = env.expense_workflow(&alice, "expense").await;
    let created = call(
        &env.create_tool(),
        &alice,
        create_input("経費アプリ", "expense"),
    )
    .await;
    let id = ref_of(&created)["id"].as_str().unwrap().to_string();
    env.bump(&alice, wf).await;

    let read = MiniappReadTool::new(env.deps.clone());
    assert!(read.is_read_only());
    let o = call(&read, &alice, json!({ "app": "経費アプリ" })).await;
    assert!(!o.is_error, "{}", o.content);
    assert!(
        o.content
            .contains(&format!("id: {id}, v1・まだ公開していません")),
        "{}",
        o.content
    );
    assert!(
        o.content.contains("v1 を固定中・最新は v2"),
        "{}",
        o.content
    );
    assert!(!o.content.contains("<shared_artifact>"), "{}", o.content);
    assert!(
        o.content.contains("button 申請する -> workflow 申請"),
        "{}",
        o.content
    );

    // 見えない人には、無いのと同じ文言。
    let hidden = call(&read, &bob, json!({ "app": id })).await;
    assert!(
        hidden.is_error && hidden.content.contains("見つからないか"),
        "{}",
        hidden.content
    );
    // 共同編集者として共有すると読めるが、ほかの人が書いた画面は封筒で包む。ワークフローは読めない。
    env.artifacts
        .share(
            &alice,
            id.parse().unwrap(),
            &storage::ShareTarget::User { id: "bob".into() },
            ArtifactRole::Editor,
            None,
        )
        .await
        .unwrap();
    let shared = call(&read, &bob, json!({ "app": id })).await;
    assert!(!shared.is_error, "{}", shared.content);
    assert!(
        shared.content.contains("<shared_artifact>"),
        "{}",
        shared.content
    );
    assert!(
        shared.content.contains("あなたには読めない部品"),
        "{}",
        shared.content
    );
}

#[tokio::test]
async fn updates_need_the_id_and_version_and_keep_or_bump_pins() {
    let Some(env) = Env::new().await else { return };
    let alice = env.user("alice");
    let bob = env.user("bob");
    let wf = env.expense_workflow(&alice, "expense").await;
    let notify = env.expense_workflow(&alice, "notify").await;
    let created = call(
        &env.create_tool(),
        &alice,
        create_input("経費アプリ", "expense"),
    )
    .await;
    let id = ref_of(&created)["id"].as_str().unwrap().to_string();
    env.bump(&alice, wf).await;
    let update = MiniappUpdateTool::new(env.deps.clone());
    assert!(update.requires_confirmation(), "改訂は承認カードを経る");

    // 名前なら直さず id を示す。
    let o = call(
        &update,
        &alice,
        json!({ "app": "経費アプリ", "expected_version": 1, "description": "x" }),
    )
    .await;
    assert!(o.is_error && o.content.contains(&id), "{}", o.content);
    // 版が違えば直さない。
    let o = call(
        &update,
        &alice,
        json!({ "app": id, "expected_version": 5, "description": "x" }),
    )
    .await;
    assert!(
        o.is_error && o.content.contains("v1 になっています"),
        "{}",
        o.content
    );
    // 新しく束ねる部品は版の番号が要る。latest は受けない。
    let pins = |extra: Value| json!([{ "alias": "申請", "workflow": "expense" }, extra]);
    let o = call(
        &update,
        &alice,
        json!({ "app": id, "expected_version": 1, "workflows": pins(json!({ "alias": "通知", "workflow": "notify" })) }),
    )
    .await;
    assert!(
        o.is_error && o.content.contains("version（版の番号）を書いて"),
        "{}",
        o.content
    );
    let o = call(
        &update,
        &alice,
        json!({ "app": id, "expected_version": 1, "workflows": pins(json!({ "alias": "通知", "workflow": "notify", "version": "latest" })) }),
    )
    .await;
    assert!(o.is_error && o.content.contains("整数"), "{}", o.content);

    // 今のピンは版を省くと今のまま。版を上げ、新しい部品を版つきで束ねる。
    let o = call(
        &update,
        &alice,
        json!({ "app": id, "expected_version": 1, "workflows": [
            { "alias": "申請", "workflow": "expense", "version": 2 },
            { "alias": "通知", "workflow": notify.to_string(), "version": 1 }
        ] }),
    )
    .await;
    assert!(!o.is_error, "{}", o.content);
    assert_eq!(ref_of(&o)["version"], 2);
    assert!(
        o.content.contains("ワークフロー「申請」を v1 → v2"),
        "{}",
        o.content
    );
    assert!(
        o.content.contains("ワークフロー「通知」を束ねた（v1）"),
        "{}",
        o.content
    );
    // 画面だけを変えると、ピンはそのまま。
    let screen = SCREEN.replace("申請する", "送る");
    let o = call(
        &update,
        &alice,
        json!({ "app": id, "expected_version": 2, "screen": screen }),
    )
    .await;
    assert!(!o.is_error, "{}", o.content);
    let body = env
        .artifacts
        .get_version(&alice, id.parse().unwrap(), 3, None)
        .await
        .unwrap()
        .body;
    assert_eq!(body["workflows"][0]["version"], 2);
    assert_eq!(body["workflows"].as_array().unwrap().len(), 2);
    // 同じ内容は保存しない。
    let o = call(
        &update,
        &alice,
        json!({ "app": id, "expected_version": 3, "screen": screen }),
    )
    .await;
    assert!(
        o.is_error && o.content.contains("同じ内容"),
        "{}",
        o.content
    );

    // 閲覧だけ共有された人は直せない。
    env.artifacts
        .share(
            &alice,
            id.parse().unwrap(),
            &storage::ShareTarget::User { id: "bob".into() },
            ArtifactRole::Viewer,
            None,
        )
        .await
        .unwrap();
    let o = call(
        &update,
        &bob,
        json!({ "app": id, "expected_version": 3, "description": "x" }),
    )
    .await;
    assert!(o.is_error, "{}", o.content);
    assert!(
        o.content.contains("編集する権限がありません") || o.content.contains("見つからないか"),
        "{}",
        o.content
    );
}

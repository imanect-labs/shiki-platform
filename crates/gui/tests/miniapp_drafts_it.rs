//! ミニアプリの下書きと公開・共同編集者の保存・束ねたスキルの本文・画面の書き損じの結合テスト（D-16・GAP-36・
//! BUG-79・BUG-80）。
//!
//! - 実 Postgres（`STORAGE_TEST_DATABASE_URL`）＋実 OpenFGA（`OPENFGA_TEST_URL`）: 使う人（viewer）には公開した版
//!   だけを出し、editor は下書きを試せる。公開・公開を戻す・公開の権限・開いた後に公開した版が変わったボタン。
//!   共同編集者は、自分が読めない部品を束ねたアプリを、その部品に触れなければ保存できる。
//! - 実 Postgres だけ: 束ねたスキルの本文を保存のときに確かめる。画面の書き損じは下書きには注意つきで保存でき、
//!   公開と AI の経路では止める。
//!
//! テストで作った行（テナントごと）と OpenFGA のストアは、落ちても最後に消す。

#![allow(
    clippy::pedantic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

mod common;

use std::sync::Arc;

use artifact::{ArtifactError, ArtifactKind, ArtifactRole, ArtifactStore, NewArtifact};
use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use gui::{
    ActionDispatcher, ActionError, GuiError, MiniAppActionRequest, MiniAppChange, MiniAppStore,
    SaveMode, SkillStore, WorkflowStarter,
};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

async fn pool() -> Option<PgPool> {
    let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("Postgres へ接続できること");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("マイグレーション適用");
    Some(pool)
}

/// テストごとの OpenFGA のストア（最後に消す）。
struct Fga {
    client: Arc<dyn AuthzClient>,
    base_url: String,
    store_id: String,
}

impl Fga {
    async fn connect() -> Option<Fga> {
        let Ok(base_url) = shiki_testenv::var("OPENFGA_TEST_URL") else {
            eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
            return None;
        };
        use authz::client::{OpenFgaClient, OpenFgaConfig};
        let client = OpenFgaClient::connect(
            reqwest::Client::new(),
            &OpenFgaConfig {
                base_url: base_url.clone(),
                store_name: format!("shiki-miniapp-drafts-{}", Uuid::new_v4()),
            },
            &authz::model::default_model(),
        )
        .await
        .expect("OpenFGA 接続");
        let store_id = client.store_id().to_string();
        Some(Fga {
            client: Arc::new(client),
            base_url,
            store_id,
        })
    }

    async fn delete_store(&self) {
        let resp = reqwest::Client::new()
            .delete(format!("{}/stores/{}", self.base_url, self.store_id))
            .send()
            .await
            .expect("OpenFGA delete store");
        assert!(
            resp.status().is_success(),
            "delete store: {}",
            resp.status()
        );
    }
}

/// 全許可（DB 面のテストで OpenFGA を不要にする）。
struct AllowAll;

#[async_trait]
impl AuthzClient for AllowAll {
    async fn check(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: &FgaObject,
        _c: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn write_tuple(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn delete_tuple(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn read_tuples(
        &self,
        _o: &FgaObject,
        _r: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        Ok(vec![])
    }
    async fn list_objects(
        &self,
        _s: &Subject,
        _r: Relation,
        _t: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
    async fn delete_object_tuples(&self, _o: &FgaObject) -> Result<u32, AuthzError> {
        Ok(0)
    }
    async fn read_subject_objects(
        &self,
        _s: &Subject,
        _t: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
}

/// 起動口の偽物（呼ばれた回数を数える）。
#[derive(Default)]
struct CountingStarter(std::sync::atomic::AtomicUsize);

#[async_trait]
impl WorkflowStarter for CountingStarter {
    async fn start_pinned(
        &self,
        _ctx: &AuthContext,
        _workflow_id: Uuid,
        _version: i64,
        _input: &serde_json::Value,
    ) -> Result<Option<Uuid>, ActionError> {
        unreachable!("ミニアプリはバンドル権限で起動する")
    }

    async fn start_pinned_via_bundle(
        &self,
        _ctx: &AuthContext,
        _bundle_id: Uuid,
        _workflow_id: Uuid,
        _version: i64,
        _input: &serde_json::Value,
    ) -> Result<Option<Uuid>, ActionError> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(Some(Uuid::new_v4()))
    }
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
        authz::TenantId::parse(tenant).unwrap(),
    )
}

struct Stores {
    artifacts: Arc<ArtifactStore>,
    skills: SkillStore,
    mini_apps: MiniAppStore,
}

fn stores(pool: PgPool, authz: Arc<dyn AuthzClient>) -> Stores {
    let artifacts = Arc::new(ArtifactStore::new(pool.clone(), authz));
    Stores {
        skills: SkillStore::new(Arc::clone(&artifacts)),
        mini_apps: MiniAppStore::new(Arc::clone(&artifacts), pool),
        artifacts,
    }
}

async fn workflow(s: &Stores, c: &AuthContext, name: &str) -> Uuid {
    s.artifacts
        .create(
            c,
            NewArtifact {
                kind: ArtifactKind::Workflow,
                name: name.into(),
                body: json!({ "rev": 1 }),
            },
            None,
        )
        .await
        .expect("wf")
        .id
}

/// `expense` ワークフローを起動するボタンの画面（見出しで版を見分ける）。
fn screen(title: &str) -> String {
    format!(
        "# {title}\n\n```ui\napp 経費の申請\n金額 = number 必須 円 as amount\n\
         button 申請する -> workflow expense\n```\n"
    )
}

fn app_body(title: &str, wf: Uuid, wf_version: i64) -> serde_json::Value {
    json!({
        "description": title,
        "screen": screen(title),
        "workflows": [ { "alias": "expense", "artifact_id": wf, "version": wf_version } ]
    })
}

async fn share(s: &Stores, owner: &AuthContext, app: Uuid, who: &str, role: ArtifactRole) {
    s.artifacts
        .share(
            owner,
            app,
            &storage::ShareTarget::User { id: who.into() },
            role,
            None,
        )
        .await
        .expect("share");
}

/// 公開した版のほかを開いた・押した（使う人）ことを表す誤りか。
fn not_published(r: &Result<impl std::fmt::Debug, GuiError>, published: Option<i64>) -> bool {
    matches!(
        r,
        Err(GuiError::Artifact(ArtifactError::NotPublished { published: p })) if *p == published
    )
}

/// 本体を走らせ、落ちても後片付け（テナントの行・OpenFGA のストア）をしてから結果を返す。
async fn run_with_cleanup<F>(pool: PgPool, tenant: String, fga: Option<Fga>, body: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let outcome = tokio::spawn(body).await;
    common::purge_tenant_rows(&pool, &tenant).await;
    if let Some(fga) = fga {
        fga.delete_store().await;
    }
    if let Err(e) = outcome {
        std::panic::resume_unwind(e.into_panic());
    }
}

/// 使う人（viewer）には公開した版だけを出し、editor は下書きを試せる。公開・公開を戻す・公開の権限（D-16 の (a)）。
#[tokio::test]
async fn viewers_get_only_the_published_version() {
    let Some(pool) = pool().await else { return };
    let Some(fga) = Fga::connect().await else {
        return;
    };
    let s = stores(pool.clone(), Arc::clone(&fga.client));
    let tenant = format!("t-{}", Uuid::new_v4());
    let t = tenant.clone();
    let db = pool.clone();
    run_with_cleanup(pool, tenant, Some(fga), async move {
        let alice = ctx(&t, "alice");
        let bob = ctx(&t, "bob");
        let carol = ctx(&t, "carol");
        let wf = workflow(&s, &alice, "expense-wf").await;
        let saved = s
            .mini_apps
            .create(
                &alice,
                "draft-app",
                &app_body("v1", wf, 1),
                SaveMode::Draft,
                None,
            )
            .await
            .expect("create");
        assert_eq!(
            saved.published_version, None,
            "新しいアプリはまだ公開していない"
        );
        let app = saved.id;
        share(&s, &alice, app, "bob", ArtifactRole::Viewer).await;
        share(&s, &alice, app, "carol", ArtifactRole::Editor).await;

        // まだ公開していない: 使う人は開けない。editor は最新の版（下書き）を開ける。
        assert!(not_published(
            &s.mini_apps.resolve(&bob, app, None, None).await,
            None
        ));
        let draft = s
            .mini_apps
            .resolve(&carol, app, None, None)
            .await
            .expect("carol");
        assert_eq!((draft.version, draft.published_version), (1, None));
        assert!(s
            .artifacts
            .list_versions(&bob, app, None)
            .await
            .expect("bob versions")
            .is_empty());

        // 使う人は公開できない（editor の操作）。
        assert!(matches!(
            s.mini_apps.publish(&bob, app, 1, None).await,
            Err(GuiError::Artifact(ArtifactError::Forbidden))
        ));
        let p1 = s
            .mini_apps
            .publish(&alice, app, 1, None)
            .await
            .expect("publish v1");
        assert_eq!(
            (p1.published_version, p1.previous_published_version),
            (1, None)
        );

        // 保存は下書きの版になり、使う人の画面は変わらない。
        let v2 = s
            .mini_apps
            .update(
                &alice,
                app,
                &app_body("v2", wf, 1),
                Some(1),
                SaveMode::Draft,
                None,
            )
            .await
            .expect("v2");
        assert_eq!((v2.version, v2.published_version), (2, Some(1)));
        let seen = s
            .mini_apps
            .resolve(&bob, app, None, None)
            .await
            .expect("bob");
        assert_eq!(seen.version, 1);
        assert!(seen.body.screen.starts_with("# v1"));
        assert!(not_published(
            &s.mini_apps.resolve(&bob, app, Some(2), None).await,
            Some(1)
        ));
        // 汎用の版の口も、使う人には公開した版だけを出す（実在しない版も同じ答え）。
        for v in [2, 99] {
            assert!(matches!(
                s.artifacts.get_version(&bob, app, v, None).await,
                Err(ArtifactError::NotPublished { published: Some(1) })
            ));
        }
        let listed: Vec<i64> = s
            .artifacts
            .list_versions(&bob, app, None)
            .await
            .unwrap()
            .iter()
            .map(|v| v.version)
            .collect();
        assert_eq!(listed, vec![1]);
        // editor（共有で編集にした人）は下書きを試せ、版の一覧もすべて読める。
        let tried = s
            .mini_apps
            .resolve(&carol, app, Some(2), None)
            .await
            .expect("carol v2");
        assert!(tried.body.screen.starts_with("# v2"));
        assert_eq!(
            (tried.published_version, tried.latest_version),
            (Some(1), 2)
        );
        assert_eq!(
            s.artifacts
                .list_versions(&carol, app, None)
                .await
                .unwrap()
                .len(),
            2
        );

        // ボタン: 使う人が押せるのは公開した版だけ。下書きの版のボタンは、開き直しを促す誤り。
        let starter = Arc::new(CountingStarter::default());
        let mut dispatcher = ActionDispatcher::new(storage::audit::AuditRecorder::new(db.clone()));
        dispatcher.set_workflow_starter(starter.clone());
        let block = |title: &str| yui::extract_action_blocks(&screen(title)).remove(0).hash;
        let press = |version: i64, title: &str, key: &str| MiniAppActionRequest {
            version,
            action_id: format!("{}:a1", block(title)),
            params: json!({ "amount": 100 }).as_object().cloned().unwrap(),
            idempotency_key: Some(key.into()),
        };
        let draft_press = s
            .mini_apps
            .invoke_yui_action(
                &bob,
                &dispatcher,
                app,
                press(2, "v2", "draft-press-0001"),
                None,
            )
            .await;
        assert!(
            matches!(
                draft_press,
                Err(ActionError::AppVersionChanged { published: Some(1) })
            ),
            "{draft_press:?}"
        );
        s.mini_apps
            .invoke_yui_action(
                &bob,
                &dispatcher,
                app,
                press(1, "v1", "pub-press-0001"),
                None,
            )
            .await
            .expect("公開した版のボタン");
        s.mini_apps
            .invoke_yui_action(
                &carol,
                &dispatcher,
                app,
                press(2, "v2", "try-press-0001"),
                None,
            )
            .await
            .expect("editor は下書きのボタンを試せる");
        assert_eq!(starter.0.load(std::sync::atomic::Ordering::SeqCst), 2);

        // 公開すると使う人の画面が変わり、開いたままの前の版のボタンは開き直しを促す。
        let preview = s
            .mini_apps
            .publish_preview(&carol, app, Some(2), None)
            .await
            .unwrap();
        assert_eq!(preview.published_version, Some(1));
        assert!(preview.problems.is_empty());
        assert!(preview.changes.contains(&MiniAppChange::Screen));
        let p2 = s
            .mini_apps
            .publish(&carol, app, 2, None)
            .await
            .expect("carol publishes");
        assert_eq!(
            (p2.published_version, p2.previous_published_version),
            (2, Some(1))
        );
        let seen = s.mini_apps.resolve(&bob, app, None, None).await.unwrap();
        assert_eq!(seen.version, 2);
        let stale = s
            .mini_apps
            .invoke_yui_action(
                &bob,
                &dispatcher,
                app,
                press(1, "v1", "stale-press-0001"),
                None,
            )
            .await;
        assert!(
            matches!(
                stale,
                Err(ActionError::AppVersionChanged { published: Some(2) })
            ),
            "{stale:?}"
        );

        // 公開を戻す（前に公開していた版を公開し直す）。いまの版を公開し直しても前に公開していた版を潰さない。
        let back = s
            .mini_apps
            .publish(&alice, app, 1, None)
            .await
            .expect("roll back");
        assert_eq!(
            (back.published_version, back.previous_published_version),
            (1, Some(2))
        );
        let again = s.mini_apps.publish(&alice, app, 1, None).await.unwrap();
        assert_eq!(again, back);
        assert!(s
            .mini_apps
            .resolve(&bob, app, None, None)
            .await
            .unwrap()
            .body
            .screen
            .starts_with("# v1"));
        // 無い版は公開できない。
        assert!(matches!(
            s.mini_apps.publish(&alice, app, 9, None).await,
            Err(GuiError::Artifact(ArtifactError::NotFound))
        ));
        // 公開は監査に残る（誰が・どの版を・前の版）。
        let audited: Vec<serde_json::Value> = sqlx::query_scalar(
            "SELECT metadata FROM audit_log WHERE tenant_id = $1 AND action = 'artifact.publish' \
             AND decision = 'allow' ORDER BY id",
        )
        .bind(&t)
        .fetch_all(&db)
        .await
        .unwrap();
        assert_eq!(
            audited,
            vec![
                json!({ "version": 1, "previous_version": null }),
                json!({ "version": 2, "previous_version": 1 }),
                json!({ "version": 1, "previous_version": 2 }),
            ]
        );
    })
    .await;
}

/// 共同編集者は、自分が読めない部品を束ねたアプリを、その部品に触れなければ保存できる。足した部品・版を変えた
/// 部品は、保存する人の権限で確かめる（D-16 の (d)・BUG-80）。
#[tokio::test]
async fn co_editors_keep_components_they_cannot_read() {
    let Some(pool) = pool().await else { return };
    let Some(fga) = Fga::connect().await else {
        return;
    };
    let s = stores(pool.clone(), Arc::clone(&fga.client));
    let tenant = format!("t-{}", Uuid::new_v4());
    let t = tenant.clone();
    run_with_cleanup(pool, tenant, Some(fga), async move {
        let alice = ctx(&t, "alice");
        let carol = ctx(&t, "carol");
        // alice のワークフロー（carol には共有しない）を束ねたアプリを、carol に編集で共有する。
        let wf = workflow(&s, &alice, "secret-wf").await;
        s.artifacts
            .append_version(&alice, wf, json!({ "rev": 2 }), Some(1), None)
            .await
            .expect("wf v2");
        let app = s
            .mini_apps
            .create(&alice, "shared-app", &app_body("v1", wf, 1), SaveMode::Draft, None)
            .await
            .expect("create")
            .id;
        share(&s, &alice, app, "carol", ArtifactRole::Editor).await;

        // ワークフローに触れずに画面だけを直す: 保存できる（前は「見つからないか、あなたには読めません」）。
        let mut edited = app_body("v1", wf, 1);
        edited["description"] = json!("経費の申請（直した）");
        let saved = s
            .mini_apps
            .update(&carol, app, &edited, Some(1), SaveMode::Draft, None)
            .await
            .expect("触れない部品は確かめ直さない");
        assert_eq!(saved.version, 2);
        // 別名だけを変えても、同じ部品の同じ版なので確かめ直さない。
        let mut renamed = edited.clone();
        renamed["workflows"][0]["alias"] = json!("経費");
        renamed["screen"] = json!(screen("v1").replace("workflow expense", "workflow 経費"));
        s.mini_apps
            .update(&carol, app, &renamed, Some(2), SaveMode::Draft, None)
            .await
            .expect("別名の変更");

        // 版を変えると、carol の権限で確かめる（読めないので断る・文言は別名と版で言う）。
        let mut bumped = renamed.clone();
        bumped["workflows"][0]["version"] = json!(2);
        let err = s
            .mini_apps
            .update(&carol, app, &bumped, Some(3), SaveMode::Draft, None)
            .await
            .expect_err("版を変えたピンは確かめる");
        let GuiError::Validation(errors) = err else {
            panic!("validation error expected");
        };
        assert_eq!(
            errors
                .iter()
                .map(|e| (e.code.as_str(), e.message.as_str(), e.path.as_deref()))
                .collect::<Vec<_>>(),
            vec![(
                "miniapp.pin_unresolved",
                "ワークフロー「経費」（v2）が見つからないか、あなたには読めません",
                Some("workflows[0]")
            )]
        );
        // 足した部品も確かめる（carol が読めるワークフローなら足せる）。
        let own = workflow(&s, &carol, "carol-wf").await;
        let mut added = renamed.clone();
        added["workflows"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "alias": "control", "artifact_id": own, "version": 1 }));
        s.mini_apps
            .update(&carol, app, &added, Some(3), SaveMode::Draft, None)
            .await
            .expect("読める部品は足せる");
        let mut foreign = added.clone();
        let other = workflow(&s, &alice, "other-wf").await;
        foreign["workflows"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "alias": "other", "artifact_id": other, "version": 1 }));
        assert!(matches!(
            s.mini_apps.update(&carol, app, &foreign, Some(4), SaveMode::Draft, None).await,
            Err(GuiError::Validation(e)) if e.iter().any(|x| x.path.as_deref() == Some("workflows[2]"))
        ));
        // 使う人（viewer）は保存できない（前の版を読む前に断る）。
        let dave = ctx(&t, "dave");
        share(&s, &alice, app, "dave", ArtifactRole::Viewer).await;
        assert!(matches!(
            s.mini_apps.update(&dave, app, &added, Some(4), SaveMode::Draft, None).await,
            Err(GuiError::Artifact(ArtifactError::Forbidden))
        ));
    })
    .await;
}

/// 画面の書き損じは、下書きには注意つきで保存でき、公開と AI の経路では止める（D-16 の (c)）。
#[tokio::test]
async fn broken_figures_are_saved_as_drafts_but_not_published() {
    let Some(pool) = pool().await else { return };
    let s = stores(pool.clone(), Arc::new(AllowAll));
    let tenant = format!("t-{}", Uuid::new_v4());
    let t = tenant.clone();
    run_with_cleanup(pool, tenant, None, async move {
        let alice = ctx(&t, "alice");
        let wf = workflow(&s, &alice, "expense-wf").await;
        let app = s
            .mini_apps
            .create(
                &alice,
                "figure-app",
                &app_body("経費", wf, 1),
                SaveMode::Draft,
                None,
            )
            .await
            .expect("create");
        assert!(app.warnings.is_empty());
        let app = app.id;

        // 書き損じた図（種類の誤りで入力の行が落ちる）: 下書きには注意つきで保存できる。
        let mut broken = app_body("経費", wf, 1);
        broken["screen"] = json!(screen("経費").replace("number 必須 円", "numbr 必須 円"));
        let saved = s
            .mini_apps
            .update(&alice, app, &broken, Some(1), SaveMode::Draft, None)
            .await
            .expect("下書きには保存できる");
        assert_eq!(saved.warnings.len(), 1, "{:?}", saved.warnings);
        assert_eq!(saved.warnings[0].code, "miniapp.broken_block");
        assert!(saved.warnings[0]
            .message
            .starts_with("画面の 5 行目（図「経費の申請」）: "));
        // 公開の前に止める（公開の前の確かめにも同じ誤りが出る）。
        let preview = s
            .mini_apps
            .publish_preview(&alice, app, Some(2), None)
            .await
            .unwrap();
        assert_eq!(preview.problems, saved.warnings);
        assert!(matches!(
            s.mini_apps.publish(&alice, app, 2, None).await,
            Err(GuiError::Validation(e)) if e == saved.warnings
        ));
        // 書き損じの無い版は公開できる。
        s.mini_apps
            .publish(&alice, app, 1, None)
            .await
            .expect("v1 は公開できる");
        // AI の経路（Strict）は保存でも止める。
        assert!(matches!(
            s.mini_apps.create(&alice, "ai-app", &broken, SaveMode::Strict, None).await,
            Err(GuiError::Validation(e)) if e.iter().all(|x| x.code == "miniapp.broken_block")
        ));
        assert!(matches!(
            s.mini_apps
                .update(&alice, app, &broken, Some(2), SaveMode::Strict, None)
                .await,
            Err(GuiError::Validation(_))
        ));
    })
    .await;
}

/// 書き損じで止めるのは一度も公開していない版だけ。前に公開していた版（migration 0149 が書き損じごと公開した版にした
/// ものを含む）へは、書き損じがあっても公開を戻せる（新しい版に誤りがあったときの逃げ道を塞がない）。
#[tokio::test]
async fn rolling_back_to_a_broken_published_version_is_allowed() {
    let Some(pool) = pool().await else { return };
    let s = stores(pool.clone(), Arc::new(AllowAll));
    let tenant = format!("t-{}", Uuid::new_v4());
    let t = tenant.clone();
    let db = pool.clone();
    run_with_cleanup(pool, tenant, None, async move {
        let alice = ctx(&t, "alice");
        let wf = workflow(&s, &alice, "expense-wf").await;
        let mut broken = app_body("経費", wf, 1);
        broken["screen"] = json!(screen("経費").replace("number 必須 円", "numbr 必須 円"));
        let app = s
            .mini_apps
            .create(&alice, "rollback-app", &broken, SaveMode::Draft, None)
            .await
            .expect("create");
        assert_eq!(app.warnings.len(), 1, "{:?}", app.warnings);
        let app = app.id;
        // migration 0149 と同じく、書き損じのある v1 をそのまま公開した版にする。
        sqlx::query(
            "UPDATE artifact SET published_version = 1, published_at = now() WHERE id = $1",
        )
        .bind(app)
        .execute(&db)
        .await
        .unwrap();
        // 書き損じの無い v2 を公開する（v1 が前に公開していた版になる）。
        s.mini_apps
            .update(
                &alice,
                app,
                &app_body("経費", wf, 1),
                Some(1),
                SaveMode::Draft,
                None,
            )
            .await
            .expect("v2");
        let p2 = s
            .mini_apps
            .publish(&alice, app, 2, None)
            .await
            .expect("v2 を公開");
        assert_eq!(p2.previous_published_version, Some(1));

        // v1 へは公開を戻せる（公開の前の確かめも止める誤りを言わない）。
        let preview = s
            .mini_apps
            .publish_preview(&alice, app, Some(1), None)
            .await
            .unwrap();
        assert!(preview.problems.is_empty(), "{:?}", preview.problems);
        let back = s
            .mini_apps
            .publish(&alice, app, 1, None)
            .await
            .expect("書き損じがあっても公開を戻せる");
        assert_eq!(
            (back.published_version, back.previous_published_version),
            (1, Some(2))
        );
        // いま公開している v1 を公開し直しても止めない（何も変えない）。
        s.mini_apps
            .publish(&alice, app, 1, None)
            .await
            .expect("いまの版の公開し直し");

        // 一度も公開していない、書き損じのある v3 は止める。
        s.mini_apps
            .update(&alice, app, &broken, Some(2), SaveMode::Draft, None)
            .await
            .expect("v3");
        let preview = s
            .mini_apps
            .publish_preview(&alice, app, Some(3), None)
            .await
            .unwrap();
        assert_eq!(preview.problems.len(), 1);
        assert!(matches!(
            s.mini_apps.publish(&alice, app, 3, None).await,
            Err(GuiError::Validation(e)) if e.iter().all(|x| x.code == "miniapp.broken_block")
        ));
    })
    .await;
}

/// 束ねたスキルの本文を保存のときに確かめる（開いた途端に使えないと出るアプリを保存させない・BUG-79）。
#[tokio::test]
async fn saving_checks_the_bundled_skill_body() {
    let Some(pool) = pool().await else { return };
    let s = stores(pool.clone(), Arc::new(AllowAll));
    let tenant = format!("t-{}", Uuid::new_v4());
    let t = tenant.clone();
    run_with_cleanup(pool, tenant, None, async move {
        let alice = ctx(&t, "alice");
        let wf = workflow(&s, &alice, "expense-wf").await;
        let broken_skill = s
            .artifacts
            .create(
                &alice,
                NewArtifact {
                    kind: ArtifactKind::Skill,
                    name: "old-format".into(),
                    body: json!({ "description": "古い形式" }),
                },
                None,
            )
            .await
            .expect("壊れた skill")
            .id;
        let mut with_skill = app_body("経費", wf, 1);
        with_skill["skill"] = json!({ "artifact_id": broken_skill, "version": 1 });
        let err = s
            .mini_apps
            .create(&alice, "skill-app", &with_skill, SaveMode::Draft, None)
            .await
            .expect_err("壊れたスキル");
        let GuiError::Validation(errors) = err else {
            panic!("validation error expected");
        };
        assert_eq!(
            errors
                .iter()
                .map(|e| (e.code.as_str(), e.message.as_str(), e.path.as_deref()))
                .collect::<Vec<_>>(),
            vec![(
                "miniapp.skill_broken",
                "スキル（v1）の本文を読めません（古い形式か、壊れています）",
                Some("skill")
            )]
        );
        // 読めるスキルなら束ねられ、あとから壊れたスキルに替える保存も断る。
        let (good_skill, _) = s
            .skills
            .create(
                &alice,
                "good-skill",
                &json!({ "description": "経費の案内", "instructions": "経費の質問に答える。" }),
                None,
            )
            .await
            .expect("skill");
        let mut good = with_skill.clone();
        good["skill"] = json!({ "artifact_id": good_skill, "version": 1 });
        let app = s
            .mini_apps
            .create(&alice, "skill-app", &good, SaveMode::Draft, None)
            .await
            .expect("読めるスキル")
            .id;
        assert!(matches!(
            s.mini_apps.update(&alice, app, &with_skill, Some(1), SaveMode::Draft, None).await,
            Err(GuiError::Validation(e)) if e.iter().any(|x| x.code == "miniapp.skill_broken")
        ));
    })
    .await;
}

/// 画面のボタンは押すたびにミニアプリの解決（`miniapp.resolve`・束ねた skill のバンドル読み）をやり直さない
/// （BUG-77）。開いたときの解決が 1 回残り、押すたびに残るのは操作（`ui_action.invoke`）だけ。本体の viewer は
/// 押すたびに確かめ直すので、共有を外した直後の押下から起動しない。
#[tokio::test]
async fn pressing_buttons_does_not_resolve_the_app_again() {
    let Some(pool) = pool().await else { return };
    let Some(fga) = Fga::connect().await else {
        return;
    };
    let s = stores(pool.clone(), Arc::clone(&fga.client));
    let tenant = format!("t-{}", Uuid::new_v4());
    let t = tenant.clone();
    let db = pool.clone();
    run_with_cleanup(pool, tenant, Some(fga), async move {
        let alice = ctx(&t, "alice");
        let bob = ctx(&t, "bob");
        let wf = workflow(&s, &alice, "expense-wf").await;
        let (skill, _) = s
            .skills
            .create(
                &alice,
                "expense-guide",
                &json!({ "description": "経費の案内", "instructions": "経費の質問に答える。" }),
                None,
            )
            .await
            .expect("skill");
        let mut body = app_body("v1", wf, 1);
        body["skill"] = json!({ "artifact_id": skill, "version": 1 });
        let app = s
            .mini_apps
            .create(&alice, "press-app", &body, SaveMode::Draft, None)
            .await
            .expect("create")
            .id;
        s.mini_apps
            .publish(&alice, app, 1, None)
            .await
            .expect("publish");
        share(&s, &alice, app, "bob", ArtifactRole::Viewer).await;

        // bob が開く（解決は 1 回）。
        s.mini_apps
            .resolve(&bob, app, None, None)
            .await
            .expect("bob opens");
        let starter = Arc::new(CountingStarter::default());
        let mut dispatcher = ActionDispatcher::new(storage::audit::AuditRecorder::new(db.clone()));
        dispatcher.set_workflow_starter(starter.clone());
        let action_id = format!(
            "{}:a1",
            yui::extract_action_blocks(&screen("v1")).remove(0).hash
        );
        let press = |key: String| MiniAppActionRequest {
            version: 1,
            action_id: action_id.clone(),
            params: json!({ "amount": 100 }).as_object().cloned().unwrap(),
            idempotency_key: Some(key),
        };
        for i in 0..3 {
            s.mini_apps
                .invoke_yui_action(&bob, &dispatcher, app, press(format!("press-{i:04}")), None)
                .await
                .expect("press");
        }
        assert_eq!(starter.0.load(std::sync::atomic::Ordering::SeqCst), 3);
        let count = |action: &'static str, object: Option<Uuid>| {
            let db = db.clone();
            let t = t.clone();
            async move {
                sqlx::query_scalar::<_, i64>(
                    "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND actor = 'bob' \
                       AND action = $2 AND ($3::text IS NULL OR object_id = $3)",
                )
                .bind(&t)
                .bind(action)
                .bind(object.map(|id| id.to_string()))
                .fetch_one(&db)
                .await
                .expect("audit")
            }
        };
        assert_eq!(
            count("miniapp.resolve", None).await,
            1,
            "解決は開いたときの 1 回だけ（押すたびに重ねない）"
        );
        assert_eq!(
            count("artifact.read_via_bundle", Some(skill)).await,
            1,
            "ボタンは skill を読まない"
        );
        assert_eq!(count("ui_action.invoke", None).await, 3, "操作は 1 回ごと");

        // 共有を外すと、直後の押下から起動しない（権限は押すたびに確かめ直す）。
        s.artifacts
            .unshare(
                &alice,
                app,
                &storage::ShareTarget::User { id: "bob".into() },
                ArtifactRole::Viewer,
                None,
            )
            .await
            .expect("unshare");
        let after = s
            .mini_apps
            .invoke_yui_action(&bob, &dispatcher, app, press("press-9999".into()), None)
            .await;
        assert!(
            matches!(after, Err(ActionError::NotFound | ActionError::Forbidden)),
            "{after:?}"
        );
        assert_eq!(starter.0.load(std::sync::atomic::Ordering::SeqCst), 3);
    })
    .await;
}

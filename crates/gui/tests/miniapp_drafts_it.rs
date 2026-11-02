//! ミニアプリの下書きと公開・画面の書き損じの結合テスト（D-16・GAP-36）。
//!
//! - 実 Postgres（`STORAGE_TEST_DATABASE_URL`）＋実 OpenFGA（`OPENFGA_TEST_URL`）: 使う人（viewer）には公開した版
//!   だけを出し、editor は下書きを試せる。公開・公開を戻す・公開の権限・開いた後に公開した版が変わったボタン。
//! - 実 Postgres だけ: 画面の書き損じは下書きには注意つきで保存でき、公開と AI の経路では止める。
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
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
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
        let Ok(base_url) = std::env::var("OPENFGA_TEST_URL") else {
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
        tenant.into(),
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

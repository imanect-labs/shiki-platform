//! アプリとワークフローを使う人の権限の確かめの結合テスト（GAP-37）。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）＋実 OpenFGA（`OPENFGA_TEST_URL`）で次を確かめる。
//!
//! - 作る人（持ち主）は、部品が要る権限と、共有の相手（人・部署）ごとに足りるかを受け取る。追加だけの相手は、追加には
//!   足り、書き換えは範囲が狭い。自分に見えない対象は名前を出さず、相手の権限も確かめない。値で決まる対象は確かめない。
//! - 相手を省くと共有している相手を確かめる。持ち主でない editor は相手ごとの結果を受け取れず、相手を書くと権限なし。
//!   相手を書けるのは共有している相手だけ。相手の権限は、求めた人が持ち主の対象だけ確かめる（見えるだけ・追加だけの
//!   対象は確かめない）。部署の相手は、組織全体に共有したファイル・フォルダーを使える。
//! - 使う人は、自分に足りない権限を部品ごとに受け取り、見えない対象は名前を出さない。足りている部品は返さない。
//!   使う人は公開した版だけを確かめられる。共有を足すと、次の確かめから足りると言う。
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
use std::time::Duration;

use artifact::{ArtifactKind, ArtifactRole, ArtifactStore, NewArtifact};
use async_trait::async_trait;
use authz::{AuthContext, AuthzClient, Principal, Relation};
use data::{
    DataStore, DataViewBody, DataViewStore, FieldDef, FieldType, NewDataTable, RefResolver,
};
use gui::{
    AccessChecker, AccessKind, AccessObject, AccessPart, AccessStatus, GuiError, MiniAppStore,
    SaveMode, SkillStore,
};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use storage::{ShareTarget, StorageService};
use uuid::Uuid;

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

/// フォルダーだけを作るテストのオブジェクトストア（バイトは扱わない）。
struct NoBytes;
#[async_trait]
impl storage::ObjectStore for NoBytes {
    async fn ensure_bucket(&self) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn presign_put(
        &self,
        _: &str,
        _: Duration,
        _: i64,
    ) -> Result<String, storage::ObjectStoreError> {
        Err(storage::ObjectStoreError::NotFound("none".into()))
    }
    async fn presign_get(
        &self,
        _: &str,
        _: Duration,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Result<String, storage::ObjectStoreError> {
        Err(storage::ObjectStoreError::NotFound("none".into()))
    }
    async fn presign_get_internal(
        &self,
        _: &str,
        _: Duration,
    ) -> Result<String, storage::ObjectStoreError> {
        Err(storage::ObjectStoreError::NotFound("none".into()))
    }
    async fn read_and_hash(&self, _: &str) -> Result<(String, u64), storage::ObjectStoreError> {
        Err(storage::ObjectStoreError::NotFound("none".into()))
    }
    async fn put_object(
        &self,
        _: &str,
        _: Vec<u8>,
        _: &str,
    ) -> Result<(), storage::ObjectStoreError> {
        Err(storage::ObjectStoreError::NotFound("none".into()))
    }
    async fn get_object(&self, _: &str) -> Result<Vec<u8>, storage::ObjectStoreError> {
        Err(storage::ObjectStoreError::NotFound("none".into()))
    }
    async fn exists(&self, _: &str) -> Result<bool, storage::ObjectStoreError> {
        Ok(false)
    }
    async fn copy(&self, _: &str, _: &str) -> Result<(), storage::ObjectStoreError> {
        Err(storage::ObjectStoreError::NotFound("none".into()))
    }
    async fn delete(&self, _: &str) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn list_prefix(
        &self,
        _: &str,
        _: Option<&str>,
    ) -> Result<(Vec<String>, Option<String>), storage::ObjectStoreError> {
        Ok((Vec::new(), None))
    }
    async fn delete_batch(&self, _: &[String]) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
}

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
                store_name: format!("shiki-access-check-{}", Uuid::new_v4()),
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

/// 本体を走らせ、落ちても後片付け（テナントの行・OpenFGA のストア）をしてから結果を返す。
async fn run_with_cleanup<F>(pool: PgPool, tenant: String, fga: Fga, body: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let outcome = tokio::spawn(body).await;
    common::purge_tenant_rows(&pool, &tenant).await;
    fga.delete_store().await;
    if let Err(e) = outcome {
        std::panic::resume_unwind(e.into_panic());
    }
}

struct Env {
    fga: Arc<dyn AuthzClient>,
    artifacts: Arc<ArtifactStore>,
    data: DataStore,
    storage: StorageService,
    mini_apps: MiniAppStore,
    skills: SkillStore,
    views: DataViewStore,
}

impl Env {
    fn new(pool: &PgPool, fga: &Arc<dyn AuthzClient>) -> Env {
        let artifacts = Arc::new(ArtifactStore::new(pool.clone(), Arc::clone(fga)));
        let data = DataStore::new(pool.clone(), Arc::clone(fga), Arc::new(AllowRefs));
        Env {
            fga: Arc::clone(fga),
            storage: StorageService::new(
                pool.clone(),
                Arc::new(NoBytes),
                Arc::clone(fga),
                Duration::from_secs(60),
                Duration::from_secs(60),
                1024,
            ),
            mini_apps: MiniAppStore::new(Arc::clone(&artifacts), pool.clone()),
            skills: SkillStore::new(Arc::clone(&artifacts)),
            views: DataViewStore::new(Arc::clone(&artifacts), data.clone()),
            data,
            artifacts,
        }
    }

    fn checker(&self) -> AccessChecker<'_> {
        AccessChecker {
            authz: self.fga.as_ref(),
            artifacts: &self.artifacts,
            data: &self.data,
            storage: &self.storage,
        }
    }

    async fn table(&self, owner: &AuthContext, name: &str) -> Uuid {
        self.data
            .create_table(
                owner,
                NewDataTable {
                    name: name.into(),
                    schema: data::TableSchema {
                        fields: vec![FieldDef::new("title", FieldType::Text)],
                        status_field: None,
                        row_policy: None,
                        field_policy: Vec::new(),
                        aggregate_min_rows: None,
                        fsm_ref: None,
                        rag: None,
                    },
                },
                None,
            )
            .await
            .expect("table")
            .id
    }

    async fn grant(
        &self,
        who: &storage::ShareTarget,
        relation: Relation,
        object: authz::FgaObject,
        by: &AuthContext,
    ) {
        self.fga
            .write_tuple(&who.subject(&by.ns()), relation, &object)
            .await
            .expect("tuple");
    }
}

fn user(id: &str) -> ShareTarget {
    ShareTarget::User { id: id.into() }
}

/// 作る人の確かめ: 相手ごとの結果・見えない対象・値で決まる対象・部署。使う人の確かめ: 自分に足りないものだけ、
/// 見えない対象は名前なし、公開した版だけ。共有を足すと次の確かめから足りる。
#[tokio::test]
async fn makers_see_each_recipient_and_users_see_their_own_gaps() {
    let Some(pool) = pool().await else { return };
    let Some(fga) = Fga::connect().await else {
        return;
    };
    let client = Arc::clone(&fga.client);
    let tenant = format!("t-{}", Uuid::new_v4());
    let t = tenant.clone();
    let db = pool.clone();
    run_with_cleanup(pool, tenant, fga, async move {
        let e = Env::new(&db, &client);
        let alice = ctx(&t, "alice"); // 作る人（アプリ・テーブル・フォルダーの持ち主）
        let bob = ctx(&t, "bob"); // 使う人（アプリの閲覧・申請のテーブルは追加だけ・一覧のテーブルは閲覧）
        let erin = ctx(&t, "erin"); // 別の人（alice に見えないテーブルの持ち主）
        let dave = ctx(&t, "dave"); // アプリを共有されていない人
        let ns = alice.ns();

        let claims = e.table(&alice, "経費の申請").await;
        let ledger = e.table(&alice, "経費の台帳").await;
        let hidden = e.table(&erin, "人事の記録").await;
        // ドライブのトップにフォルダーを作れるのは組織のメンバー。
        e.fga
            .write_tuple(&alice.subject(), Relation::Member, &ns.organization("acme"))
            .await
            .unwrap();
        let folder = e
            .storage
            .create_folder(&alice, None, "申請の控え", None)
            .await
            .expect("folder")
            .id;
        e.grant(&user("bob"), Relation::Submitter, ns.data_table(&claims.to_string()), &alice).await;
        e.grant(&user("bob"), Relation::Viewer, ns.data_table(&ledger.to_string()), &alice).await;
        // 部署（経理）は台帳を見られる。carol はその部署の人。
        e.grant(&ShareTarget::Role { id: "keiri".into() }, Relation::Viewer, ns.data_table(&ledger.to_string()), &alice).await;
        e.fga
            .write_tuple(&user("carol").subject(&ns), Relation::Member, &ns.role("keiri"))
            .await
            .unwrap();

        // 申請のワークフロー: 追加・書き換え・フォルダーへの保存・見えないテーブルへの追加・値で決まるテーブルを読む。
        let ir = json!({
            "ir_version": 1, "name": "経費の申請",
            "declared_scopes": ["data.read", "data.write", "storage.write"],
            "nodes": [
                { "id": "add", "type": "data.record.create", "params": { "table": claims.to_string(), "data": {} } },
                { "id": "fix", "type": "data.record.update", "params": { "table": claims.to_string(), "id": "x", "data": {} } },
                { "id": "save", "type": "storage.write", "params": { "folder": folder.to_string(), "name": "控え.txt", "content": "x" } },
                { "id": "hr", "type": "data.record.create", "params": { "table": hidden.to_string(), "data": {} } },
                { "id": "q", "type": "data.query", "params": { "table": { "$from": "input", "path": "/table" } } }
            ],
            "edges": []
        });
        let wf = e
            .artifacts
            .create(&alice, NewArtifact { kind: ArtifactKind::Workflow, name: "経費の申請".into(), body: ir }, None)
            .await
            .expect("wf")
            .id;
        let view = e
            .views
            .create(
                &alice,
                "台帳",
                &DataViewBody { table_id: ledger, query: Default::default(), display: Default::default() },
                None,
            )
            .await
            .expect("view");
        let (skill, _) = e
            .skills
            .create(
                &alice,
                "経費の規程",
                &json!({ "description": "経費の規程", "instructions": "規程で答えます。",
                         "knowledge_scope": { "folders": [folder], "files": [] } }),
                None,
            )
            .await
            .expect("skill");
        let screen = "# 経費\n\n```ui\napp 経費の申請\n金額 = number 必須 円 as amount\n\
                      button 申請する -> workflow 申請\n```\n\n```data_grid\n台帳\n```\n";
        let body = json!({
            "description": "経費",
            "screen": screen,
            "skill": { "artifact_id": skill, "version": 1 },
            "workflows": [{ "alias": "申請", "artifact_id": wf, "version": 1 }],
            "data_views": [{ "alias": "台帳", "artifact_id": view, "version": 1 }],
        });
        let app = e.mini_apps.create(&alice, "経費", &body, SaveMode::Draft, None).await.expect("app").id;
        e.mini_apps.publish(&alice, app, 1, None).await.expect("publish");
        e.artifacts.share(&alice, app, &user("bob"), ArtifactRole::Viewer, None).await.expect("share");
        e.artifacts
            .share(&alice, app, &ShareTarget::Role { id: "keiri".into() }, ArtifactRole::Viewer, None)
            .await
            .expect("share role");

        // --- 作る人（持ち主）: 相手を省くと共有している相手を確かめる ---
        let c = e.checker();
        c.require_maker(&alice, app, ArtifactKind::MiniApp, None).await.expect("maker");
        let subjects = c.subjects_for(&alice, app, None, None).await.expect("subjects").expect("持ち主は相手ごと");
        assert_eq!(subjects.0.len(), 2, "{subjects:?}");
        assert!(!subjects.1);
        let (version, parts) = e.mini_apps.part_needs(&alice, app, None, "access_check", None).await.expect("parts");
        assert_eq!(version, 1);
        let report = c.report(&alice, version, &parts, Some(&subjects.0)).await.expect("report");
        let row = |part: &AccessPart, object: AccessObject, id: Option<Uuid>, kind: AccessKind| {
            report
                .needs
                .iter()
                .position(|n| {
                    n.part.as_ref() == Some(part)
                        && n.object == object
                        && n.need == kind
                        && match id {
                            Some(id) => n.target.as_ref().map(|t| t.id) == Some(id),
                            None => n.target.is_none(),
                        }
                })
                .unwrap_or_else(|| panic!("{part:?} {object:?} {kind:?} が無い: {:?}", report.needs))
        };
        let wf_part = AccessPart::Workflow { alias: "申請".into() };
        let view_part = AccessPart::DataView { alias: "台帳".into() };
        let add = row(&wf_part, AccessObject::Table, Some(claims), AccessKind::Add);
        let fix = row(&wf_part, AccessObject::Table, Some(claims), AccessKind::Change);
        let save = row(&wf_part, AccessObject::Folder, Some(folder), AccessKind::Edit);
        let read_ledger = row(&view_part, AccessObject::Table, Some(ledger), AccessKind::Read);
        let skill_folder = row(&AccessPart::Skill, AccessObject::Folder, Some(folder), AccessKind::Read);
        assert_eq!(report.needs.len(), 7, "{:?}", report.needs);
        // 見えないテーブルは名前を出さない（id も返さない）。値で決まる対象は dynamic。
        let hr = report
            .needs
            .iter()
            .position(|n| n.need == AccessKind::Add && n.target.is_none() && !n.dynamic)
            .expect("見えないテーブルへの追加");
        let dynamic = report.needs.iter().position(|n| n.dynamic).expect("値で決まる対象");
        assert_eq!(report.needs[add].target.as_ref().unwrap().name, "経費の申請");
        assert!(report.needs[add].manageable && report.needs[save].manageable, "持ち主の対象は相手を確かめる");
        assert!(!report.needs[hr].manageable && !report.needs[dynamic].manageable);
        assert!(!serde_json::to_string(&report).unwrap().contains(&hidden.to_string()), "見えない対象の id を出さない");
        let results = report.subjects.as_ref().expect("持ち主は相手ごと");
        let bob_s = results.iter().find(|r| r.subject == user("bob")).expect("bob");
        let keiri = results
            .iter()
            .find(|r| r.subject == ShareTarget::Role { id: "keiri".into() })
            .expect("keiri");
        assert_eq!(bob_s.statuses[add], AccessStatus::Ok, "追加だけで追加はできる");
        assert_eq!(bob_s.statuses[fix], AccessStatus::Limited, "追加だけの書き換えは範囲が狭い");
        assert_eq!(bob_s.statuses[save], AccessStatus::Missing);
        assert_eq!(bob_s.statuses[read_ledger], AccessStatus::Ok);
        assert_eq!(bob_s.statuses[skill_folder], AccessStatus::Missing);
        assert_eq!(bob_s.statuses[hr], AccessStatus::Unchecked, "見えない対象の他人の権限は問わない");
        assert_eq!(bob_s.statuses[dynamic], AccessStatus::Unchecked);
        assert_eq!(keiri.statuses[read_ledger], AccessStatus::Ok, "部署に共有した台帳は部署として読める");
        assert_eq!(keiri.statuses[add], AccessStatus::Missing);
        assert_eq!(keiri.statuses[save], AccessStatus::Missing);

        // 組織全体に編集で共有したフォルダーは、部署の人も書ける（部署の userset では一致しないので、組織全体も問う）。
        e.fga
            .write_tuple(&ns.organization_member("acme"), Relation::Editor, &ns.folder(&folder.to_string()))
            .await
            .unwrap();
        let report = c.report(&alice, version, &parts, Some(&subjects.0)).await.expect("report");
        let results = report.subjects.as_ref().unwrap();
        let keiri = results.iter().find(|r| matches!(r.subject, ShareTarget::Role { .. })).unwrap();
        assert_eq!(keiri.statuses[save], AccessStatus::Ok, "組織全体の共有は部署にも効く");
        assert_eq!(keiri.statuses[skill_folder], AccessStatus::Ok);
        let bob_s = results.iter().find(|r| r.subject == user("bob")).unwrap();
        assert_eq!(bob_s.statuses[save], AccessStatus::Missing, "組織の member でない人には足さない");
        e.fga
            .delete_tuple(&ns.organization_member("acme"), Relation::Editor, &ns.folder(&folder.to_string()))
            .await
            .unwrap();

        // 書ける相手は、今共有している相手だけ（ほかの人の権限を問う口にしない）。
        let picked = c.subjects_for(&alice, app, Some(vec![user("bob")]), None).await.unwrap().unwrap();
        assert_eq!(picked.0, vec![user("bob")]);
        match c.subjects_for(&alice, app, Some(vec![user("bob"), user("dave")]), None).await {
            Err(GuiError::Validation(errs)) => assert_eq!(errs[0].code, "access.subject_not_shared"),
            other => panic!("共有していない相手は断る: {other:?}"),
        }

        // 相手を書けるのは持ち主だけ。共同編集者（editor）は相手ごとの結果を受け取れない。
        let ed = ctx(&t, "ed");
        e.artifacts.share(&alice, app, &user("ed"), ArtifactRole::Editor, None).await.unwrap();
        e.checker().require_maker(&ed, app, ArtifactKind::MiniApp, None).await.expect("editor は作る人");
        assert!(e.checker().subjects_for(&ed, app, None, None).await.unwrap().is_none());
        assert!(matches!(
            e.checker().subjects_for(&ed, app, Some(vec![user("bob")]), None).await,
            Err(GuiError::Artifact(artifact::ArtifactError::Forbidden))
        ));
        assert!(matches!(
            e.checker().require_maker(&bob, app, ArtifactKind::MiniApp, None).await,
            Err(GuiError::Artifact(artifact::ArtifactError::Forbidden))
        ));
        // 相手の上限と書き方の誤り。
        let many: Vec<ShareTarget> = (0..=gui::MAX_ACCESS_SUBJECTS).map(|i| user(&format!("u{i}"))).collect();
        assert!(matches!(
            e.checker().subjects_for(&alice, app, Some(many), None).await,
            Err(GuiError::Validation(_))
        ));
        assert!(matches!(
            e.checker().subjects_for(&alice, app, Some(vec![user("a|b")]), None).await,
            Err(GuiError::Validation(_))
        ));

        // --- 使う人（bob）: 足りない部品だけ・見えない対象は名前なし ---
        let (v, parts) = e.mini_apps.part_needs(&bob, app, None, "my_access", None).await.expect("bob parts");
        let mine = e.checker().mine(&bob, v, &parts).await.expect("mine");
        let wf_mine = mine.parts.iter().find(|p| p.part == wf_part).expect("申請は足りない");
        assert_eq!(wf_mine.status, AccessStatus::Missing);
        let items: Vec<(AccessObject, AccessKind, AccessStatus, Option<&str>)> = wf_mine
            .items
            .iter()
            .map(|i| (i.object, i.need, i.status, i.name.as_deref()))
            .collect();
        assert_eq!(
            items,
            vec![
                (AccessObject::Table, AccessKind::Change, AccessStatus::Limited, Some("経費の申請")),
                (AccessObject::Folder, AccessKind::Edit, AccessStatus::Missing, None),
                (AccessObject::Table, AccessKind::Add, AccessStatus::Missing, None),
            ],
            "見えないフォルダー・テーブルは名前を出さない"
        );
        assert!(mine.parts.iter().all(|p| p.part != view_part), "足りている表は返さない: {mine:?}");
        let skill_mine = mine.parts.iter().find(|p| p.part == AccessPart::Skill).expect("スキル");
        assert_eq!(skill_mine.items[0].name, None);
        assert!(!serde_json::to_string(&mine).unwrap().contains(&folder.to_string()), "使う人には id を返さない");

        // 共有を足すと、次の確かめから足りると言う（フォルダーの編集）。
        e.grant(&user("bob"), Relation::Editor, ns.folder(&folder.to_string()), &alice).await;
        let mine = e.checker().mine(&bob, v, &parts).await.expect("mine again");
        let wf_mine = mine.parts.iter().find(|p| p.part == wf_part).unwrap();
        assert!(wf_mine.items.iter().all(|i| i.object != AccessObject::Folder), "{wf_mine:?}");
        assert!(mine.parts.iter().all(|p| p.part != AccessPart::Skill), "フォルダーを見られるようになった");

        // 使う人は公開した版だけ。下書きを確かめようとすると公開していないと言う。共有されていない人は見つからない。
        e.mini_apps.update(&alice, app, &body, None, SaveMode::Draft, None).await.expect("draft");
        assert!(matches!(
            e.mini_apps.part_needs(&bob, app, Some(2), "my_access", None).await,
            Err(GuiError::Artifact(artifact::ArtifactError::NotPublished { published: Some(1) }))
        ));
        assert!(e.mini_apps.part_needs(&dave, app, None, "my_access", None).await.is_err());
    })
    .await;
}

/// 相手の権限は、求めた人が持ち主の対象だけ確かめる。テーブルを閲覧できるだけの人が、そのテーブルを読むワークフローを
/// 作って共有しても、相手がそのテーブルに持つ権限は返さない（共有の一覧を持ち主にだけ見せる決まりを迂回させない）。
#[tokio::test]
async fn makers_learn_others_access_only_to_what_they_own() {
    let Some(pool) = pool().await else { return };
    let Some(fga) = Fga::connect().await else {
        return;
    };
    let client = Arc::clone(&fga.client);
    let tenant = format!("t-{}", Uuid::new_v4());
    let t = tenant.clone();
    let db = pool.clone();
    run_with_cleanup(pool, tenant, fga, async move {
        let e = Env::new(&db, &client);
        let alice = ctx(&t, "alice"); // 人事の表の持ち主
        let bob = ctx(&t, "bob"); // 人事の表を閲覧できるだけの人
        let ns = alice.ns();
        let hr = e.table(&alice, "人事の記録").await;
        let own = e.table(&bob, "bob の表").await;
        e.grant(&user("bob"), Relation::Viewer, ns.data_table(&hr.to_string()), &alice).await;
        e.grant(&user("carol"), Relation::Editor, ns.data_table(&hr.to_string()), &alice).await;
        e.grant(&ShareTarget::Role { id: "jinji".into() }, Relation::Viewer, ns.data_table(&hr.to_string()), &alice)
            .await;
        let ir = json!({
            "ir_version": 1, "name": "読む",
            "declared_scopes": ["data.read", "data.write"],
            "nodes": [
                { "id": "q", "type": "data.query", "params": { "table": hr.to_string() } },
                { "id": "add", "type": "data.record.create", "params": { "table": own.to_string(), "data": {} } }
            ],
            "edges": []
        });
        let wf = e
            .artifacts
            .create(&bob, NewArtifact { kind: ArtifactKind::Workflow, name: "読む".into(), body: ir.clone() }, None)
            .await
            .unwrap()
            .id;
        e.artifacts.share(&bob, wf, &user("carol"), ArtifactRole::Viewer, None).await.unwrap();
        e.artifacts
            .share(&bob, wf, &ShareTarget::Role { id: "jinji".into() }, ArtifactRole::Viewer, None)
            .await
            .unwrap();
        let parts = [gui::workflow_needs(&workflow_engine::WorkflowIr::from_json(&ir).unwrap())];
        let subjects = e.checker().subjects_for(&bob, wf, None, None).await.unwrap().unwrap();
        assert_eq!(subjects.0.len(), 2);
        let report = e.checker().report(&bob, 1, &parts, Some(&subjects.0)).await.unwrap();
        let read_hr = report
            .needs
            .iter()
            .position(|n| n.need == AccessKind::Read)
            .expect("人事の表を読む");
        let add_own = report.needs.iter().position(|n| n.need == AccessKind::Add).expect("自分の表に追加");
        // 見える表なので名前は出すが、持ち主でないので相手の権限は問わない。
        assert_eq!(report.needs[read_hr].target.as_ref().unwrap().name, "人事の記録");
        assert!(!report.needs[read_hr].manageable);
        assert!(report.needs[add_own].manageable);
        for r in report.subjects.as_ref().unwrap() {
            assert_eq!(r.statuses[read_hr], AccessStatus::Unchecked, "{:?} の人事の表の権限を返さない", r.subject);
            assert_eq!(r.statuses[add_own], AccessStatus::Missing);
        }
        // 共有していない人を書いても問えない。
        match e.checker().subjects_for(&bob, wf, Some(vec![user("dave")]), None).await {
            Err(GuiError::Validation(errs)) => assert_eq!(errs[0].code, "access.subject_not_shared"),
            other => panic!("共有していない相手は断る: {other:?}"),
        }
    })
    .await;
}

/// 読めない部品（消えたワークフロー）は使えないと返し、ほかの部品は確かめる。ワークフローそのものの確かめ
/// （共有されたワークフロー）は部品の名前を持たない。
#[tokio::test]
async fn unavailable_parts_and_shared_workflows() {
    let Some(pool) = pool().await else { return };
    let Some(fga) = Fga::connect().await else {
        return;
    };
    let client = Arc::clone(&fga.client);
    let tenant = format!("t-{}", Uuid::new_v4());
    let t = tenant.clone();
    let db = pool.clone();
    run_with_cleanup(pool, tenant, fga, async move {
        let e = Env::new(&db, &client);
        let alice = ctx(&t, "alice");
        let bob = ctx(&t, "bob");
        let ns = alice.ns();
        let claims = e.table(&alice, "経費の申請").await;
        let ir = json!({
            "ir_version": 1, "name": "申請",
            "declared_scopes": ["data.write"],
            "nodes": [{ "id": "add", "type": "data.record.create", "params": { "table": claims.to_string(), "data": {} } }],
            "edges": []
        });
        let wf = e
            .artifacts
            .create(&alice, NewArtifact { kind: ArtifactKind::Workflow, name: "申請".into(), body: ir.clone() }, None)
            .await
            .unwrap()
            .id;
        let gone = e
            .artifacts
            .create(&alice, NewArtifact { kind: ArtifactKind::Workflow, name: "消す".into(), body: ir.clone() }, None)
            .await
            .unwrap()
            .id;
        let screen = "```ui\napp 申請\n件名 = text 必須 as title\nbutton 申請する -> workflow 申請\nbutton 消す -> workflow 消す\n```\n";
        let body = json!({
            "description": "経費", "screen": screen,
            "workflows": [
                { "alias": "申請", "artifact_id": wf, "version": 1 },
                { "alias": "消す", "artifact_id": gone, "version": 1 }
            ],
        });
        let app = e.mini_apps.create(&alice, "経費", &body, SaveMode::Draft, None).await.unwrap().id;
        e.mini_apps.publish(&alice, app, 1, None).await.unwrap();
        e.artifacts.share(&alice, app, &user("bob"), ArtifactRole::Viewer, None).await.unwrap();
        e.artifacts.delete(&alice, gone, None).await.expect("delete");

        let (v, parts) = e.mini_apps.part_needs(&bob, app, None, "my_access", None).await.unwrap();
        let mine = e.checker().mine(&bob, v, &parts).await.unwrap();
        let gone_part = mine
            .parts
            .iter()
            .find(|p| p.part == AccessPart::Workflow { alias: "消す".into() })
            .expect("消えた部品");
        assert!(gone_part.unavailable);
        let ok_part = mine
            .parts
            .iter()
            .find(|p| p.part == AccessPart::Workflow { alias: "申請".into() })
            .expect("申請は見えないテーブルに書く");
        assert_eq!(ok_part.items[0].name, None, "見えないテーブルは名前なし");
        let report = e.checker().report(&alice, v, &parts, None).await.unwrap();
        assert_eq!(report.unavailable, vec![AccessPart::Workflow { alias: "消す".into() }]);
        assert!(report.subjects.is_none());

        // ワークフローそのもの（共有されたワークフロー）: 部品の名前は持たない。
        let parts = [gui::workflow_needs(&workflow_engine::WorkflowIr::from_json(&ir).unwrap())];
        e.artifacts.share(&alice, wf, &user("bob"), ArtifactRole::Viewer, None).await.unwrap();
        let subjects = e.checker().subjects_for(&alice, wf, None, None).await.unwrap().unwrap();
        let report = e.checker().report(&alice, 1, &parts, Some(&subjects.0)).await.unwrap();
        assert_eq!(report.needs.len(), 1);
        assert_eq!(report.needs[0].part, None);
        assert_eq!(report.subjects.unwrap()[0].statuses, vec![AccessStatus::Missing]);
        e.fga
            .write_tuple(&user("bob").subject(&ns), Relation::Submitter, &ns.data_table(&claims.to_string()))
            .await
            .unwrap();
        let report = e.checker().report(&alice, 1, &parts, Some(&subjects.0)).await.unwrap();
        assert_eq!(report.subjects.unwrap()[0].statuses, vec![AccessStatus::Ok]);
    })
    .await;
}

/// 道によって通るブロックだけが要る権限では、使う人のボタンを止めない（BUG-216）。分かれた流れの片方・「エラー時」の
/// 出口の先だけが書くテーブルに権限が無くても、保存済みのアプリのボタンは押せる（7g の前と同じ）。必ず通るブロックが
/// 要る権限が足りなければ止める。作る人には、どちらも挙げて「場合によって要る」と言い分ける。
#[tokio::test]
async fn needs_on_some_paths_do_not_block_buttons() {
    let Some(pool) = pool().await else { return };
    let Some(fga) = Fga::connect().await else {
        return;
    };
    let client = Arc::clone(&fga.client);
    let tenant = format!("t-{}", Uuid::new_v4());
    let t = tenant.clone();
    let db = pool.clone();
    run_with_cleanup(pool, tenant, fga, async move {
        let e = Env::new(&db, &client);
        let alice = ctx(&t, "alice");
        let bob = ctx(&t, "bob");
        let ns = alice.ns();
        let claims = e.table(&alice, "経費の申請").await;
        let big = e.table(&alice, "高額の確認").await;
        let errors = e.table(&alice, "失敗の記録").await;
        e.grant(&user("bob"), Relation::Submitter, ns.data_table(&claims.to_string()), &alice).await;
        // 申請を足し、金額が大きいときだけ高額の確認に足す。足せなかったら失敗の記録に足す（bob はどちらも持たない）。
        let ir = json!({
            "ir_version": 1, "name": "申請",
            "declared_scopes": ["data.write"],
            "nodes": [
                { "id": "add", "type": "data.record.create", "on_error": "continue",
                  "params": { "table": claims.to_string(), "data": {} } },
                { "id": "check", "type": "control.branch",
                  "params": { "condition": { "op": "gt", "left": { "$from": "input", "path": "/amount" }, "right": 100000 } } },
                { "id": "big", "type": "data.record.create", "params": { "table": big.to_string(), "data": {} } },
                { "id": "log", "type": "data.record.create", "params": { "table": errors.to_string(), "data": {} } }
            ],
            "edges": [
                { "from": "add", "to": "check" },
                { "from": "check", "from_port": "true", "to": "big" },
                { "from": "add", "from_port": "error", "to": "log" }
            ]
        });
        let wf = e
            .artifacts
            .create(&alice, NewArtifact { kind: ArtifactKind::Workflow, name: "申請".into(), body: ir }, None)
            .await
            .unwrap()
            .id;
        let screen = "```ui\napp 申請\n金額 = number 必須 円 as amount\nbutton 申請する -> workflow 申請\n```\n";
        let body = json!({
            "description": "経費", "screen": screen,
            "workflows": [{ "alias": "申請", "artifact_id": wf, "version": 1 }],
        });
        let app = e.mini_apps.create(&alice, "経費", &body, SaveMode::Draft, None).await.unwrap().id;
        e.mini_apps.publish(&alice, app, 1, None).await.unwrap();
        e.artifacts.share(&alice, app, &user("bob"), ArtifactRole::Viewer, None).await.unwrap();

        // 使う人: 必ず通る追加は足りるので、ボタンは止まらない（高額の確認・失敗の記録の権限は問わない）。
        let (v, parts) = e.mini_apps.part_needs(&bob, app, None, "my_access", None).await.unwrap();
        let mine = e.checker().mine(&bob, v, &parts).await.unwrap();
        assert!(mine.parts.is_empty(), "道によって通るブロックの権限でボタンを止めない: {mine:?}");

        // 作る人: 3 つとも挙げ、道によって通るものに印を付ける。bob に足りないのは場合によって要るものだけ。
        let subjects = e.checker().subjects_for(&alice, app, None, None).await.unwrap().unwrap();
        let report = e.checker().report(&alice, v, &parts, Some(&subjects.0)).await.unwrap();
        let flags: Vec<(String, bool)> = report
            .needs
            .iter()
            .map(|n| (n.target.as_ref().unwrap().name.clone(), n.conditional))
            .collect();
        assert_eq!(
            flags,
            vec![
                ("経費の申請".to_string(), false),
                ("高額の確認".to_string(), true),
                ("失敗の記録".to_string(), true),
            ]
        );
        assert_eq!(
            report.subjects.unwrap()[0].statuses,
            vec![AccessStatus::Ok, AccessStatus::Missing, AccessStatus::Missing]
        );

        // 必ず通る追加の権限を外すと、ボタンを止める。
        e.fga
            .delete_tuple(&user("bob").subject(&ns), Relation::Submitter, &ns.data_table(&claims.to_string()))
            .await
            .unwrap();
        let mine = e.checker().mine(&bob, v, &parts).await.unwrap();
        let wf_mine = mine.parts.iter().find(|p| p.part == AccessPart::Workflow { alias: "申請".into() }).unwrap();
        assert_eq!(wf_mine.items.len(), 1, "必ず通る追加だけを言う: {wf_mine:?}");
        assert_eq!(wf_mine.items[0].need, AccessKind::Add);
    })
    .await;
}

/// 前もって確かめられないブロック（スクリプト・シークレット・別のワークフローを実行・ドライブのトップへの保存）は、
/// 要る権限には挙げず、作る人の結果にブロックの名前つきで返す（GAP-82）。部品ごとにまとめ、使う人には返さない。
#[tokio::test]
async fn blocks_beyond_the_check_are_reported_to_makers() {
    let Some(pool) = pool().await else { return };
    let Some(fga) = Fga::connect().await else {
        return;
    };
    let client = Arc::clone(&fga.client);
    let tenant = format!("t-{}", Uuid::new_v4());
    let t = tenant.clone();
    let db = pool.clone();
    run_with_cleanup(pool, tenant, fga, async move {
        let e = Env::new(&db, &client);
        let alice = ctx(&t, "alice");
        let bob = ctx(&t, "bob");
        let ir = json!({
            "ir_version": 1, "name": "集計",
            "declared_scopes": ["data.write", "http.egress", "workflow.start", "storage.write"],
            "nodes": [
                { "id": "calc", "type": "script.run", "label": "金額を足す", "params": { "source": { "inline": "export function main(){}" } } },
                { "id": "post", "type": "http.request", "params": { "url": "https://example.com", "secret": { "name": "k" } } },
                { "id": "next", "type": "workflow.start", "params": { "name": "child" } },
                { "id": "top", "type": "storage.write", "label": "控えを保存", "params": { "name": "a.txt", "content": "x" } }
            ],
            "edges": []
        });
        let wf = e
            .artifacts
            .create(&alice, NewArtifact { kind: ArtifactKind::Workflow, name: "集計".into(), body: ir.clone() }, None)
            .await
            .unwrap()
            .id;
        let screen = "```ui\napp 集計\nbutton 集計する -> workflow 集計\n```\n";
        let body = json!({
            "description": "集計", "screen": screen,
            "workflows": [{ "alias": "集計", "artifact_id": wf, "version": 1 }],
        });
        let app = e.mini_apps.create(&alice, "集計", &body, SaveMode::Draft, None).await.unwrap().id;
        e.mini_apps.publish(&alice, app, 1, None).await.unwrap();
        e.artifacts.share(&alice, app, &user("bob"), ArtifactRole::Viewer, None).await.unwrap();

        let (v, parts) = e.mini_apps.part_needs(&alice, app, None, "access_check", None).await.unwrap();
        let subjects = e.checker().subjects_for(&alice, app, None, None).await.unwrap().unwrap();
        let report = e.checker().report(&alice, v, &parts, Some(&subjects.0)).await.unwrap();
        assert!(report.needs.is_empty(), "確かめられるものは無い: {:?}", report.needs);
        let part = Some(AccessPart::Workflow { alias: "集計".into() });
        let got: Vec<(Option<AccessPart>, String, gui::AccessUnchecked)> =
            report.unchecked.iter().map(|u| (u.part.clone(), u.block.clone(), u.kind)).collect();
        assert_eq!(got.len(), 4, "{got:?}");
        assert_eq!(got[0], (part.clone(), "金額を足す".to_string(), gui::AccessUnchecked::Script));
        assert_eq!(got[1].2, gui::AccessUnchecked::Secret);
        assert_eq!(got[2].2, gui::AccessUnchecked::StartWorkflow);
        assert_eq!(got[3], (part, "控えを保存".to_string(), gui::AccessUnchecked::DriveTop));
        // ワークフローそのもの（共有されたワークフロー）でも同じ。部品の名前は持たない。
        let wf_parts = [gui::workflow_needs(&workflow_engine::WorkflowIr::from_json(&ir).unwrap())];
        let report = e.checker().report(&alice, 1, &wf_parts, None).await.unwrap();
        assert_eq!(report.unchecked.len(), 4);
        assert!(report.unchecked.iter().all(|u| u.part.is_none()));
        // 使う人のボタンは止めない（確かめられないものでは止めない）。
        let (v, parts) = e.mini_apps.part_needs(&bob, app, None, "my_access", None).await.unwrap();
        assert!(e.checker().mine(&bob, v, &parts).await.unwrap().parts.is_empty());
    })
    .await;
}

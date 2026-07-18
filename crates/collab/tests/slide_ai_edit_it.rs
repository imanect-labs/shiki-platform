//! AI スライド共同編集の結合テスト（Task 11.3・実 Postgres が必要）。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行し、未設定ならスキップする。
//! 検証（phase-11 Task 11.3 受け入れ条件）:
//! - 人間の並行編集と AI 編集が同一 LiveDoc 上で収束し、保存 JSON に両方が乗る（排他なし）
//! - AI 入力の敵対的 HTML が適用時にサニタイズされる（PIT-40 第1層）
//! - editor 権限のない実行主体の slide.edit が拒否される（viewer は読めるが書けない）

// テストコード: pedantic/安全系 lint は本番コードのみ厳格化する方針のため許容する。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::cognitive_complexity
)]

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use collab::slide::{Slide, SlideDoc, SlideEditOp};
use collab::{CollabHub, SLIDE_MIME};
use sqlx::{postgres::PgPoolOptions, PgPool};
use storage::{Node, StorageService};
use uuid::Uuid;

/// (subject, relation) 付与集合を持つ authz モック。
struct RoleAuthz {
    grants: Mutex<HashSet<(String, Relation)>>,
}
impl RoleAuthz {
    fn new() -> Self {
        RoleAuthz {
            grants: Mutex::new(HashSet::new()),
        }
    }
    fn grant(&self, subject: &Subject, relation: Relation) {
        self.grants
            .lock()
            .unwrap()
            .insert((subject.as_str().to_string(), relation));
    }
}
#[async_trait]
impl AuthzClient for RoleAuthz {
    async fn check(
        &self,
        subject: &Subject,
        relation: Relation,
        _o: &FgaObject,
        _c: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(self
            .grants
            .lock()
            .unwrap()
            .contains(&(subject.as_str().to_string(), relation)))
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

/// バイト保持の in-memory ObjectStore。
#[derive(Default)]
struct MemStore {
    objects: Mutex<std::collections::HashMap<String, Vec<u8>>>,
}
#[async_trait]
impl storage::object_store::ObjectStore for MemStore {
    async fn ensure_bucket(&self) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn presign_get_internal(
        &self,
        _: &str,
        _: Duration,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://mem".into())
    }
    async fn presign_put(
        &self,
        _: &str,
        _: Duration,
        _: i64,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://mem".into())
    }
    async fn presign_get(
        &self,
        _: &str,
        _: Duration,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://mem".into())
    }
    async fn read_and_hash(&self, _: &str) -> Result<(String, u64), storage::ObjectStoreError> {
        Err(storage::ObjectStoreError::NotFound("mem".into()))
    }
    async fn put_object(
        &self,
        key: &str,
        bytes: Vec<u8>,
        _: &str,
    ) -> Result<(), storage::ObjectStoreError> {
        self.objects.lock().unwrap().insert(key.into(), bytes);
        Ok(())
    }
    async fn get_object(&self, key: &str) -> Result<Vec<u8>, storage::ObjectStoreError> {
        self.objects
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .ok_or_else(|| storage::ObjectStoreError::NotFound(key.into()))
    }
    async fn exists(&self, key: &str) -> Result<bool, storage::ObjectStoreError> {
        Ok(self.objects.lock().unwrap().contains_key(key))
    }
    async fn copy(&self, _: &str, _: &str) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn delete(&self, _: &str) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn list_prefix(
        &self,
        _: &str,
        _: Option<&str>,
    ) -> Result<(Vec<String>, Option<String>), storage::ObjectStoreError> {
        Ok((vec![], None))
    }
    async fn delete_batch(&self, _: &[String]) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
}

fn ctx_for(user: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: user.into(),
            email: None,
            groups: vec!["/acme".into()],
            roles: vec![],
            tenant_id: None,
        },
        "acme".into(),
        "default".into(),
    )
}

struct Env {
    storage: Arc<StorageService>,
    hub: Arc<CollabHub>,
    authz: Arc<RoleAuthz>,
}

async fn setup() -> Option<Env> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool: PgPool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("connect");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    let authz = Arc::new(RoleAuthz::new());
    let authz_dyn: Arc<dyn AuthzClient> = authz.clone();
    let storage = Arc::new(StorageService::new(
        pool.clone(),
        Arc::new(MemStore::default()),
        authz_dyn.clone(),
        Duration::from_secs(120),
        Duration::from_secs(900),
        64 * 1024 * 1024,
    ));
    let hub = Arc::new(CollabHub::new(pool, authz_dyn, storage.clone()));
    Some(Env {
        storage,
        hub,
        authz,
    })
}

/// スライドを作成し node を返す（内容は一意ノンスで dedup 事故を避ける）。
async fn create_slide(env: &Env, owner: &AuthContext) -> (Node, String) {
    env.authz.grant(&owner.subject(), Relation::Member);
    env.authz.grant(&owner.subject(), Relation::Editor);
    env.authz.grant(&owner.subject(), Relation::Viewer);
    let nonce = Uuid::new_v4().to_string();
    let json = SlideDoc {
        meta: collab::note::NoteMeta::default(),
        slides: vec![Slide {
            id: "s1".into(),
            html: format!("<h1>表紙 {nonce}</h1>"),
            notes: String::new(),
            bg: None,
        }],
    }
    .to_json();
    let name = format!("deck-{}.slide", Uuid::new_v4());
    let node = env
        .storage
        .write_file_internal(owner, None, &name, json.as_bytes(), SLIDE_MIME, None)
        .await
        .expect("create slide");
    (node, nonce)
}

/// 受け入れ条件: 人間の並行編集と AI 編集が収束し、保存 JSON に両方乗る（排他なし）。
/// あわせて AI 入力の敵対的 HTML が適用時に落ちること（PIT-40 第1層）。
#[tokio::test]
async fn ai_edit_converges_with_human_edits_and_sanitizes() {
    let Some(env) = setup().await else { return };
    let alice = ctx_for("alice");
    let (node, nonce) = create_slide(&env, &alice).await;

    // 人間の編集セッション（WS 相当）: サーバ状態へ同期した上で s1 を書き換える
    // （実クライアントは sync step1/2 で共有アイテムを得てから編集する）。
    let human_doc = env.hub.join(&alice, &node).await.expect("join");
    let human_update = {
        use yrs::updates::decoder::Decode;
        use yrs::{Doc, ReadTxn, Transact, Update};
        let base = Doc::new();
        {
            let mut txn = base.transact_mut();
            let server_state = human_doc.full_state().expect("server state");
            txn.apply_update(Update::decode_v1(&server_state).expect("decode"))
                .expect("apply server state");
        }
        let before = base.transact().state_vector();
        let slides = base.get_or_insert_array(collab::slide::yjs_doc::SLIDES_ARRAY_NAME);
        {
            let mut txn = base.transact_mut();
            collab::slide::yjs_doc::write_slides(
                &mut txn,
                &slides,
                &[Slide {
                    id: "s1".into(),
                    html: format!("<h1>人間が編集した表紙 {nonce}</h1>"),
                    notes: String::new(),
                    bg: None,
                }],
            );
        }
        let update = base.transact().encode_state_as_update_v1(&before);
        update
    };
    human_doc
        .apply_and_persist(env.hub.store(), &human_update, &alice)
        .await
        .expect("human edit");

    // AI が並行して追加する（敵対的 HTML 込み → サニタイズされて適用される）。
    let report = env
        .hub
        .apply_ai_slide_edit(
            &alice,
            &node,
            &[SlideEditOp::AppendSlide {
                html: format!(r#"<script>alert(1)</script><h2>AI が追加したまとめ {nonce}</h2>"#),
                notes: Some("AI のノート".into()),
            }],
        )
        .await
        .expect("ai edit");
    assert_eq!(report.applied, 1);

    // 人間セッションを終了（最終切断で保存が走る）。
    env.hub.leave(&human_doc).await;

    // 保存された JSON に人間と AI の両方の編集が乗り、script は落ちている。
    let (_n, bytes) = env
        .storage
        .read_file_internal(&alice, node.id, None)
        .await
        .expect("read");
    let saved = String::from_utf8_lossy(&bytes);
    assert!(
        saved.contains(&format!("人間が編集した表紙 {nonce}")),
        "人間の編集が保存される: {saved}"
    );
    assert!(
        saved.contains(&format!("AI が追加したまとめ {nonce}")),
        "AI の編集が保存される: {saved}"
    );
    assert!(!saved.contains("script"), "script が残留: {saved}");
    let parsed = SlideDoc::from_json(&saved).expect("保存 JSON がパース可能");
    assert_eq!(parsed.slides.len(), 2, "2 枚（人間編集＋AI 追加）になる");
}

/// 受け入れ条件: editor 権限のない実行主体の slide.edit が拒否される（viewer は読める）。
#[tokio::test]
async fn ai_slide_edit_denied_without_editor() {
    let Some(env) = setup().await else { return };
    let alice = ctx_for("alice");
    let (node, _) = create_slide(&env, &alice).await;

    // bob は viewer のみ（editor なし）: 読めるが書けない。
    let bob = ctx_for("bob");
    env.authz.grant(&bob.subject(), Relation::Viewer);
    let json = env
        .hub
        .read_slide_json(&bob, &node)
        .await
        .expect("viewer は読める");
    assert!(json.contains("表紙"));

    let err = env
        .hub
        .apply_ai_slide_edit(
            &bob,
            &node,
            &[SlideEditOp::AppendSlide {
                html: "<h2>不正な追加</h2>".into(),
                notes: None,
            }],
        )
        .await;
    assert!(
        matches!(err, Err(collab::CollabError::Forbidden(_))),
        "viewer のみの実行主体は拒否: {err:?}"
    );

    // 何の relation も無い charlie は読むことも出来ない。
    let charlie = ctx_for("charlie");
    let err = env.hub.read_slide_json(&charlie, &node).await;
    assert!(err.is_err(), "無権限の読み取りは拒否");
}

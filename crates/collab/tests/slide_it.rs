//! スライド保存／インポートの結合テスト（Task 11.1・実 Postgres が必要）。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行し、未設定ならスキップする。
//! 検証（phase-11 Task 11.1 受け入れ条件）:
//! - 編集 → 正規化 JSON シリアライズ保存 → 新バージョン＋書込イベント（→RAG 再索引経路）
//! - **WS 直伝搬（サーバ書込サニタイズを通らない経路）の敵対的 HTML が保存 JSON に残らない**
//!   （PIT-40: シリアライズが最終防壁になること）
//! - ファイル側の外部書込 → ロード時インポート（単方向規約）
//! - 不正 JSON のインポートが fail-closed（Yjs 側を壊さない）

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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use collab::slide::{Slide, SlideDoc};
use collab::{saver, DocKind, DocStore, LiveDoc, PersistedDoc, SLIDE_MIME};
use sqlx::{postgres::PgPoolOptions, PgPool};
use storage::StorageService;
use uuid::Uuid;

struct AllowAll;

#[async_trait]
impl AuthzClient for AllowAll {
    async fn check(
        &self,
        _subject: &Subject,
        _relation: Relation,
        _object: &FgaObject,
        _consistency: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn write_tuple(
        &self,
        _subject: &Subject,
        _relation: Relation,
        _object: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn delete_tuple(
        &self,
        _subject: &Subject,
        _relation: Relation,
        _object: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn read_tuples(
        &self,
        _object: &FgaObject,
        _relation: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        Ok(vec![])
    }
    async fn list_objects(
        &self,
        _subject: &Subject,
        _relation: Relation,
        _object_type: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
    async fn delete_object_tuples(&self, _object: &FgaObject) -> Result<u32, AuthzError> {
        Ok(0)
    }
    async fn read_subject_objects(
        &self,
        _subject: &Subject,
        _object_type: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
}

/// バイトを実際に保持する in-memory ObjectStore（保存内容の検証用）。
#[derive(Default)]
struct MemStore {
    objects: Mutex<HashMap<String, Vec<u8>>>,
}

#[async_trait]
impl storage::object_store::ObjectStore for MemStore {
    async fn ensure_bucket(&self) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn presign_get_internal(
        &self,
        _key: &str,
        _ttl: Duration,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://mem/internal".into())
    }
    async fn presign_put(
        &self,
        _key: &str,
        _ttl: Duration,
        _content_length: i64,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://mem/put".into())
    }
    async fn presign_get(
        &self,
        _key: &str,
        _ttl: Duration,
        _filename: Option<&str>,
        _content_type: Option<&str>,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://mem/get".into())
    }
    async fn read_and_hash(&self, _key: &str) -> Result<(String, u64), storage::ObjectStoreError> {
        Err(storage::ObjectStoreError::NotFound("mem".into()))
    }
    async fn put_object(
        &self,
        key: &str,
        bytes: Vec<u8>,
        _content_type: &str,
    ) -> Result<(), storage::ObjectStoreError> {
        self.objects.lock().unwrap().insert(key.to_string(), bytes);
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
    async fn copy(&self, src: &str, dst: &str) -> Result<(), storage::ObjectStoreError> {
        let value = self.objects.lock().unwrap().get(src).cloned();
        if let Some(v) = value {
            self.objects.lock().unwrap().insert(dst.to_string(), v);
        }
        Ok(())
    }
    async fn delete(&self, key: &str) -> Result<(), storage::ObjectStoreError> {
        self.objects.lock().unwrap().remove(key);
        Ok(())
    }
    async fn list_prefix(
        &self,
        _prefix: &str,
        _continuation: Option<&str>,
    ) -> Result<(Vec<String>, Option<String>), storage::ObjectStoreError> {
        Ok((vec![], None))
    }
    async fn delete_batch(&self, keys: &[String]) -> Result<(), storage::ObjectStoreError> {
        let mut objects = self.objects.lock().unwrap();
        for key in keys {
            objects.remove(key);
        }
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
    pool: PgPool,
    storage: Arc<StorageService>,
    store: DocStore,
}

async fn setup() -> Option<Env> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("Postgres 接続");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migration");
    let storage = Arc::new(StorageService::new(
        pool.clone(),
        Arc::new(MemStore::default()),
        Arc::new(AllowAll),
        Duration::from_secs(120),
        Duration::from_secs(900),
        64 * 1024 * 1024,
    ));
    let store = DocStore::new(pool.clone());
    Some(Env {
        pool,
        storage,
        store,
    })
}

fn empty_persisted() -> PersistedDoc {
    PersistedDoc {
        snapshot: None,
        snapshot_seq: 0,
        next_seq: 1,
        updates: vec![],
        saved_node_version: None,
    }
}

fn slide_json(title: &str, html: &str) -> String {
    SlideDoc {
        meta: collab::note::NoteMeta {
            title: Some(title.to_string()),
            ..Default::default()
        },
        slides: vec![Slide {
            id: "s1".into(),
            html: html.to_string(),
            notes: String::new(),
            bg: None,
        }],
    }
    .to_json()
}

/// クライアント Doc でスライドを組み立て、その全状態を update として返す（編集の模擬）。
///
/// **サニタイズを通さず** Yjs 構造へ直接書く＝悪意あるクライアントの WS update と同じ経路。
fn update_for_raw_slides(slides: &[Slide]) -> Vec<u8> {
    use yrs::{Doc, ReadTxn, StateVector, Transact};
    let doc = Doc::new();
    let array = doc.get_or_insert_array(collab::slide::yjs_doc::SLIDES_ARRAY_NAME);
    {
        let mut txn = doc.transact_mut();
        collab::slide::yjs_doc::write_slides(&mut txn, &array, slides);
    }
    let update = doc
        .transact()
        .encode_state_as_update_v1(&StateVector::default());
    update
}

/// 受け入れ条件: 編集内容が正規化 JSON として保存され、書込イベント（→RAG 再索引）が流れる。
/// あわせて **WS 直伝搬の敵対的 HTML が保存 JSON に残らない**こと（PIT-40）を検証する。
#[tokio::test]
async fn edits_are_saved_as_sanitized_json_with_write_event() {
    let Some(env) = setup().await else { return };
    let alice = ctx_for("alice");

    let name = format!("deck-{}.slide", Uuid::new_v4());
    let initial = slide_json("初期", "<h1>初期</h1>");
    let node = env
        .storage
        .write_file_internal(&alice, None, &name, initial.as_bytes(), SLIDE_MIME, None)
        .await
        .expect("スライド作成");
    env.store
        .load_or_init(node.id, "acme", "default")
        .await
        .expect("collab_doc init");

    // 悪意あるクライアントの WS update 相当（サニタイズを通らない書込経路）。
    let unique = Uuid::new_v4();
    let live = Arc::new(
        LiveDoc::restore(node.id, Some(DocKind::Slide), &empty_persisted()).expect("restore"),
    );
    let dirty = vec![Slide {
        id: "s1".into(),
        html: format!(
            r#"<h1>更新 {unique}</h1><script>alert(1)</script><p onclick="x()">本文</p>"#
        ),
        notes: "ノート".into(),
        bg: None,
    }];
    live.apply_and_persist(&env.store, &update_for_raw_slides(&dirty), &alice)
        .await
        .expect("適用");

    // 保存（デバウンス経路の実体を直接呼ぶ）。
    let version = saver::save_doc(&live, &env.store, &env.storage)
        .await
        .expect("保存")
        .expect("dirty だったので保存される");
    assert_eq!(version, node.version + 1, "新バージョンが切られること");

    // 保存内容 = サニタイズ済みの正規化 JSON（script/on* が落ちる）。
    let (saved_node, bytes) = env
        .storage
        .read_file_internal(&alice, node.id, None)
        .await
        .expect("読み戻し");
    assert_eq!(saved_node.version, version);
    let saved = String::from_utf8_lossy(&bytes);
    assert!(saved.contains(&format!("更新 {unique}")));
    assert!(saved.contains("本文"));
    assert!(!saved.contains("script"), "script が保存に残留: {saved}");
    assert!(!saved.contains("onclick"), "onclick が保存に残留: {saved}");
    // 正規形（再パース→再シリアライズで安定）。
    let reparsed = SlideDoc::from_json(&saved).expect("保存 JSON がパース可能");
    assert_eq!(reparsed.to_json(), saved);

    // 書込イベントが outbox に発行されている（op=update・RAG 再索引のトリガ）。
    let (events,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM storage_event_outbox WHERE node_id = $1 AND op = 'update'",
    )
    .bind(node.id)
    .fetch_one(&env.pool)
    .await
    .expect("outbox 照会");
    assert!(events >= 1, "書込イベントが発行されること");

    let persisted = env.store.load(node.id, "default").await.expect("reload");
    assert_eq!(persisted.saved_node_version, Some(version));
}

/// 受け入れ条件: ファイル側の外部書込がロード時インポートで取り込まれる（単方向規約）。
#[tokio::test]
async fn external_write_is_imported_on_load() {
    let Some(env) = setup().await else { return };
    let alice = ctx_for("alice");

    let name = format!("deck-{}.slide", Uuid::new_v4());
    let node = env
        .storage
        .write_file_internal(
            &alice,
            None,
            &name,
            slide_json("v1", &format!("<h1>v1 {}</h1>", Uuid::new_v4())).as_bytes(),
            SLIDE_MIME,
            None,
        )
        .await
        .expect("作成");
    env.store
        .load_or_init(node.id, "acme", "default")
        .await
        .expect("init");

    // 外部書込（エージェントの file write 相当）。敵対的 HTML 込み → インポートで落ちる。
    let unique = Uuid::new_v4();
    let external_json = slide_json(
        "外部更新",
        &format!("<h1>外部 {unique}</h1><iframe src=\"https://evil\"></iframe>"),
    );
    let external = env
        .storage
        .update_file_content_internal(&alice, node.id, external_json.as_bytes(), SLIDE_MIME, None)
        .await
        .expect("外部書込");

    let persisted = env.store.load(node.id, "default").await.expect("load");
    let live =
        Arc::new(LiveDoc::restore(node.id, Some(DocKind::Slide), &persisted).expect("restore"));
    saver::import_if_stale(
        &live,
        &env.store,
        &env.storage,
        &alice,
        node.id,
        external.version,
        persisted.saved_node_version,
        persisted.next_seq - 1,
    )
    .await
    .expect("インポート");

    let serialized = live.serialize_content().expect("シリアライズ");
    assert!(serialized.contains(&format!("外部 {unique}")));
    assert!(
        !serialized.contains("iframe"),
        "インポート経路で iframe が残留: {serialized}"
    );
    let persisted = env.store.load(node.id, "default").await.expect("reload");
    assert_eq!(persisted.saved_node_version, Some(external.version));
}

/// 不正 JSON の外部書込はインポートが fail-closed（エラー・Yjs 側を壊さない）。
#[tokio::test]
async fn corrupt_external_json_fails_closed() {
    let Some(env) = setup().await else { return };
    let alice = ctx_for("alice");

    let name = format!("deck-{}.slide", Uuid::new_v4());
    let unique = Uuid::new_v4();
    let node = env
        .storage
        .write_file_internal(
            &alice,
            None,
            &name,
            slide_json("v1", &format!("<h1>正常 {unique}</h1>")).as_bytes(),
            SLIDE_MIME,
            None,
        )
        .await
        .expect("作成");
    env.store
        .load_or_init(node.id, "acme", "default")
        .await
        .expect("init");

    // まず正常内容をインポートして Yjs 側の真実を作る。
    let persisted = env.store.load(node.id, "default").await.expect("load");
    let live =
        Arc::new(LiveDoc::restore(node.id, Some(DocKind::Slide), &persisted).expect("restore"));
    saver::import_if_stale(
        &live,
        &env.store,
        &env.storage,
        &alice,
        node.id,
        node.version,
        persisted.saved_node_version,
        persisted.next_seq - 1,
    )
    .await
    .expect("初回インポート");
    let before = live.serialize_content().expect("シリアライズ");

    // 壊れた JSON の外部書込 → 次のロードのインポートはエラー（空で上書きしない）。
    let corrupt = env
        .storage
        .update_file_content_internal(&alice, node.id, b"{broken json", SLIDE_MIME, None)
        .await
        .expect("外部書込");
    let persisted = env.store.load(node.id, "default").await.expect("load2");
    let result = saver::import_if_stale(
        &live,
        &env.store,
        &env.storage,
        &alice,
        node.id,
        corrupt.version,
        persisted.saved_node_version,
        persisted.next_seq - 1,
    )
    .await;
    assert!(result.is_err(), "不正 JSON のインポートはエラーになること");
    assert_eq!(
        live.serialize_content().expect("シリアライズ"),
        before,
        "失敗したインポートが Yjs 側を壊さないこと"
    );
}

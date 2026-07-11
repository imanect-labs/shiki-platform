//! ノート保存／インポートの結合テスト（Task 11P.2・実 Postgres が必要）。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行し、未設定ならスキップする。
//! バイト層は in-memory ObjectStore（put/get を実装）で密閉する。検証:
//! - 編集 → md シリアライズ保存 → 新バージョン＋書込イベント（outbox）→ RAG 再索引経路
//! - ファイル側の外部書込 → ロード時インポート（単方向規約）→ snapshot 即時永続化
//! - 保存主体（最終編集者の AuthContext）の editor 認可を write 側で通ること

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
use collab::note::saver;
use collab::{DocStore, LiveDoc, PersistedDoc};
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

/// クライアント Doc で md を組み立て、その全状態を update として返す（編集の模擬）。
fn update_for_markdown(md: &str) -> Vec<u8> {
    use yrs::{Doc, ReadTxn, StateVector, Transact};
    let doc = Doc::new();
    collab::note::import_markdown(&doc, md);
    let update = doc
        .transact()
        .encode_state_as_update_v1(&StateVector::default());
    update
}

/// 受け入れ条件: 編集内容が md として保存され、書込イベント（→RAG 再索引）が流れる。
#[tokio::test]
async fn edits_are_saved_as_markdown_with_write_event() {
    let Some(env) = setup().await else { return };
    let alice = ctx_for("alice");

    // ノート作成（POST /notes 相当の内部書込・org ルート直下）。
    let name = format!("note-{}.md", Uuid::new_v4());
    let node = env
        .storage
        .write_file_internal(&alice, None, &name, b"", "text/markdown", None)
        .await
        .expect("ノート作成");
    env.store
        .load_or_init(node.id, "acme", "default")
        .await
        .expect("collab_doc init");

    // 編集セッション: update を適用（dirty マーク＋update log 追記）。
    // 内容は実行ごとに一意化する（content-addressing の dedup により、過去実行が
    // 同一ハッシュの blob 行を残していると put_object がスキップされるため）。
    let live = Arc::new(LiveDoc::restore(node.id, &empty_persisted()).expect("restore"));
    let canonical = format!("# 会議メモ {}\n\n- 決定事項\n", Uuid::new_v4());
    let canonical = canonical.as_str();
    live.apply_and_persist(&env.store, &update_for_markdown(canonical), &alice)
        .await
        .expect("適用");

    // 保存（デバウンス経路の実体を直接呼ぶ）。
    let version = saver::save_note(&live, &env.store, &env.storage)
        .await
        .expect("保存")
        .expect("dirty だったので保存される");
    assert_eq!(version, node.version + 1, "新バージョンが切られること");

    // 保存内容 = 正規化 md（frontmatter なしの本文）。
    let (saved_node, bytes) = env
        .storage
        .read_file_internal(&alice, node.id, None)
        .await
        .expect("読み戻し");
    assert_eq!(saved_node.version, version);
    assert_eq!(String::from_utf8_lossy(&bytes), canonical);

    // 書込イベントが outbox に発行されている（op=update・RAG 再索引のトリガ）。
    let (events,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM storage_event_outbox WHERE node_id = $1 AND op = 'update'",
    )
    .bind(node.id)
    .fetch_one(&env.pool)
    .await
    .expect("outbox 照会");
    assert!(events >= 1, "書込イベントが発行されること");

    // saved_node_version が更新され、再ロードでインポートが走らない状態になる。
    let persisted = env.store.load(node.id, "default").await.expect("reload");
    assert_eq!(persisted.saved_node_version, Some(version));

    // dirty は消費済み（連続保存は no-op）。
    let again = saver::save_note(&live, &env.store, &env.storage)
        .await
        .expect("2回目");
    assert_eq!(again, None, "dirty でなければ保存しない");
}

/// 受け入れ条件: ファイル側の外部書込が編集セッションと衝突せず取り込まれる。
#[tokio::test]
async fn external_write_is_imported_on_load() {
    let Some(env) = setup().await else { return };
    let alice = ctx_for("alice");
    let agent = ctx_for("agent-fs-write");

    let name = format!("note-{}.md", Uuid::new_v4());
    let node = env
        .storage
        .write_file_internal(
            &alice,
            None,
            &name,
            format!("# v1 {}\n", Uuid::new_v4()).as_bytes(),
            "text/markdown",
            None,
        )
        .await
        .expect("作成");
    env.store
        .load_or_init(node.id, "acme", "default")
        .await
        .expect("init");

    // 外部書込（エージェントの file write 相当・collab を経由しない）。内容は一意化する。
    let external_md = format!("# 外部更新 {}\n\n追記された行。\n", Uuid::new_v4());
    let external = env
        .storage
        .update_file_content_internal(
            &agent,
            node.id,
            external_md.as_bytes(),
            "text/markdown",
            None,
        )
        .await
        .expect("外部書込");
    assert_eq!(external.version, node.version + 1);

    // ロード時インポート（saved_node_version 不一致 → md を Yjs へ全置換）。
    let persisted = env.store.load(node.id, "default").await.expect("load");
    let live = Arc::new(LiveDoc::restore(node.id, &persisted).expect("restore"));
    saver::import_if_stale(
        &live,
        &env.store,
        &env.storage,
        &alice,
        node.id,
        external.version,
        persisted.saved_node_version,
    )
    .await
    .expect("インポート");
    assert_eq!(
        live.to_markdown().expect("md 化"),
        external_md,
        "外部書込の内容が Yjs に取り込まれること"
    );

    // インポート結果は snapshot として即時永続化され、再ロードでも同じ内容になる。
    let persisted = env.store.load(node.id, "default").await.expect("reload");
    assert_eq!(persisted.saved_node_version, Some(external.version));
    assert!(persisted.snapshot.is_some(), "snapshot が書かれること");
    let restored = Arc::new(LiveDoc::restore(node.id, &persisted).expect("restore2"));
    assert_eq!(restored.to_markdown().expect("md 化"), external_md);

    // インポート後の編集セッションが通常どおり収束・保存できる（衝突しない）。
    restored
        .apply_and_persist(&env.store, &update_for_markdown("# 追加編集\n"), &alice)
        .await
        .expect("追記適用");
    let saved = saver::save_note(&restored, &env.store, &env.storage)
        .await
        .expect("保存")
        .expect("保存される");
    assert_eq!(saved, external.version + 1);
}

/// 同一 node.version なら再ロードでインポートは走らない（Yjs 真実の維持）。
#[tokio::test]
async fn import_is_skipped_when_version_matches() {
    let Some(env) = setup().await else { return };
    let alice = ctx_for("alice");
    let name = format!("note-{}.md", Uuid::new_v4());
    let node = env
        .storage
        .write_file_internal(
            &alice,
            None,
            &name,
            format!("# f {}\n", Uuid::new_v4()).as_bytes(),
            "text/markdown",
            None,
        )
        .await
        .expect("作成");
    env.store
        .load_or_init(node.id, "acme", "default")
        .await
        .expect("init");
    env.store
        .set_saved_node_version(node.id, node.version)
        .await
        .expect("saved 記録");

    let live = Arc::new(LiveDoc::restore(node.id, &empty_persisted()).expect("restore"));
    // Yjs 側にのみ存在する内容（ファイルとは異なる）。
    live.apply_update_bytes(&update_for_markdown("# Yjs 側の真実\n"))
        .expect("適用");
    saver::import_if_stale(
        &live,
        &env.store,
        &env.storage,
        &alice,
        node.id,
        node.version,
        Some(node.version),
    )
    .await
    .expect("判定");
    assert_eq!(
        live.to_markdown().expect("md 化"),
        "# Yjs 側の真実\n",
        "version 一致ならインポートで上書きされないこと"
    );
}

//! ワークスペース封じ込めの不変条件テスト（#350）。
//!
//! `StorageWorkspaceStore` が **root フォルダ（起動フォルダ）配下から出られない**ことを固定する:
//! root 外の同名ファイルは解決されない（read/delete/list）・トラバーサル的な名前（`..`・`/` 入り）は
//! 拒否される・書込は常に root 直下に落ちる。詳細は `crates/chat/src/workspace.rs` のモジュール doc。
//!
//! 実 Postgres＋MinIO が必要（`STORAGE_TEST_DATABASE_URL`＋`STORAGE_TEST_S3_ENDPOINT`）。
//! worker/jobq は使わない（アダプタ単体の構造検証・他テストバイナリと jobq を奪い合わない）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;
use std::time::Duration;

use agent_core::{ToolError, WorkspaceStore};
use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use chat::StorageWorkspaceStore;
use sqlx::{postgres::PgPoolOptions, PgPool};
use storage::{object_store::S3Config, ObjectStore, S3ObjectStore, StorageService};

/// 全許可のモック authz（封じ込めが ReBAC ではなく**構造**として成立していることを検証する:
/// 認可が素通りでも root 外へは到達できない）。
struct AllowAll;

#[async_trait]
impl AuthzClient for AllowAll {
    async fn check(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
        _: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(true)
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

async fn setup() -> Option<(PgPool, Arc<StorageService>)> {
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
    let s3_endpoint = std::env::var("STORAGE_TEST_S3_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:9000".into());
    let s3 = S3Config {
        internal_endpoint: s3_endpoint.clone(),
        public_endpoint: s3_endpoint,
        bucket: "shiki-it-blobs".into(),
        access_key: std::env::var("STORAGE_TEST_S3_ACCESS_KEY")
            .unwrap_or_else(|_| "minioadmin".into()),
        secret_key: std::env::var("STORAGE_TEST_S3_SECRET_KEY")
            .unwrap_or_else(|_| "minioadmin".into()),
        region: "us-east-1".into(),
        presign_get_ttl_secs: 300,
        presign_put_ttl_secs: 900,
        cors_allowed_origins: vec![],
    };
    let object_store: Arc<dyn ObjectStore> = Arc::new(S3ObjectStore::new(&s3));
    object_store.ensure_bucket().await.expect("バケット準備");
    let storage = Arc::new(StorageService::new(
        pool.clone(),
        object_store,
        Arc::new(AllowAll),
        Duration::from_mins(5),
        Duration::from_mins(15),
        5 * 1024 * 1024 * 1024,
    ));
    Some((pool, storage))
}

fn ctx(tenant: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        tenant.into(),
    )
}

/// 封じ込め: root 外のファイルは名前が同じでも見えず、トラバーサル名は拒否され、
/// 書込は常に root 直下へ落ちる（#350 の不変条件を固定）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_cannot_escape_root_folder() {
    let Some((_pool, storage)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let c = ctx(&tenant);

    // root（ワークスペース）と兄弟フォルダ（root 外）を用意し、外側に秘密ファイルを置く。
    let root = storage
        .create_folder(&c, None, "ws-root", None)
        .await
        .unwrap();
    let outside = storage
        .create_folder(&c, None, "outside", None)
        .await
        .unwrap();
    storage
        .write_file_at(
            &c,
            outside.id,
            "secret.txt",
            b"outside-only",
            "text/plain",
            None,
        )
        .await
        .unwrap();

    let ws = StorageWorkspaceStore::new(storage.clone(), root.id);

    // ── root 外は名前解決されない（read / delete / list の全経路） ──
    let read = ws.read(&c, "secret.txt", None).await;
    assert!(
        matches!(read, Err(ToolError::Invalid(_))),
        "root 外のファイルは read できない: {read:?}"
    );
    let delete = ws.delete(&c, "secret.txt", None).await;
    assert!(
        matches!(delete, Err(ToolError::Invalid(_))),
        "root 外のファイルは delete できない: {delete:?}"
    );
    assert!(
        ws.list(&c, None).await.unwrap().is_empty(),
        "list は root 直下のみ（外側の secret.txt が漏れない）"
    );
    // 外側のファイルは無傷（存在秘匿＝触れてすらいない）。
    assert!(
        storage
            .resolve_child_file(&c, outside.id, "secret.txt", None)
            .await
            .unwrap()
            .is_some(),
        "外側のファイルは残っている"
    );

    // ── トラバーサル的な名前は拒否される（パス解釈は存在しない） ──
    for bad in ["../secret.txt", "..", "sub/dir.txt", "/etc/passwd"] {
        let w = ws.write(&c, bad, b"x".to_vec(), "text/plain", None).await;
        assert!(
            matches!(w, Err(ToolError::Invalid(_))),
            "不正名 '{bad}' の write は拒否: {w:?}"
        );
        let r = ws.read(&c, bad, None).await;
        assert!(r.is_err(), "不正名 '{bad}' の read は解決されない: {r:?}");
    }

    // ── 書込は常に root 直下・同名の root 内ファイルだけが見える ──
    ws.write(&c, "note.txt", b"inside".to_vec(), "text/plain", None)
        .await
        .unwrap();
    // root 外にも同名ファイルを置いても、ワークスペースからは root 内の実体が読める。
    storage
        .write_file_at(&c, outside.id, "note.txt", b"outside", "text/plain", None)
        .await
        .unwrap();
    assert_eq!(
        ws.read(&c, "note.txt", None).await.unwrap(),
        b"inside".to_vec(),
        "同名でも解決は root 配下のみ"
    );
    let listed = ws.list(&c, None).await.unwrap();
    assert_eq!(listed.len(), 1, "list は root 直下の 1 件のみ");
    assert_eq!(listed[0].name, "note.txt");
    let in_root = storage
        .resolve_child_file(&c, root.id, "note.txt", None)
        .await
        .unwrap();
    assert!(in_root.is_some(), "書込は root 直下に落ちる");
}

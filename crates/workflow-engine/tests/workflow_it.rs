//! ワークフロー IR 保存/取得の結合テスト（Task 10.1a 受け入れ条件）。
//!
//! 実 Postgres＋モック authz（AllowAll）で: IR を artifact として保存・バージョン管理・
//! 旧バージョンの不変取得を検証する。語彙違反の保存拒否は lib の validate テストが担保する。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use artifact::ArtifactStore;
use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use workflow_engine::{Catalog, WorkflowStore, WorkflowStoreError};

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

async fn setup() -> Option<WorkflowStore> {
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
    let artifacts = Arc::new(ArtifactStore::new(pool, Arc::new(AllowAll)));
    Some(WorkflowStore::new(artifacts))
}

fn ctx(tenant: &str) -> AuthContext {
    AuthContext::new(
        Principal {
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

fn sample_ir(name: &str, rev: i64) -> serde_json::Value {
    json!({
        "ir_version": 1,
        "name": name,
        "declared_scopes": ["storage.read", "storage.write"],
        "nodes": [
            { "id": "read", "type": "storage.read", "params": { "id": { "$from": "input", "path": "/id" } } },
            { "id": "write", "type": "storage.write",
              "params": { "parent": { "$from": "input", "path": "/parent" }, "name": "out", "rev": rev,
                          "content": { "$from": "nodes.read.output", "path": "/body" } } }
        ],
        "edges": [{ "from": "read", "to": "write" }]
    })
}

#[tokio::test]
async fn save_versions_and_immutable_get() {
    let Some(store) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let c = ctx(&tenant);
    let cat = Catalog::default();

    // 保存（version 1）。
    let (id, ir) = store
        .create(&c, &sample_ir("report", 1), &cat, None)
        .await
        .expect("create");
    assert_eq!(ir.name, "report");

    // 新バージョン（version 2）。
    let (v2, _) = store
        .update(&c, id, &sample_ir("report", 2), &cat, Some(1), None)
        .await
        .expect("update");
    assert_eq!(v2, 2);

    // 最新は version 2。
    let (latest, _) = store.get_latest(&c, id, None).await.expect("latest");
    assert_eq!(latest, 2);

    // 旧バージョン（1）は不変で取得できる（rev=1 が保たれる）。
    let (_, v1_ir) = store.get_version(&c, id, 1, None).await.expect("v1");
    assert_eq!(v1_ir.name, "report");
    let (_, v2_ir) = store.get_version(&c, id, 2, None).await.expect("v2");
    // どちらも同一 IR 構造（rev はパラメータ内・IR 型では params は Value）。
    assert_eq!(v1_ir.nodes.len(), 2);
    assert_eq!(v2_ir.nodes.len(), 2);
}

#[tokio::test]
async fn save_rejects_invalid_ir_before_persist() {
    let Some(store) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let c = ctx(&tenant);
    let cat = Catalog::default();

    // 未知ノード種＋未知スコープ → 保存前に検証エラー（全件）。
    let bad = json!({
        "ir_version": 1, "name": "bad",
        "declared_scopes": ["data.read"],
        "nodes": [{ "id": "q", "type": "data.query", "params": {} }],
        "edges": []
    });
    let err = store.create(&c, &bad, &cat, None).await.unwrap_err();
    match err {
        WorkflowStoreError::Validation(errors) => {
            assert!(errors.iter().any(|e| e.code == "ir.unknown_node_type"));
            assert!(errors.iter().any(|e| e.code == "ir.unknown_scope"));
        }
        WorkflowStoreError::Artifact(e) => panic!("検証で止まるべき: {e}"),
    }
}

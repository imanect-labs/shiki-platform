//! UI スペック保存路の結合テスト（Task 6.3 受け入れ条件）。
//!
//! - 実 Postgres（`STORAGE_TEST_DATABASE_URL`）: 検証済みのみ保存・版不変・検証拒否の監査
//!   Deny 行・workflow 束縛の解決とバージョンピン。authz はモック（AllowAll）。
//! - 実 OpenFGA（`OPENFGA_TEST_URL` 併設時のみ）: 非共有ユーザーの参照拒否と
//!   「読めない workflow を束縛できない」の実経路検証。

#![allow(
    clippy::pedantic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use artifact::{ArtifactError, ArtifactKind, ArtifactStore, NewArtifact};
use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use gui::{GuiError, SpecValidator, UiSpecStore};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

/// 全許可モック（DB 面のテストで OpenFGA を不要にする）。
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

async fn setup() -> Option<PgPool> {
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

fn unique_tenant() -> String {
    format!("t-{}", uuid::Uuid::new_v4())
}

fn stores(pool: PgPool, authz: Arc<dyn AuthzClient>) -> (Arc<ArtifactStore>, UiSpecStore) {
    let artifacts = Arc::new(ArtifactStore::new(pool.clone(), authz));
    let validator = Arc::new(SpecValidator::new(Arc::clone(&artifacts), pool));
    let store = UiSpecStore::new(Arc::clone(&artifacts), validator);
    (artifacts, store)
}

fn form_spec() -> serde_json::Value {
    json!({
        "version": 1,
        "actions": [ { "type": "handler", "id": "submit", "handler": "chat.submit" } ],
        "root": {
            "component": "form", "id": "f1", "submit": { "action": "submit" },
            "fields": [ { "component": "text_input", "id": "c", "label": "コメント" } ]
        }
    })
}

#[tokio::test]
async fn validated_specs_only_are_saved_and_versions_immutable() {
    let Some(pool) = setup().await else { return };
    let (artifacts, store) = stores(pool.clone(), Arc::new(AllowAll));
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");

    // 検証を通ったスペックのみ保存に進む（6.3）。
    let (id, resolved) = store
        .create(&c, "spec-1", &form_spec(), None)
        .await
        .expect("create");
    assert_eq!(resolved.doc.version, 1);

    // 版追記 → 過去版が不変で取得できる。
    let mut v2_spec = form_spec();
    v2_spec["root"]["fields"][0]["label"] = json!("感想");
    let (v2, _) = store
        .update(&c, id, &v2_spec, Some(1), None)
        .await
        .expect("update");
    assert_eq!(v2, 2);
    let (v, body) = store.get_version(&c, id, 1, None).await.expect("v1");
    assert_eq!(v, 1);
    assert_eq!(body["root"]["fields"][0]["label"], "コメント");

    // 保存された本文は kind=ui_spec の artifact に載る（共通枠の共有・監査へ乗る）。
    let meta = artifacts.get(&c, id, None).await.expect("meta");
    assert_eq!(meta.kind, ArtifactKind::UiSpec);
}

#[tokio::test]
async fn invalid_spec_is_rejected_and_audited() {
    let Some(pool) = setup().await else { return };
    let (_artifacts, store) = stores(pool.clone(), Arc::new(AllowAll));
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");

    // カタログ外コンポーネント → 拒否（保存されない）。
    let bad = json!({ "version": 1, "root": { "component": "iframe", "src": "https://x" } });
    let err = store
        .create(&c, "bad-1", &bad, None)
        .await
        .expect_err("reject");
    let GuiError::Validation(errors) = err else {
        panic!("validation error expected");
    };
    assert!(errors.iter().any(|e| e.code == "gui.unknown_component"));

    // 拒否は監査（ui_spec.validate / deny）に残る（6.12）。
    let denies: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log \
         WHERE tenant_id = $1 AND action = 'ui_spec.validate' AND decision = 'deny'",
    )
    .bind(&tenant)
    .fetch_one(&pool)
    .await
    .expect("audit query");
    assert_eq!(denies, 1);

    // アーティファクトは作られていない。
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM artifact WHERE tenant_id = $1")
        .bind(&tenant)
        .fetch_one(&pool)
        .await
        .expect("artifact query");
    assert_eq!(count, 0);
}

#[tokio::test]
async fn update_rejects_other_kinds() {
    let Some(pool) = setup().await else { return };
    let (artifacts, store) = stores(pool.clone(), Arc::new(AllowAll));
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");

    // 別 kind（script）の artifact を ui-spec エンドポイントで上書きできない。
    let script = artifacts
        .create(
            &c,
            NewArtifact {
                kind: ArtifactKind::Script,
                name: "s1".into(),
                body: json!({ "source": "1" }),
            },
            None,
        )
        .await
        .expect("script create");
    let err = store
        .update(&c, script.id, &form_spec(), None, None)
        .await
        .expect_err("kind mismatch");
    let GuiError::Validation(errors) = err else {
        panic!("validation error expected");
    };
    assert!(errors.iter().any(|e| e.code == "gui.kind_mismatch"));

    // 読み出しも kind 検査で 404 相当。
    let err = store
        .get_latest(&c, script.id, None)
        .await
        .expect_err("not ui_spec");
    assert!(matches!(err, GuiError::Artifact(ArtifactError::NotFound)));
}

#[tokio::test]
async fn workflow_binding_is_resolved_and_version_pinned() {
    let Some(pool) = setup().await else { return };
    let (artifacts, store) = stores(pool.clone(), Arc::new(AllowAll));
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");

    // 参照先 workflow artifact（v1 → v2 と進める）。
    let wf = artifacts
        .create(
            &c,
            NewArtifact {
                kind: ArtifactKind::Workflow,
                name: "wf-ui".into(),
                body: json!({ "rev": 1 }),
            },
            None,
        )
        .await
        .expect("wf create");
    artifacts
        .append_version(&c, wf.id, json!({ "rev": 2 }), Some(1), None)
        .await
        .expect("wf v2");

    // name 参照 → 保存時に artifact_id と current_version（2）がピンされる。
    let spec = json!({
        "version": 1,
        "actions": [ { "type": "workflow", "id": "run", "workflow": { "name": "wf-ui" } } ],
        "root": { "component": "button", "label": "実行", "on_click": { "action": "run" } }
    });
    let (_id, resolved) = store
        .create(&c, "wf-spec", &spec, None)
        .await
        .expect("create");
    let binding = &resolved.json["actions"][0]["workflow"];
    assert_eq!(binding["artifact_id"], json!(wf.id));
    assert_eq!(binding["version"], json!(2));

    // 存在しない名前は解決エラー（存在秘匿の文言）。
    let spec = json!({
        "version": 1,
        "actions": [ { "type": "workflow", "id": "run", "workflow": { "name": "no-such" } } ],
        "root": { "component": "button", "label": "x", "on_click": { "action": "run" } }
    });
    let err = store
        .create(&c, "wf-spec-2", &spec, None)
        .await
        .expect_err("unresolved");
    let GuiError::Validation(errors) = err else {
        panic!("validation error expected");
    };
    assert!(errors
        .iter()
        .any(|e| e.code == "gui.action_workflow_unresolved"));

    // ui_spec への workflow 参照は他種を掴めない（script を name にしても不発）。
    artifacts
        .create(
            &c,
            NewArtifact {
                kind: ArtifactKind::Script,
                name: "script-x".into(),
                body: json!({}),
            },
            None,
        )
        .await
        .expect("script");
    let spec = json!({
        "version": 1,
        "actions": [ { "type": "workflow", "id": "run", "workflow": { "name": "script-x" } } ],
        "root": { "component": "button", "label": "x", "on_click": { "action": "run" } }
    });
    assert!(store.create(&c, "wf-spec-3", &spec, None).await.is_err());
}

/// 実 OpenFGA: 非共有ユーザーの参照拒否・読めない workflow を束縛できない。
#[tokio::test]
async fn live_fga_sharing_and_unreadable_workflow_binding() {
    let Some(pool) = setup().await else { return };
    let Ok(base_url) = std::env::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return;
    };
    use authz::client::{OpenFgaClient, OpenFgaConfig};
    let model: serde_json::Value =
        serde_json::from_str(include_str!("../../authz/model/authorization-model.json"))
            .expect("model json");
    let config = OpenFgaConfig {
        base_url,
        store_name: format!("shiki-gui-test-{}", uuid::Uuid::new_v4()),
    };
    let fga = OpenFgaClient::connect(reqwest::Client::new(), &config, &model)
        .await
        .expect("OpenFGA 接続");
    let (artifacts, store) = stores(pool.clone(), Arc::new(fga));

    let tenant = unique_tenant();
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");

    // alice が保存 → bob（非共有）は読めない（6.1 の共有枠に乗っている）。
    let (id, _) = store
        .create(&alice, "spec-share", &form_spec(), None)
        .await
        .expect("create");
    let err = store
        .get_latest(&bob, id, None)
        .await
        .expect_err("forbidden");
    assert!(matches!(err, GuiError::Artifact(ArtifactError::Forbidden)));

    // alice の workflow を bob は束縛できない（viewer 不足 → unresolved）。
    artifacts
        .create(
            &alice,
            NewArtifact {
                kind: ArtifactKind::Workflow,
                name: "wf-private".into(),
                body: json!({ "rev": 1 }),
            },
            None,
        )
        .await
        .expect("wf create");
    let spec = json!({
        "version": 1,
        "actions": [ { "type": "workflow", "id": "run", "workflow": { "name": "wf-private" } } ],
        "root": { "component": "button", "label": "x", "on_click": { "action": "run" } }
    });
    let err = store
        .create(&bob, "bob-spec", &spec, None)
        .await
        .expect_err("unresolved");
    let GuiError::Validation(errors) = err else {
        panic!("validation error expected");
    };
    assert!(errors
        .iter()
        .any(|e| e.code == "gui.action_workflow_unresolved"));
}

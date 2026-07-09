//! skill / ミニアプリの結合テスト（Task 6.7 / 6.10 受け入れ条件）。
//!
//! - 実 Postgres（`STORAGE_TEST_DATABASE_URL`）: 作成・新版追記・過去版不変・body 検証・
//!   kind 不一致拒否・ピン解決。
//! - 実 OpenFGA（`OPENFGA_TEST_URL` 併設時のみ）: ロール共有 → **部品を個別共有せずに**
//!   共有相手がミニアプリを解決できる（バンドル権限）・部品への直接アクセスは依然 403・
//!   旧版ピンの再現性。

#![allow(
    clippy::pedantic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use artifact::{ArtifactError, ArtifactKind, ArtifactRole, ArtifactStore, NewArtifact};
use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use gui::{GuiError, MiniAppStore, SkillStore, SpecValidator, UiSpecStore};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

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
    format!("t-{}", Uuid::new_v4())
}

struct Stores {
    artifacts: Arc<ArtifactStore>,
    skills: SkillStore,
    ui_specs: UiSpecStore,
    mini_apps: MiniAppStore,
}

fn stores(pool: PgPool, authz: Arc<dyn AuthzClient>) -> Stores {
    let artifacts = Arc::new(ArtifactStore::new(pool.clone(), authz));
    let validator = Arc::new(SpecValidator::new(Arc::clone(&artifacts), pool.clone()));
    Stores {
        skills: SkillStore::new(Arc::clone(&artifacts)),
        ui_specs: UiSpecStore::new(Arc::clone(&artifacts), validator),
        mini_apps: MiniAppStore::new(Arc::clone(&artifacts), pool),
        artifacts,
    }
}

fn skill_body(label: &str) -> serde_json::Value {
    json!({
        "description": format!("経費精算のアシスタント（{label}）"),
        "instructions": "あなたは経費規程に詳しいアシスタントです。規程に基づいて回答してください。",
        "knowledge_scope": { "folders": [Uuid::new_v4()], "files": [] },
        "allowed_tools": ["doc_search", "emit_ui"],
        "model": { "model": "skill-model", "temperature": 0.2, "max_tokens": 1024 },
        "few_shot": [ { "user": "交通費は？", "assistant": "領収書が必要です。" } ],
        "scripts": [
            { "path": "scripts/summarize.shiki", "kind": "shiki", "source": "function main(){}" },
            { "path": "scripts/export.sh", "kind": "shell", "source": "#!/bin/sh\necho ok" }
        ],
        "references": []
    })
}

fn button_spec() -> serde_json::Value {
    json!({
        "version": 1,
        "actions": [ { "type": "handler", "id": "submit", "handler": "chat.submit" } ],
        "root": {
            "component": "form", "id": "f1", "submit": { "action": "submit" },
            "fields": [ { "component": "text_input", "id": "c", "label": "内容" } ]
        }
    })
}

#[tokio::test]
async fn skill_create_update_and_immutable_versions() {
    let Some(pool) = setup().await else { return };
    let s = stores(pool.clone(), Arc::new(AllowAll));
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");

    // 作成（version 1）→ 新版追記 → 過去版が不変で取得できる（6.7 受け入れ条件）。
    let (id, body) = s
        .skills
        .create(&c, "expense-skill", &skill_body("v1"), None)
        .await
        .expect("create");
    assert_eq!(body.few_shot.len(), 1);
    assert_eq!(
        body.scripts.len(),
        2,
        "shiki と shell の両 script を保持できる"
    );

    let (v2, _) = s
        .skills
        .update(&c, id, &skill_body("v2"), Some(1), None)
        .await
        .expect("update");
    assert_eq!(v2, 2);
    let (v, body_v1, _raw) = s.skills.get_version(&c, id, 1, None).await.expect("v1");
    assert_eq!(v, 1);
    assert!(body_v1.description.contains("v1"), "過去版は不変");

    // 不正 body は 拒否される（未知ツール名・パストラバーサル・空 instructions）。
    for (bad, code) in [
        (
            json!({ "description": "x", "instructions": "y", "allowed_tools": ["rm_rf"] }),
            "skill.schema_violation",
        ),
        (
            json!({ "description": "x", "instructions": "y",
                    "scripts": [ { "path": "../evil.sh", "kind": "shell", "source": "x" } ] }),
            "skill.invalid_script_path",
        ),
        (
            json!({ "description": "x", "instructions": "  " }),
            "skill.empty_instructions",
        ),
        (
            json!({ "description": "x", "instructions": "y",
                    "scripts": [ { "path": "scripts/a.sh", "kind": "shiki", "source": "x" } ] }),
            "skill.script_kind_mismatch",
        ),
        (
            json!({ "description": "x", "instructions": "y",
                    "knowledge_scope": { "folders": [], "files": [] } }),
            "skill.empty_scope",
        ),
    ] {
        let err = s
            .skills
            .create(&c, &format!("bad-{}", Uuid::new_v4()), &bad, None)
            .await
            .expect_err("reject");
        let GuiError::Validation(errors) = err else {
            panic!("validation error expected");
        };
        assert!(
            errors.iter().any(|e| e.code == code),
            "expected {code}, got {errors:?}"
        );
    }

    // 別 kind を skill エンドポイントで上書きできない。
    let script = s
        .artifacts
        .create(
            &c,
            NewArtifact {
                kind: ArtifactKind::Script,
                name: "s1".into(),
                body: json!({}),
            },
            None,
        )
        .await
        .expect("script");
    assert!(s
        .skills
        .update(&c, script.id, &skill_body("x"), None, None)
        .await
        .is_err());
}

#[tokio::test]
async fn miniapp_pins_validated_and_resolve_rechecks() {
    let Some(pool) = setup().await else { return };
    let s = stores(pool.clone(), Arc::new(AllowAll));
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");

    let (skill_id, _) = s
        .skills
        .create(&c, "app-skill", &skill_body("app"), None)
        .await
        .expect("skill");
    let (spec_id, _) = s
        .ui_specs
        .create(&c, "app-spec", &button_spec(), None)
        .await
        .expect("spec");
    let wf = s
        .artifacts
        .create(
            &c,
            NewArtifact {
                kind: ArtifactKind::Workflow,
                name: "app-wf".into(),
                body: json!({ "rev": 1 }),
            },
            None,
        )
        .await
        .expect("wf");

    // 作成（ピン解決・kind 検査）→ resolve が検証済み一式を返す（6.10 受け入れ条件①）。
    let body = json!({
        "description": "経費精算アプリ",
        "ui_spec": { "artifact_id": spec_id, "version": 1 },
        "skill": { "artifact_id": skill_id, "version": 1 },
        "workflows": [ { "alias": "approve", "artifact_id": wf.id, "version": 1 } ]
    });
    let (app_id, _) = s
        .mini_apps
        .create(&c, "expense-app", &body, None)
        .await
        .expect("create");
    let resolved = s
        .mini_apps
        .resolve(&c, app_id, None, None)
        .await
        .expect("resolve");
    assert_eq!(resolved.version, 1);
    assert_eq!(resolved.ui_spec.version, 1);
    assert!(resolved.skill.is_some());

    // 存在しないピン・kind 不一致ピンは保存時に拒否される。
    let bad = json!({
        "description": "x",
        "ui_spec": { "artifact_id": Uuid::new_v4(), "version": 1 }
    });
    let err = s
        .mini_apps
        .create(&c, "bad-app", &bad, None)
        .await
        .expect_err("unresolved pin");
    let GuiError::Validation(errors) = err else {
        panic!("validation error expected");
    };
    assert!(errors.iter().any(|e| e.code == "miniapp.pin_unresolved"));

    // UI スペックの workflow 束縛がバンドル外なら拒否される（束縛 ⊆ ピン集合）。
    let wf_spec = json!({
        "version": 1,
        "actions": [ { "type": "workflow", "id": "run", "workflow": { "name": "app-wf" } } ],
        "root": { "component": "button", "label": "実行", "on_click": { "action": "run" } }
    });
    let (wf_spec_id, _) = s
        .ui_specs
        .create(&c, "wf-spec", &wf_spec, None)
        .await
        .expect("wf spec");
    let unbundled = json!({
        "description": "x",
        "ui_spec": { "artifact_id": wf_spec_id, "version": 1 },
        "workflows": []
    });
    let err = s
        .mini_apps
        .create(&c, "unbundled-app", &unbundled, None)
        .await
        .expect_err("binding not bundled");
    let GuiError::Validation(errors) = err else {
        panic!("validation error expected");
    };
    assert!(errors
        .iter()
        .any(|e| e.code == "miniapp.binding_not_bundled"));
}

/// 実 OpenFGA: ロール共有 → 部品を個別共有せずに共有相手が resolve できる・
/// 部品への直接アクセスは依然 403・旧版ピンの再現性。
#[tokio::test]
async fn live_fga_bundle_authority_and_version_pinning() {
    let Some(pool) = setup().await else { return };
    let Ok(base_url) = std::env::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return;
    };
    use authz::client::{OpenFgaClient, OpenFgaConfig};
    let fga = Arc::new(
        OpenFgaClient::connect(
            reqwest::Client::new(),
            &OpenFgaConfig {
                base_url,
                store_name: format!("shiki-miniapp-test-{}", Uuid::new_v4()),
            },
            &authz::model::default_model(),
        )
        .await
        .expect("OpenFGA 接続"),
    ) as Arc<dyn AuthzClient>;
    let s = stores(pool.clone(), Arc::clone(&fga));

    let tenant = unique_tenant();
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");

    // alice が部品＋ミニアプリを作成し、v2 へ改訂する。
    let (skill_id, _) = s
        .skills
        .create(&alice, "share-skill", &skill_body("v1"), None)
        .await
        .expect("skill");
    let (spec_id, _) = s
        .ui_specs
        .create(&alice, "share-spec", &button_spec(), None)
        .await
        .expect("spec");
    let body_v1 = json!({
        "description": "v1 アプリ",
        "ui_spec": { "artifact_id": spec_id, "version": 1 },
        "skill": { "artifact_id": skill_id, "version": 1 },
        "workflows": []
    });
    let (app_id, _) = s
        .mini_apps
        .create(&alice, "share-app", &body_v1, None)
        .await
        .expect("create");
    let mut spec_v2 = button_spec();
    spec_v2["root"]["fields"][0]["label"] = json!("感想");
    s.ui_specs
        .update(&alice, spec_id, &spec_v2, Some(1), None)
        .await
        .expect("spec v2");
    let body_v2 = json!({
        "description": "v2 アプリ",
        "ui_spec": { "artifact_id": spec_id, "version": 2 },
        "skill": { "artifact_id": skill_id, "version": 1 },
        "workflows": []
    });
    s.mini_apps
        .update(&alice, app_id, &body_v2, Some(1), None)
        .await
        .expect("app v2");

    // 共有前: bob は resolve できない。
    assert!(matches!(
        s.mini_apps.resolve(&bob, app_id, None, None).await,
        Err(GuiError::Artifact(ArtifactError::Forbidden))
    ));

    // ミニアプリ**本体だけ**を bob に共有（部品は共有しない）。
    s.artifacts
        .share(
            &alice,
            app_id,
            &storage::ShareTarget::User {
                id: "bob".to_string(),
            },
            ArtifactRole::Viewer,
            None,
        )
        .await
        .expect("share");

    // 共有相手が resolve できる（6.10 受け入れ条件②・部品はバンドル権限で読む）。
    let resolved = s
        .mini_apps
        .resolve(&bob, app_id, None, None)
        .await
        .expect("bob resolve");
    assert_eq!(resolved.version, 2);
    assert_eq!(
        resolved.ui_spec_json["root"]["fields"][0]["label"], "感想",
        "current は v2 の ui_spec"
    );
    assert!(resolved.skill.is_some(), "skill もバンドル権限で読める");

    // 旧版ピンの resolve は旧 ui_spec を返す（再現性・6.10 受け入れ条件①）。
    let resolved_v1 = s
        .mini_apps
        .resolve(&bob, app_id, Some(1), None)
        .await
        .expect("bob resolve v1");
    assert_eq!(
        resolved_v1.ui_spec_json["root"]["fields"][0]["label"], "内容",
        "旧版ピンは旧 ui_spec のまま"
    );

    // 部品への**直接**アクセスは依然 403（バンドル越しのみ・アンビエント権限なし）。
    assert!(matches!(
        s.ui_specs.get_latest(&bob, spec_id, None).await,
        Err(GuiError::Artifact(ArtifactError::Forbidden))
    ));
    assert!(matches!(
        s.skills.get_latest(&bob, skill_id, None).await,
        Err(GuiError::Artifact(ArtifactError::Forbidden))
    ));

    // バンドル読みの監査が残る（6.12）。
    let via_bundle: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log \
         WHERE tenant_id = $1 AND action = 'artifact.read_via_bundle' AND actor = 'bob'",
    )
    .bind(&tenant)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        via_bundle >= 2,
        "ui_spec と skill のバンドル読みが監査に残る"
    );
    let resolves: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log \
         WHERE tenant_id = $1 AND action = 'miniapp.resolve' AND actor = 'bob'",
    )
    .bind(&tenant)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(resolves >= 1, "miniapp.resolve が監査に残る");
}

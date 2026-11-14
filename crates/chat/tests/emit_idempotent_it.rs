//! emit_workflow の作成の冪等（R1・7p-10）の結合テスト。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）の WorkflowStore（artifact）に、道具を直接 2 回呼ぶ。認可は全許可。
//! - 同じ run（`since` 以降）で同じ IR を作り直すと、2 回目は作成済みとして今の版を返し、版は増えない。
//! - 内容が違う作り直しは保存されず、is_error の観測が id と版を添えて直し方を言う。
//! - run の前からある同名・ほかの人の同名は、これまでどおり作れなかった旨（持ち主・id を出さない）。

#![allow(
    clippy::pedantic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use agent_core::{Tool, ToolOutcome};
use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use chat::builder_obs::{BuilderKind, BuilderRef};
use chat::EmitWorkflowTool;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use uuid::Uuid;
use workflow_engine::{Catalog, WorkflowStore};

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

/// 空のカタログ（保存 API と同じ形の材料・モデルは照合しない）。
struct EmptyCatalog;

#[async_trait]
impl chat::WorkflowCatalogSource for EmptyCatalog {
    async fn catalog(&self, _ctx: &AuthContext) -> Result<Catalog, String> {
        Ok(Catalog::default())
    }
}

async fn setup() -> Option<PgPool> {
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

/// 保存の検証（V1〜V7）を通る IR（`declared_scopes` は保存が補う）。
fn ir(name: &str, body: &str) -> Value {
    json!({
        "ir_version": 1,
        "name": name,
        "display_name": "経費の確認",
        "triggers": [{ "kind": "interactive" }],
        "nodes": [{
            "id": "compute",
            "type": "script.run",
            "params": { "source": { "inline": format!("function main(i){{ return {{ ok: {body} }}; }}") } }
        }],
        "edges": []
    })
}

struct Fixture {
    pool: PgPool,
    artifacts: Arc<artifact::ArtifactStore>,
    workflows: Arc<WorkflowStore>,
    tenant: String,
}

impl Fixture {
    async fn new() -> Option<Self> {
        let pool = setup().await?;
        let artifacts = Arc::new(artifact::ArtifactStore::new(
            pool.clone(),
            Arc::new(AllowAll),
        ));
        let workflows = Arc::new(WorkflowStore::new(Arc::clone(&artifacts)));
        Some(Fixture {
            pool,
            artifacts,
            workflows,
            tenant: format!("t-{}", Uuid::new_v4()),
        })
    }

    /// run が `since` に作られた会話の emit_workflow。
    fn tool(&self, since: DateTime<Utc>) -> EmitWorkflowTool {
        EmitWorkflowTool::new(Arc::clone(&self.workflows), Arc::new(EmptyCatalog))
            .with_created_since(Arc::clone(&self.artifacts), since)
    }

    async fn now(&self) -> DateTime<Utc> {
        sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    /// 生きている同名のワークフローの (id, 今の版, 版の数)。
    async fn stored(&self, name: &str) -> (Uuid, i64, i64) {
        sqlx::query_as(
            "SELECT a.id, a.current_version, \
                    (SELECT count(*) FROM artifact_version v \
                      WHERE v.tenant_id = a.tenant_id AND v.artifact_id = a.id) \
             FROM artifact a \
             WHERE a.tenant_id = $1 AND a.kind = 'workflow' AND a.name = $2 AND a.deleted_at IS NULL",
        )
        .bind(&self.tenant)
        .bind(name)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }
}

async fn emit(tool: &EmitWorkflowTool, ctx: &AuthContext, ir: Value) -> ToolOutcome {
    tool.call(ctx, json!({ "ir": ir }), None).await.unwrap()
}

/// 同じ run で同じ IR を作り直すと、2 回目は作成済みとして今の版（v1）を返し、版は増えない。参照の行と参照カードが
/// 付く。キーの順や、保存が補う declared_scopes の有無は内容の違いに数えない。
#[tokio::test]
async fn same_ir_in_the_same_run_returns_the_existing_version() {
    let Some(fx) = Fixture::new().await else {
        return;
    };
    let alice = ctx(&fx.tenant, "alice");
    let tool = fx.tool(fx.now().await);

    let first = emit(&tool, &alice, ir("expense-check", "true")).await;
    assert!(!first.is_error, "{}", first.content);
    let (id, version, versions) = fx.stored("expense-check").await;
    assert_eq!((version, versions), (1, 1));
    // 作成の成功の観測も、末尾に参照の行を持つ。
    let line = BuilderRef::new(BuilderKind::Workflow, id, "expense-check", 1).line();
    assert!(first.content.ends_with(&line), "{}", first.content);

    // 2 回目（モデルの作り直し・run の再試行）。declared_scopes を明示しても同じ内容。
    let mut again = ir("expense-check", "true");
    again["declared_scopes"] = json!([]);
    let second = emit(&tool, &alice, again).await;
    assert!(!second.is_error, "{}", second.content);
    assert!(
        second
            .content
            .contains("あなたがすでに作っています。新しい版は作っていません"),
        "{}",
        second.content
    );
    assert!(second.content.ends_with(&line), "{}", second.content);
    assert_eq!(
        second.builder_refs,
        [BuilderRef::new(BuilderKind::Workflow, id, "expense-check", 1).to_value()]
    );
    assert!(second.workflow_refs.is_empty());
    assert_eq!(fx.stored("expense-check").await, (id, 1, 1), "版が増えない");
}

/// 内容の違う作り直しは保存されず、is_error の観測が id と版を添えて、workflow_id と expected_version で保存し直す
/// よう言う。版は増えない。
#[tokio::test]
async fn different_ir_in_the_same_run_is_not_saved() {
    let Some(fx) = Fixture::new().await else {
        return;
    };
    let alice = ctx(&fx.tenant, "alice");
    let tool = fx.tool(fx.now().await);

    assert!(
        !emit(&tool, &alice, ir("expense-check", "true"))
            .await
            .is_error
    );
    let (id, _, _) = fx.stored("expense-check").await;
    let changed = emit(&tool, &alice, ir("expense-check", "false")).await;
    assert!(changed.is_error, "{}", changed.content);
    assert!(
        changed.content.contains(&format!("id: {id}, v1")),
        "{}",
        changed.content
    );
    assert!(
        changed.content.contains("今回の内容は保存していません"),
        "{}",
        changed.content
    );
    assert!(
        changed
            .content
            .contains(&format!("workflow_id に {id} を、expected_version に 1 を")),
        "{}",
        changed.content
    );
    assert!(changed.builder_refs.is_empty() && changed.workflow_refs.is_empty());
    assert_eq!(fx.stored("expense-check").await, (id, 1, 1), "保存しない");
}

/// run の前からある本人の同名と、ほかの人の同名は、これまでどおり作れなかった旨を返す（id は出さない）。
#[tokio::test]
async fn names_taken_before_the_run_or_by_others_are_refused_as_before() {
    let Some(fx) = Fixture::new().await else {
        return;
    };
    let alice = ctx(&fx.tenant, "alice");
    let bob = ctx(&fx.tenant, "bob");

    // 前の run で作ったもの。
    let earlier = fx.tool(fx.now().await);
    assert!(
        !emit(&earlier, &alice, ir("expense-check", "true"))
            .await
            .is_error
    );
    let (id, _, _) = fx.stored("expense-check").await;

    let later = fx.tool(fx.now().await);
    for (who, label) in [
        (&alice, "run の前からある本人の同名"),
        (&bob, "ほかの人の同名"),
    ] {
        let out = emit(&later, who, ir("expense-check", "true")).await;
        assert!(out.is_error, "{label}: {}", out.content);
        assert!(
            out.content
                .starts_with("ワークフローを作れませんでした。name が「expense-check」のワークフローがすでにあります"),
            "{label}: {}",
            out.content
        );
        assert!(
            !out.content.contains(&id.to_string()),
            "{label}: {}",
            out.content
        );
        assert!(out.builder_refs.is_empty());
    }
    assert_eq!(fx.stored("expense-check").await, (id, 1, 1));
}

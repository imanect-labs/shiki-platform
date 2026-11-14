//! 会話の AI がテーブルを作る・直す道具（data_table_create / data_table_update・D-10 の T05）の結合テスト。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）＋実 OpenFGA（`OPENFGA_TEST_URL`）で次を確かめる。
//!
//! - 作る: 列・行の決まり・状態の進め方つきで作れ、持ち主は話した本人だけ（ほかの人には見えない）。状態の列の選択肢は
//!   状態の一覧になり、申請者の書き込みの決まりが付く。観測の末尾に参照の行・参照カードが付く。
//! - 作成の冪等（R1）: 同じ会話の run で同じ名前・同じ列なら作り直さず、列が違えば直し方を返す。ほかの人の同名は
//!   「名前が使えない」だけを言う。上限（1 回の run で 5 表）を超えて作らない。対称のリンクは受けない（R6）。
//! - 直す: 足す列・変える列だけを重ね、版で楽観ロックする。名前で呼んだら id を示して直さない。持ち主でなければ
//!   直せない。行の決まりを入れ替えても申請者の書き込みの決まりは残る。書き換えられなくなるレコードが出る状態の
//!   進め方は保存しない。
//! - どちらも承認カードを経る（`requires_confirmation`）。

#![allow(
    clippy::pedantic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use agent_core::{Tool, ToolOutcome};
use artifact::ArtifactStore;
use async_trait::async_trait;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Consistency, Principal, Relation};
use chat::{
    BuilderRun, DataBuilderDeps, DataTableCreateTool, DataTableUpdateTool, DataViewSaveTool,
    ViewBuilderDeps,
};
use chrono::Utc;
use data::{DataStore, DataTable, DataViewStore, FieldType, FsmStore, RefResolver};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
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

struct Env {
    pool: PgPool,
    fga: Arc<dyn AuthzClient>,
    data: DataStore,
    deps: DataBuilderDeps,
    views: ViewBuilderDeps,
    tenant: String,
}

impl Env {
    async fn new() -> Option<Self> {
        let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
            eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
            return None;
        };
        let Ok(base_url) = shiki_testenv::var("OPENFGA_TEST_URL") else {
            eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
            return None;
        };
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect(&db_url)
            .await
            .expect("Postgres へ接続できること");
        sqlx::migrate!("../../migrations")
            .run(&pool)
            .await
            .expect("マイグレーション適用");
        let fga: Arc<dyn AuthzClient> = Arc::new(
            OpenFgaClient::connect(
                reqwest::Client::new(),
                &OpenFgaConfig {
                    base_url,
                    store_name: format!("shiki-chat-data-builder-{}", Uuid::new_v4()),
                },
                &authz::model::default_model(),
            )
            .await
            .expect("OpenFGA 接続"),
        );
        let data = DataStore::new(pool.clone(), Arc::clone(&fga), Arc::new(AllowRefs));
        let artifacts = Arc::new(ArtifactStore::new(pool.clone(), Arc::clone(&fga)));
        let fsms = Arc::new(FsmStore::new(Arc::clone(&artifacts), data.clone()));
        let views = ViewBuilderDeps {
            data: data.clone(),
            views: Arc::new(DataViewStore::new(Arc::clone(&artifacts), data.clone())),
            artifacts,
        };
        let deps = DataBuilderDeps {
            data: data.clone(),
            fsms,
            approvals: Arc::new(workflow_engine::ApprovalStore::new(pool.clone())),
        };
        Some(Env {
            pool,
            fga,
            data,
            deps,
            views,
            tenant: format!("t{}", Uuid::new_v4().simple()),
        })
    }

    fn user(&self, name: &str) -> AuthContext {
        ctx(&self.tenant, name)
    }

    fn create_tool(&self) -> DataTableCreateTool {
        DataTableCreateTool::new(
            self.deps.clone(),
            BuilderRun {
                since: Utc::now() - chrono::Duration::seconds(1),
            },
        )
    }

    fn update_tool(&self) -> DataTableUpdateTool {
        DataTableUpdateTool::new(self.deps.clone())
    }

    fn view_tool(&self) -> DataViewSaveTool {
        DataViewSaveTool::new(
            self.views.clone(),
            BuilderRun {
                since: Utc::now() - chrono::Duration::seconds(1),
            },
        )
    }
}

/// 経費の申請（件名・金額・区分・申請者・状態）。経理が承認・却下する。
fn expense_input(name: &str) -> Value {
    json!({
        "name": name,
        "fields": [
            { "key": "title", "display_name": "件名", "type": "text", "required": true },
            { "key": "amount", "display_name": "金額", "type": "number", "required": true },
            { "key": "kind", "display_name": "区分", "type": "select", "options": ["交通費", "備品"] },
            { "key": "applicant", "display_name": "申請者", "type": "created_by" },
            // 状態の列は選択肢を書かなくてよい（状態の一覧になる）。
            { "key": "status", "display_name": "状態", "type": "select" }
        ],
        "row_policy": { "read": { "any": [
            { "field_cmp": { "field": "applicant", "op": "eq", "value": "user_id" } },
            { "has_role": { "role": "経理", "subtree": true } }
        ] } },
        "fsm": {
            "status_field": "status",
            "states": ["申請中", "承認", "却下"],
            "transitions": [
                { "from": "申請中", "to": "承認", "actor": { "has_role": { "role": "経理", "subtree": true } } },
                { "from": "申請中", "to": "却下", "actor": { "has_role": { "role": "経理", "subtree": true } } }
            ],
            "applicant_write_lock": "applicant"
        }
    })
}

/// 観測の末尾の参照の行（`参照: {...}`）。
fn ref_of(o: &ToolOutcome) -> Value {
    let line = o
        .content
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("参照: "))
        .unwrap_or_else(|| panic!("参照の行が無い: {}", o.content));
    serde_json::from_str(line).unwrap()
}

async fn call(tool: &dyn Tool, who: &AuthContext, input: Value) -> ToolOutcome {
    tool.call(who, input, Some("trace-data-builder"))
        .await
        .expect("道具の誤りは観測で返る")
}

async fn table_of(env: &Env, who: &AuthContext, o: &ToolOutcome) -> DataTable {
    let id: Uuid = ref_of(o)["id"].as_str().unwrap().parse().unwrap();
    env.data.get_table(who, id, None).await.unwrap()
}

#[tokio::test]
async fn creates_a_table_with_policy_and_fsm_owned_only_by_the_speaker() {
    let Some(env) = Env::new().await else { return };
    let alice = env.user("alice");
    let bob = env.user("bob");
    let tool = env.create_tool();
    assert!(tool.requires_confirmation(), "作成は承認カードを経る");

    let o = call(&tool, &alice, expense_input("経費申請")).await;
    assert!(!o.is_error, "{}", o.content);
    assert!(
        o.content.contains("テーブル「経費申請」を作りました"),
        "{}",
        o.content
    );
    assert!(
        o.content
            .contains("状態の進め方: 列 status・最初の状態「申請中」"),
        "{}",
        o.content
    );
    let r = ref_of(&o);
    assert_eq!(r["kind"], "data_table");
    assert_eq!(r["name"], "経費申請");
    assert_eq!(o.builder_refs, std::slice::from_ref(&r));

    let table = table_of(&env, &alice, &o).await;
    assert_eq!(r["version"], table.schema_version);
    assert_eq!(table.created_by, "alice");
    let status = table.schema.field("status").unwrap();
    assert_eq!(
        status.options,
        ["申請中", "承認", "却下"],
        "選択肢は状態の一覧"
    );
    assert_eq!(table.schema.status_field.as_deref(), Some("status"));
    assert!(table.schema.fsm_ref.is_some());
    let fsm = env
        .deps
        .fsms
        .table_fsm(&alice, table.id, None)
        .await
        .unwrap();
    assert_eq!(fsm.applicant_write_lock.as_deref(), Some("applicant"));
    assert!(table.schema.row_policy.is_some());

    // 持ち主は alice だけ。bob には見えない（共有はしない）。
    let obj = alice.ns().data_table(&table.id.to_string());
    assert!(env
        .fga
        .check(
            &alice.subject(),
            Relation::Owner,
            &obj,
            Consistency::HigherConsistency
        )
        .await
        .unwrap());
    assert!(!env
        .fga
        .check(
            &bob.subject(),
            Relation::Viewer,
            &obj,
            Consistency::HigherConsistency
        )
        .await
        .unwrap());
    assert!(env.data.get_table(&bob, table.id, None).await.is_err());
}

#[tokio::test]
async fn creating_again_in_the_same_run_is_idempotent_and_others_names_are_refused() {
    let Some(env) = Env::new().await else { return };
    let alice = env.user("alice");
    let bob = env.user("bob");
    let tool = env.create_tool();

    let first = call(&tool, &alice, expense_input("備品の貸出")).await;
    assert!(!first.is_error, "{}", first.content);
    let id = ref_of(&first)["id"].clone();

    // 同じ名前・同じ列: 作り直さず、作成済みとして同じ id を返す。
    let again = call(&tool, &alice, expense_input("備品の貸出")).await;
    assert!(!again.is_error, "{}", again.content);
    assert!(
        again.content.contains("新しい版は作っていません"),
        "{}",
        again.content
    );
    assert_eq!(ref_of(&again)["id"], id);

    // 同じ名前・違う列: 保存せず、table_id と版を添えて直し方を言う。
    let mut changed = expense_input("備品の貸出");
    changed["fields"][0]["required"] = json!(false);
    let differs = call(&tool, &alice, changed).await;
    assert!(differs.is_error);
    assert!(
        differs.content.contains("data_table_update"),
        "{}",
        differs.content
    );
    assert!(
        differs.content.contains(id.as_str().unwrap()),
        "{}",
        differs.content
    );

    // ほかの人の同名（同じ組織）: 名前が使えないとだけ言う（id も持ち主も出さない）。
    let other = call(&tool, &bob, expense_input("備品の貸出")).await;
    assert!(other.is_error);
    assert!(
        other.content.contains("この名前のテーブルは作れません"),
        "{}",
        other.content
    );
    assert!(!other.content.contains(id.as_str().unwrap()));

    // 前からある自分の同名（この run より前に作った）も、作成済みとは扱わない。
    let later = DataTableCreateTool::new(
        env.deps.clone(),
        BuilderRun {
            since: Utc::now() + chrono::Duration::seconds(5),
        },
    );
    let old = call(&later, &alice, expense_input("備品の貸出")).await;
    assert!(old.is_error);
    assert!(
        old.content.contains("この名前のテーブルは作れません"),
        "{}",
        old.content
    );
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM data_table WHERE tenant_id = $1 AND deleted_at IS NULL",
    )
    .bind(&env.tenant)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(n, 1, "テーブルは 1 つだけ");
}

#[tokio::test]
async fn caps_tables_per_run_and_rejects_symmetric_links() {
    let Some(env) = Env::new().await else { return };
    let alice = env.user("alice");
    let tool = env.create_tool();
    let simple = |name: &str| json!({ "name": name, "fields": [{ "key": "title", "display_name": "件名", "type": "text" }] });
    let mut first = None;
    for i in 0..chat::MAX_TABLES_PER_RUN {
        let o = call(&tool, &alice, simple(&format!("表{i}"))).await;
        assert!(!o.is_error, "{}", o.content);
        first.get_or_insert(ref_of(&o)["id"].as_str().unwrap().to_string());
    }
    let over = call(&tool, &alice, simple("表あふれ")).await;
    assert!(over.is_error);
    assert!(
        over.content.contains("これ以上は作りません"),
        "{}",
        over.content
    );
    // 上限を超えても、作成済みの同名は作成済みとして返す（再試行で上限の誤りにしない）。
    let again = call(&tool, &alice, simple("表0")).await;
    assert!(!again.is_error, "{}", again.content);

    // 対称のリンクは作らない（参照先のテーブルのスキーマを書き換えるため・R6）。片方向は作れる。
    let fresh = DataTableCreateTool::new(
        env.deps.clone(),
        BuilderRun {
            since: Utc::now() + chrono::Duration::seconds(5),
        },
    );
    let target = first.unwrap();
    let sym = call(
        &fresh,
        &alice,
        json!({ "name": "注文", "fields": [{
            "key": "customer", "type": "record_ref", "ref_table": target,
            "link": { "cardinality": "many_to_one", "symmetric_key": "orders" }
        }] }),
    )
    .await;
    assert!(sym.is_error);
    assert!(sym.content.contains("対称のリンク"), "{}", sym.content);
    let one_way = call(
        &fresh,
        &alice,
        json!({ "name": "注文", "fields": [{
            "key": "customer", "display_name": "顧客", "type": "record_ref", "ref_table": target
        }] }),
    )
    .await;
    assert!(!one_way.is_error, "{}", one_way.content);
    let referenced = env
        .data
        .get_table(&alice, target.parse().unwrap(), None)
        .await
        .unwrap();
    assert_eq!(referenced.schema.fields.len(), 1, "参照先の列は増えない");
}

#[tokio::test]
async fn updates_add_and_change_fields_with_optimistic_locking() {
    let Some(env) = Env::new().await else { return };
    let alice = env.user("alice");
    let bob = env.user("bob");
    let created = call(&env.create_tool(), &alice, expense_input("出張申請")).await;
    let table = table_of(&env, &alice, &created).await;
    let tool = env.update_tool();
    assert!(tool.requires_confirmation(), "改訂は承認カードを経る");

    // 名前で呼んだら直さず、id と版を示す。
    let named = call(
        &tool,
        &alice,
        json!({ "table_id": "出張申請", "expected_schema_version": table.schema_version,
                "fields": [{ "key": "memo", "type": "text" }] }),
    )
    .await;
    assert!(named.is_error);
    assert!(
        named.content.contains(&table.id.to_string()),
        "{}",
        named.content
    );
    assert_eq!(
        env.data
            .get_table(&alice, table.id, None)
            .await
            .unwrap()
            .schema_version,
        table.schema_version
    );

    // 古い版は直さない。
    let stale = call(
        &tool,
        &alice,
        json!({ "table_id": table.id, "expected_schema_version": table.schema_version - 1,
                "fields": [{ "key": "memo", "type": "text" }] }),
    )
    .await;
    assert!(stale.is_error);
    assert!(
        stale
            .content
            .contains(&format!("v{}", table.schema_version)),
        "{}",
        stale.content
    );

    // 足す列と変える列だけを書く。書かなかった列・項目は今のまま。
    let ok = call(
        &tool,
        &alice,
        json!({ "table_id": table.id, "expected_schema_version": table.schema_version,
                "fields": [
                    { "key": "kind", "options": ["交通費", "備品", "宿泊費"] },
                    { "key": "memo", "display_name": "メモ", "type": "text" }
                ] }),
    )
    .await;
    assert!(!ok.is_error, "{}", ok.content);
    assert!(ok.content.contains("列を足した: memo"), "{}", ok.content);
    assert!(ok.content.contains("列を変えた: kind"), "{}", ok.content);
    let after = env.data.get_table(&alice, table.id, None).await.unwrap();
    assert_eq!(ref_of(&ok)["version"], after.schema_version);
    assert_eq!(after.schema_version, table.schema_version + 1);
    let kind = after.schema.field("kind").unwrap();
    assert_eq!(kind.options, ["交通費", "備品", "宿泊費"]);
    assert_eq!(kind.display_name, "区分");
    assert_eq!(
        after.schema.field("memo").unwrap().field_type,
        FieldType::Text
    );
    assert_eq!(after.schema.fields.len(), table.schema.fields.len() + 1);

    // 消す・型を変えるは data が断る（足すだけ）。
    let retype = call(
        &tool,
        &alice,
        json!({ "table_id": table.id, "expected_schema_version": after.schema_version,
                "fields": [{ "key": "amount", "type": "text" }] }),
    )
    .await;
    assert!(retype.is_error, "{}", retype.content);

    // 持ち主でない人（閲覧だけ共有された bob）は直せない。
    env.fga
        .write_tuple(
            &bob.subject(),
            Relation::Viewer,
            &bob.ns().data_table(&table.id.to_string()),
        )
        .await
        .unwrap();
    let denied = call(
        &tool,
        &bob,
        json!({ "table_id": table.id, "expected_schema_version": after.schema_version,
                "fields": [{ "key": "bob_memo", "type": "text" }] }),
    )
    .await;
    assert!(denied.is_error);
    assert!(denied.content.contains("持ち主"), "{}", denied.content);
    let unchanged = env.data.get_table(&alice, table.id, None).await.unwrap();
    assert_eq!(unchanged.schema_version, after.schema_version);
}

#[tokio::test]
async fn replacing_the_row_policy_keeps_the_applicant_write_lock() {
    let Some(env) = Env::new().await else { return };
    let alice = env.user("alice");
    let created = call(&env.create_tool(), &alice, expense_input("立替精算")).await;
    let table = table_of(&env, &alice, &created).await;
    let o = call(
        &env.update_tool(),
        &alice,
        json!({ "table_id": table.id, "expected_schema_version": table.schema_version,
                "row_policy": { "read": "public" } }),
    )
    .await;
    assert!(!o.is_error, "{}", o.content);
    assert!(
        o.content.contains("行の決まりを入れ替えた"),
        "{}",
        o.content
    );
    let fsm = env
        .deps
        .fsms
        .table_fsm(&alice, table.id, None)
        .await
        .unwrap();
    assert_eq!(
        fsm.applicant_write_lock.as_deref(),
        Some("applicant"),
        "申請者の書き込みの決まりは残る"
    );
    let after = env.data.get_table(&alice, table.id, None).await.unwrap();
    assert_eq!(
        after.schema.row_policy.as_ref().map(|p| p.read.clone()),
        Some(data::PolicyExpr::Public)
    );
}

#[tokio::test]
async fn fsm_changes_that_would_lock_records_are_not_saved() {
    let Some(env) = Env::new().await else { return };
    let alice = env.user("alice");
    // 状態の進め方の無いテーブルに、申請者（メンバーの列）が空で最初の状態を過ぎたレコードを置く。
    let created = call(
        &env.create_tool(),
        &alice,
        json!({ "name": "備品申請", "fields": [
            { "key": "title", "display_name": "件名", "type": "text" },
            { "key": "applicant", "display_name": "申請者", "type": "user_ref" },
            { "key": "status", "display_name": "状態", "type": "select", "options": ["申請中", "承認"] }
        ] }),
    )
    .await;
    assert!(!created.is_error, "{}", created.content);
    let table = table_of(&env, &alice, &created).await;
    env.data
        .create_record(
            &alice,
            table.id,
            json!({ "title": "古い申請", "status": "承認" }),
            None,
        )
        .await
        .unwrap();
    let fsm = json!({
        "status_field": "status",
        "states": ["申請中", "承認"],
        "transitions": [{ "from": "申請中", "to": "承認", "actor": "public" }],
        "applicant_write_lock": "applicant"
    });
    let blocked = call(
        &env.update_tool(),
        &alice,
        json!({ "table_id": table.id, "expected_schema_version": table.schema_version,
                "fields": [{ "key": "memo", "type": "text" }], "fsm": fsm }),
    )
    .await;
    assert!(blocked.is_error);
    assert!(
        blocked.content.contains("書き換えたり"),
        "{}",
        blocked.content
    );
    let after = env.data.get_table(&alice, table.id, None).await.unwrap();
    assert_eq!(
        after.schema_version, table.schema_version,
        "列も足さない（途中まで直さない）"
    );
    assert!(after.schema.fsm_ref.is_none());

    // 申請者の決まりを外せば保存できる。
    let mut ok_fsm = fsm.clone();
    ok_fsm
        .as_object_mut()
        .unwrap()
        .remove("applicant_write_lock");
    let ok = call(
        &env.update_tool(),
        &alice,
        json!({ "table_id": table.id, "expected_schema_version": table.schema_version, "fsm": ok_fsm }),
    )
    .await;
    assert!(!ok.is_error, "{}", ok.content);
    assert!(
        ok.content.contains("状態の進め方を保存した"),
        "{}",
        ok.content
    );
    let after = env.data.get_table(&alice, table.id, None).await.unwrap();
    assert!(after.schema.fsm_ref.is_some());
}

/// 申請中の経費を金額の大きい順に出すビュー（`table` は名前で指す）。
fn pending_view(name: &str, table: &str) -> Value {
    json!({
        "name": name,
        "table": table,
        "query": {
            "filter": { "cmp": { "field": "status", "op": "eq", "value": "申請中" } },
            "sort": [{ "field": "amount", "descending": true }]
        },
        "display": { "kind": "grid", "columns": [{ "field": "title" }, { "field": "amount" }] }
    })
}

#[tokio::test]
async fn saves_views_idempotently_and_updates_them_with_the_version() {
    let Some(env) = Env::new().await else { return };
    let alice = env.user("alice");
    let bob = env.user("bob");
    let created = call(&env.create_tool(), &alice, expense_input("交通費")).await;
    assert!(!created.is_error, "{}", created.content);
    let table = table_of(&env, &alice, &created).await;
    let tool = env.view_tool();
    assert!(
        !tool.requires_confirmation(),
        "ビューは確認なし（版で戻せる）"
    );

    let o = call(&tool, &alice, pending_view("申請中の一覧", "交通費")).await;
    assert!(!o.is_error, "{}", o.content);
    let r = ref_of(&o);
    assert_eq!(r["kind"], "data_view");
    assert_eq!(r["version"], 1);
    assert_eq!(o.builder_refs, std::slice::from_ref(&r));
    let view_id: Uuid = r["id"].as_str().unwrap().parse().unwrap();
    let (_, stored) = env
        .views
        .views
        .get(&alice, view_id, None, None)
        .await
        .unwrap();
    assert_eq!(stored.table_id, table.id);
    assert_eq!(stored.query.sort.len(), 1);

    // 同じ名前・同じ本文: 作成済み。違う本文: 直し方。ほかの人の同名: 名前が使えないとだけ言う。
    let again = call(
        &tool,
        &alice,
        pending_view("申請中の一覧", &table.id.to_string()),
    )
    .await;
    assert!(!again.is_error, "{}", again.content);
    assert!(
        again.content.contains("新しい版は作っていません"),
        "{}",
        again.content
    );
    let mut other_body = pending_view("申請中の一覧", "交通費");
    other_body["display"] = json!({ "kind": "kanban", "group_field": "kind" });
    let differs = call(&tool, &alice, other_body.clone()).await;
    assert!(differs.is_error);
    assert!(
        differs.content.contains(&view_id.to_string()),
        "{}",
        differs.content
    );
    env.fga
        .write_tuple(
            &bob.subject(),
            Relation::Viewer,
            &bob.ns().data_table(&table.id.to_string()),
        )
        .await
        .unwrap();
    let taken = call(
        &tool,
        &bob,
        pending_view("申請中の一覧", &table.id.to_string()),
    )
    .await;
    assert!(taken.is_error);
    assert!(taken.content.contains("作れません"), "{}", taken.content);
    assert!(!taken.content.contains(&view_id.to_string()));

    // 直す: 版が要る。古い版は直さない。名前でも id でも指せる。
    other_body.as_object_mut().unwrap().remove("name");
    other_body["view"] = json!("申請中の一覧");
    let no_version = call(&tool, &alice, other_body.clone()).await;
    assert!(no_version.is_error);
    assert!(
        no_version.content.contains("expected_version"),
        "{}",
        no_version.content
    );
    other_body["expected_version"] = json!(1);
    let updated = call(&tool, &alice, other_body.clone()).await;
    assert!(!updated.is_error, "{}", updated.content);
    assert_eq!(ref_of(&updated)["version"], 2);
    assert_eq!(ref_of(&updated)["id"], view_id.to_string());
    let stale = call(&tool, &alice, other_body.clone()).await;
    assert!(stale.is_error);
    assert!(stale.content.contains("v2"), "{}", stale.content);
    other_body["view"] = json!(view_id);
    other_body["expected_version"] = json!(2);
    let by_id = call(&tool, &alice, other_body).await;
    assert!(!by_id.is_error, "{}", by_id.content);
    assert_eq!(ref_of(&by_id)["version"], 3);

    // 閲覧だけの bob は alice のビューを直せない（ビューの編集の権限が無い）。
    let denied = call(
        &tool,
        &bob,
        json!({ "view": view_id, "expected_version": 3, "table": table.id }),
    )
    .await;
    assert!(denied.is_error);
    assert!(
        denied.content.contains("見つからないか"),
        "{}",
        denied.content
    );
}

#[tokio::test]
async fn view_inputs_are_checked_and_capped() {
    let Some(env) = Env::new().await else { return };
    let alice = env.user("alice");
    let carol = env.user("carol");
    let created = call(&env.create_tool(), &alice, expense_input("宿泊費")).await;
    let table = table_of(&env, &alice, &created).await;
    let tool = env.view_tool();

    // クエリの最上位の未知のキー・読めない表示の設定・見えないテーブルは保存しない。
    let bad_query = call(
        &tool,
        &alice,
        json!({ "name": "x", "table": table.id, "query": { "limitt": 5 } }),
    )
    .await;
    assert!(bad_query.is_error, "{}", bad_query.content);
    let bad_display = call(
        &tool,
        &alice,
        json!({ "name": "x", "table": table.id, "display": { "kind": "pie" } }),
    )
    .await;
    assert!(bad_display.is_error);
    assert!(
        bad_display.content.contains("display"),
        "{}",
        bad_display.content
    );
    let hidden = call(&tool, &carol, json!({ "name": "x", "table": table.id })).await;
    assert!(hidden.is_error);
    assert!(
        hidden.content.contains("見つからないか"),
        "{}",
        hidden.content
    );
    let unknown = call(
        &tool,
        &alice,
        json!({ "name": "x", "table": table.id, "filter": {} }),
    )
    .await;
    assert!(unknown.is_error);
    assert!(unknown.content.contains("filter"), "{}", unknown.content);

    // 1 回の run で作れるのは 10 件まで（直すのは数えない）。
    for i in 0..chat::MAX_VIEWS_PER_RUN {
        let o = call(
            &tool,
            &alice,
            json!({ "name": format!("一覧{i}"), "table": table.id }),
        )
        .await;
        assert!(!o.is_error, "{}", o.content);
    }
    let over = call(
        &tool,
        &alice,
        json!({ "name": "一覧あふれ", "table": table.id }),
    )
    .await;
    assert!(over.is_error);
    assert!(
        over.content.contains("これ以上は作りません"),
        "{}",
        over.content
    );
    let again = call(&tool, &alice, json!({ "name": "一覧0", "table": table.id })).await;
    assert!(!again.is_error, "{}", again.content);
}

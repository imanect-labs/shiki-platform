//! 宣言的クエリ・集計抑制・フィールドマスク・保存ビューの結合テスト（Task 9.4 受け入れ条件）。
//!
//! PIT-17（スモールセル抑制）・PIT-19（マスク列を検索に使わせない）を実 Postgres＋実 OpenFGA で検証する。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use async_trait::async_trait;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal, Relation};
use data::{
    Aggregate, CmpOp, DataError, DataQuery, DataStore, DataViewBody, DataViewStore, FieldDef,
    FieldPolicy, FieldType, Metric, NewDataTable, PolicyExpr, PolicyOperand, QueryFilter,
    RefResolver, RowPolicy, TableSchema,
};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
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

async fn setup() -> Option<(PgPool, Arc<dyn AuthzClient>)> {
    let db_url = std::env::var("STORAGE_TEST_DATABASE_URL").ok()?;
    let base_url = std::env::var("OPENFGA_TEST_URL").ok()?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("pg connect");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    let model: serde_json::Value =
        serde_json::from_str(include_str!("../../authz/model/authorization-model.json")).unwrap();
    let config = OpenFgaConfig {
        base_url,
        store_name: format!("shiki-data-query-test-{}", Uuid::new_v4()),
    };
    let fga = OpenFgaClient::connect(reqwest::Client::new(), &config, &model)
        .await
        .expect("fga connect");
    Some((pool, Arc::new(fga)))
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

fn field(name: &str, ty: FieldType) -> FieldDef {
    FieldDef {
        name: name.into(),
        field_type: ty,
        required: false,
        unique: false,
        indexed: false,
        options: vec![],
        ref_table: None,
        lookup: None,
        computed: None,
    }
}

/// dept/amount/salary（マスク）を持つ人事テーブル。全 viewer が全行可視（行制限なし）。
fn hr_schema() -> TableSchema {
    let mut dept = field("dept", FieldType::Select);
    dept.options = vec!["sales".into(), "eng".into(), "hr".into()];
    dept.indexed = true;
    let mut amount = field("amount", FieldType::Number);
    amount.indexed = true;
    let mut salary = field("salary", FieldType::Number);
    salary.indexed = true;
    TableSchema {
        fields: vec![dept, amount, salary, field("name", FieldType::Text)],
        status_field: None,
        row_policy: None,
        // salary は経理ロールのみ可読。
        field_policy: vec![FieldPolicy {
            field: "salary".into(),
            readable_by: PolicyExpr::HasRole {
                role: "keiri".into(),
                subtree: true,
            },
        }],
        aggregate_min_rows: Some(3),
    }
}

async fn seed(store: &DataStore, ctx: &AuthContext, table: Uuid, rows: &[(&str, f64, f64)]) {
    for (dept, amount, salary) in rows {
        store
            .create_record(
                ctx,
                table,
                json!({"dept": dept, "amount": amount, "salary": salary, "name": "x"}),
                None,
            )
            .await
            .expect("seed");
    }
}

#[tokio::test]
async fn field_mask_hides_and_blocks_search() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice"); // 経理でない
    let store = DataStore::new(pool, Arc::clone(&authz), Arc::new(AllowRefs));
    let table = store
        .create_table(
            &alice,
            NewDataTable {
                name: "hr".into(),
                schema: hr_schema(),
            },
            None,
        )
        .await
        .expect("table");
    seed(
        &store,
        &alice,
        table.id,
        &[("sales", 10.0, 500.0), ("sales", 20.0, 900.0)],
    )
    .await;

    // 投影: salary が応答に出ない（PIT-19 表示マスク）。
    let page = store
        .list_records(&alice, table.id, &data::ListRecordsOptions::default(), None)
        .await
        .expect("list");
    assert!(page.items.iter().all(|r| r.data.get("salary").is_none()));
    assert!(page.items.iter().all(|r| r.data.get("amount").is_some()));

    // 検索: salary で filter/sort/集計すると 403（PIT-19 検索に使わせない）。
    let by_filter = store
        .run_query(
            &alice,
            table.id,
            &DataQuery {
                filter: Some(QueryFilter {
                    field: "salary".into(),
                    value: json!(500),
                }),
                ..Default::default()
            },
            None,
        )
        .await;
    assert!(
        matches!(by_filter, Err(DataError::Forbidden)),
        "{by_filter:?}"
    );
    let by_sort = store
        .run_query(
            &alice,
            table.id,
            &DataQuery {
                sort: Some(data::QuerySort {
                    field: "salary".into(),
                    descending: true,
                }),
                ..Default::default()
            },
            None,
        )
        .await;
    assert!(matches!(by_sort, Err(DataError::Forbidden)), "{by_sort:?}");
    let by_agg = store
        .run_query(
            &alice,
            table.id,
            &DataQuery {
                aggregate: Some(Aggregate {
                    group_by: vec![],
                    metric: Metric::Avg,
                    field: Some("salary".into()),
                }),
                ..Default::default()
            },
            None,
        )
        .await;
    assert!(matches!(by_agg, Err(DataError::Forbidden)), "{by_agg:?}");
}

#[tokio::test]
async fn field_mask_blocks_writes_for_unauthorized() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let owner = ctx(&tenant, "alice");
    let store = DataStore::new(pool, Arc::clone(&authz), Arc::new(AllowRefs));
    let table = store
        .create_table(
            &owner,
            NewDataTable {
                name: "hr-w".into(),
                schema: hr_schema(),
            },
            None,
        )
        .await
        .expect("table");
    // 経理ロールの charlie（最初から keiri・テーブル editor）なら salary が見え、書ける。
    // ※ material キャッシュの TTL 反映遅延を避けるため、初回読取が経理権限で行われる別ユーザで検証する。
    let charlie = ctx(&tenant, "charlie");
    authz
        .write_tuple(
            &charlie.subject(),
            Relation::Member,
            &owner.ns().role("keiri"),
        )
        .await
        .expect("charlie keiri");
    authz
        .write_tuple(
            &charlie.subject(),
            Relation::Editor,
            &owner.ns().data_table(&table.id.to_string()),
        )
        .await
        .expect("charlie editor");
    let rec = store
        .create_record(
            &charlie,
            table.id,
            json!({"dept": "sales", "amount": 1, "salary": 100, "name": "x"}),
            None,
        )
        .await
        .expect("charlie creates with salary");
    // 経理は salary への更新も可能。
    store
        .update_record(
            &charlie,
            table.id,
            rec.id,
            json!({"salary": 1000}),
            rec.rev,
            None,
        )
        .await
        .expect("keiri writes salary");
    // 経理でない bob（テーブル editor）は salary へ書けない（Forbidden・盲目上書き防止）。
    let bob = ctx(&tenant, "bob");
    authz
        .write_tuple(
            &bob.subject(),
            Relation::Editor,
            &owner.ns().data_table(&table.id.to_string()),
        )
        .await
        .expect("bob editor");
    let denied = store
        .update_record(
            &bob,
            table.id,
            rec.id,
            json!({"salary": 1}),
            rec.rev + 1,
            None,
        )
        .await;
    assert!(matches!(denied, Err(DataError::Forbidden)), "{denied:?}");
}

#[tokio::test]
async fn aggregate_suppresses_small_cells() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    // 経理ロールにして salary マスクを外す（集計対象に使うため）。
    let store = DataStore::new(pool, Arc::clone(&authz), Arc::new(AllowRefs));
    let table = store
        .create_table(
            &alice,
            NewDataTable {
                name: "hr2".into(),
                schema: hr_schema(),
            },
            None,
        )
        .await
        .expect("table");
    authz
        .write_tuple(
            &alice.subject(),
            Relation::Member,
            &alice.ns().role("keiri"),
        )
        .await
        .expect("keiri");
    // sales=4 件（>=K=3）、eng=1 件（<K）。
    seed(
        &store,
        &alice,
        table.id,
        &[
            ("sales", 1.0, 100.0),
            ("sales", 2.0, 200.0),
            ("sales", 3.0, 300.0),
            ("sales", 4.0, 400.0),
            ("eng", 5.0, 999.0),
        ],
    )
    .await;

    let res = store
        .run_query(
            &alice,
            table.id,
            &DataQuery {
                aggregate: Some(Aggregate {
                    group_by: vec!["dept".into()],
                    metric: Metric::Sum,
                    field: Some("amount".into()),
                }),
                ..Default::default()
            },
            None,
        )
        .await
        .expect("aggregate");
    assert!(res.suppressed, "少人数セルの抑制が起きること（PIT-17）");
    let groups = res.groups.unwrap();
    let sales = groups
        .iter()
        .find(|g| g.key["dept"] == json!("sales"))
        .unwrap();
    assert_eq!(sales.value, json!(10.0), "sales 合計 = 1+2+3+4");
    let eng = groups
        .iter()
        .find(|g| g.key["dept"] == json!("eng"))
        .unwrap();
    assert_eq!(
        eng.value,
        serde_json::Value::Null,
        "K 未満は値を伏せる（個人特定を防ぐ）"
    );
}

#[tokio::test]
async fn saved_view_reevaluates_per_viewer() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let store = DataStore::new(pool.clone(), Arc::clone(&authz), Arc::new(AllowRefs));

    // 行ポリシー: 本人（owner）のみ可読。
    let schema = TableSchema {
        fields: vec![
            field("owner_ref", FieldType::UserRef),
            field("note", FieldType::Text),
        ],
        status_field: None,
        row_policy: Some(RowPolicy {
            read: PolicyExpr::FieldCmp {
                field: "owner_ref".into(),
                op: CmpOp::Eq,
                value: PolicyOperand::UserId,
            },
            write: None,
        }),
        field_policy: vec![],
        aggregate_min_rows: None,
    };
    let table = store
        .create_table(
            &alice,
            NewDataTable {
                name: "notes".into(),
                schema,
            },
            None,
        )
        .await
        .expect("table");
    store
        .create_record(
            &alice,
            table.id,
            json!({"owner_ref": "alice", "note": "a"}),
            None,
        )
        .await
        .expect("alice row");
    store
        .create_record(
            &alice,
            table.id,
            json!({"owner_ref": "bob", "note": "b"}),
            None,
        )
        .await
        .expect("bob row");
    // bob をテーブル viewer に。
    authz
        .write_tuple(
            &bob.subject(),
            Relation::Viewer,
            &alice.ns().data_table(&table.id.to_string()),
        )
        .await
        .expect("bob viewer");

    // alice が保存ビューを作る（全件クエリ）。
    let artifacts = Arc::new(artifact::ArtifactStore::new(pool, Arc::clone(&authz)));
    let views = DataViewStore::new(artifacts, store.clone());
    let view_id = views
        .create(
            &alice,
            "all-notes",
            &DataViewBody {
                table_id: table.id,
                query: DataQuery::default(),
                display: json!({"kind": "list"}),
            },
            None,
        )
        .await
        .expect("view");
    // ビューを共有（artifact viewer）。
    authz
        .write_tuple(
            &bob.subject(),
            Relation::Viewer,
            &alice.ns().artifact(&view_id.to_string()),
        )
        .await
        .expect("share view");

    // alice が実行すると alice の行のみ。
    let a = views
        .run(&alice, view_id, None, None)
        .await
        .expect("alice run");
    let a_items = a.items.unwrap();
    assert_eq!(a_items.len(), 1);
    assert_eq!(a_items[0].data["owner_ref"], json!("alice"));
    // bob が同じビューを実行すると bob の行のみ（作成者 alice の権限を引き継がない）。
    let b = views.run(&bob, view_id, None, None).await.expect("bob run");
    let b_items = b.items.unwrap();
    assert_eq!(b_items.len(), 1);
    assert_eq!(b_items[0].data["owner_ref"], json!("bob"));
}

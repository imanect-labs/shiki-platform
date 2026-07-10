//! 構造化データサービスの結合テスト（Task 9.2 / 9.5 受け入れ条件）。
//!
//! - 実 Postgres（`STORAGE_TEST_DATABASE_URL`）: スキーマ定義→型検証付き CRUD・
//!   式インデックス（EXPLAIN で使用検証）・参照整合・リビジョン差分・楽観ロック 409。
//!   authz はモック（AllowAll）。
//! - 実 OpenFGA（`OPENFGA_TEST_URL` 併設時のみ）: テーブル ReBAC（第1層）の
//!   共有/拒否と履歴の認可追従。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use data::{
    DataError, DataStore, FieldDef, FieldType, ListRecordsOptions, NewDataTable, RecordFilter,
    RecordSort, RefResolver, TableSchema,
};
use serde_json::{json, Value};
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

/// 固定の存在集合を返す参照リゾルバ（alice/bob と role sales のみ存在）。
struct FixedResolver;

#[async_trait]
impl RefResolver for FixedResolver {
    async fn user_exists(&self, _: &AuthContext, id: &str) -> Result<bool, String> {
        Ok(matches!(id, "alice" | "bob"))
    }
    async fn role_exists(&self, _: &AuthContext, id: &str) -> Result<bool, String> {
        Ok(id == "sales")
    }
    async fn file_readable(&self, _: &AuthContext, _: Uuid) -> Result<bool, String> {
        Ok(false) // file は常に不可視（存在オラクルなしの検証に使う）
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

fn store_with(pool: PgPool) -> DataStore {
    DataStore::new(pool, Arc::new(AllowAll), Arc::new(FixedResolver))
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

/// 経費テーブル相当のスキーマ（title 必須・amount 索引・status 選択・申請者 user 参照）。
fn expense_schema() -> TableSchema {
    let mut title = field("title", FieldType::Text);
    title.required = true;
    let mut amount = field("amount", FieldType::Number);
    amount.indexed = true;
    let mut status = field("status", FieldType::Select);
    status.options = vec!["draft".into(), "submitted".into(), "approved".into()];
    status.indexed = true;
    let applicant = field("applicant", FieldType::UserRef);
    let mut code = field("code", FieldType::Text);
    code.unique = true;
    TableSchema {
        fields: vec![title, amount, status, applicant, code],
        status_field: Some("status".into()),
        row_policy: None,
    }
}

#[tokio::test]
async fn table_crud_and_typed_record_crud() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool);
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");

    let table = store
        .create_table(
            &c,
            NewDataTable {
                name: "expense".into(),
                schema: expense_schema(),
            },
            None,
        )
        .await
        .expect("create table");
    assert_eq!(table.schema_version, 1);

    // 同名は 409。
    let dup = store
        .create_table(
            &c,
            NewDataTable {
                name: "expense".into(),
                schema: expense_schema(),
            },
            None,
        )
        .await;
    assert!(matches!(dup, Err(DataError::Conflict(_))), "{dup:?}");

    // 型検証付き CRUD（受け入れ条件 9.2-1）。
    let rec = store
        .create_record(
            &c,
            table.id,
            json!({"title": "出張費", "amount": 1200.5, "status": "draft", "applicant": "alice", "code": "E-1"}),
            None,
        )
        .await
        .expect("create record");
    assert_eq!(rec.rev, 1);
    assert_eq!(rec.owner, "alice");

    // 必須欠落・未宣言フィールド・選択肢外・存在しない user は拒否。
    for bad in [
        json!({"amount": 1}),
        json!({"title": "x", "nope": 1}),
        json!({"title": "x", "status": "unknown"}),
        json!({"title": "x", "applicant": "ghost"}),
    ] {
        let r = store.create_record(&c, table.id, bad.clone(), None).await;
        assert!(matches!(r, Err(DataError::Invalid(_))), "{bad} -> {r:?}");
    }

    // unique 制約（code）: 同値は 409。
    let dup_code = store
        .create_record(&c, table.id, json!({"title": "重複", "code": "E-1"}), None)
        .await;
    assert!(
        matches!(dup_code, Err(DataError::Conflict(_))),
        "{dup_code:?}"
    );

    // 更新（merge patch）＋取得。
    let updated = store
        .update_record(&c, table.id, rec.id, json!({"amount": 2000}), 1, None)
        .await
        .expect("update");
    assert_eq!(updated.rev, 2);
    let got = store
        .get_record(&c, table.id, rec.id, None)
        .await
        .expect("get");
    assert_eq!(got.data["amount"], json!(2000));
    assert_eq!(got.data["title"], json!("出張費"), "未指定フィールドは維持");

    // 削除（楽観ロック付き）→ NotFound。
    store
        .delete_record(&c, table.id, rec.id, 2, None)
        .await
        .expect("delete");
    assert!(matches!(
        store.get_record(&c, table.id, rec.id, None).await,
        Err(DataError::NotFound)
    ));
}

#[tokio::test]
async fn expression_indexes_created_and_used() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let table = store
        .create_table(
            &c,
            NewDataTable {
                name: "idx-check".into(),
                schema: expense_schema(),
            },
            None,
        )
        .await
        .expect("create table");

    // 台帳に載る（amount=btree_numeric, status=btree_text, code=unique_text）。
    let kinds: Vec<(String, String)> = sqlx::query_as(
        "SELECT field, kind FROM data_index_registry WHERE tenant_id = $1 AND table_id = $2 ORDER BY field",
    )
    .bind(&tenant)
    .bind(table.id)
    .fetch_all(&pool)
    .await
    .expect("registry rows");
    assert_eq!(
        kinds,
        vec![
            ("amount".to_string(), "btree_numeric".to_string()),
            ("code".to_string(), "unique_text".to_string()),
            ("status".to_string(), "btree_text".to_string()),
        ]
    );

    for i in 0..50 {
        store
            .create_record(
                &c,
                table.id,
                json!({"title": format!("r{i}"), "amount": i, "status": if i % 2 == 0 { "draft" } else { "submitted" }, "code": format!("C-{i}")}),
                None,
            )
            .await
            .expect("seed record");
    }

    // フィルタ/ソートが効く（受け入れ条件 9.2-2・挙動面）。
    let page = store
        .list_records(
            &c,
            table.id,
            &ListRecordsOptions {
                filter: Some(RecordFilter {
                    field: "status".into(),
                    value: json!("draft"),
                }),
                sort: Some(RecordSort {
                    field: "amount".into(),
                    descending: true,
                }),
                limit: 10,
                offset: 0,
            },
            None,
        )
        .await
        .expect("filtered list");
    assert_eq!(page.items.len(), 10);
    assert_eq!(page.items[0].data["amount"], json!(48));
    assert!(page
        .items
        .iter()
        .all(|r| r.data["status"] == json!("draft")));

    // 未索引フィールドのフィルタ/ソートは拒否（全走査クエリを作らせない）。
    let bad = store
        .list_records(
            &c,
            table.id,
            &ListRecordsOptions {
                filter: Some(RecordFilter {
                    field: "title".into(),
                    value: json!("r1"),
                }),
                ..Default::default()
            },
            None,
        )
        .await;
    assert!(matches!(bad, Err(DataError::Invalid(_))), "{bad:?}");

    // EXPLAIN で式インデックスが使われる（受け入れ条件 9.2-2・プラン面）。
    // 小テーブルでは seqscan が選ばれ得るため無効化してプランを確認する。
    let mut conn = pool.acquire().await.expect("conn");
    sqlx::query("SET enable_seqscan = off")
        .execute(&mut *conn)
        .await
        .expect("disable seqscan");
    let plan: Value = sqlx::query_scalar(&format!(
        "EXPLAIN (FORMAT JSON) SELECT id FROM data_record \
         WHERE tenant_id = '{tenant}' AND table_id = '{}' AND (data ->> 'status') = 'draft'",
        table.id
    ))
    .fetch_one(&mut *conn)
    .await
    .expect("explain");
    let plan_text = plan.to_string();
    assert!(
        plan_text.contains("Index Scan") || plan_text.contains("Bitmap Index Scan"),
        "式インデックスが使われること: {plan_text}"
    );
}

#[tokio::test]
async fn record_refs_validated_and_file_ref_denied() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool);
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");

    // 参照先テーブル（customer）と参照元テーブル（order）。
    let customer = store
        .create_table(
            &c,
            NewDataTable {
                name: "customer".into(),
                schema: TableSchema {
                    fields: vec![field("name", FieldType::Text)],
                    status_field: None,
                    row_policy: None,
                },
            },
            None,
        )
        .await
        .expect("customer table");
    let mut ref_f = field("customer", FieldType::RecordRef);
    ref_f.ref_table = Some(customer.id);
    let order = store
        .create_table(
            &c,
            NewDataTable {
                name: "order".into(),
                schema: TableSchema {
                    fields: vec![
                        field("memo", FieldType::Text),
                        ref_f,
                        field("attachment", FieldType::FileRef),
                    ],
                    status_field: None,
                    row_policy: None,
                },
            },
            None,
        )
        .await
        .expect("order table");

    let cust = store
        .create_record(&c, customer.id, json!({"name": "ACME 商事"}), None)
        .await
        .expect("customer record");

    // 実在する参照は OK・実在しない参照は拒否（受け入れ条件 9.2-3）。
    store
        .create_record(&c, order.id, json!({"customer": cust.id.to_string()}), None)
        .await
        .expect("valid ref");
    let missing = store
        .create_record(
            &c,
            order.id,
            json!({"customer": Uuid::new_v4().to_string()}),
            None,
        )
        .await;
    assert!(matches!(missing, Err(DataError::Invalid(_))), "{missing:?}");

    // 読めないファイル参照は「見つからない」で拒否（存在オラクルなし）。
    let file_denied = store
        .create_record(
            &c,
            order.id,
            json!({"attachment": Uuid::new_v4().to_string()}),
            None,
        )
        .await;
    assert!(
        matches!(file_denied, Err(DataError::Invalid(_))),
        "{file_denied:?}"
    );
}

#[tokio::test]
async fn revisions_track_field_diffs_and_optimistic_lock() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool);
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let table = store
        .create_table(
            &c,
            NewDataTable {
                name: "rev-check".into(),
                schema: expense_schema(),
            },
            None,
        )
        .await
        .expect("table");

    let rec = store
        .create_record(&c, table.id, json!({"title": "初版", "amount": 100}), None)
        .await
        .expect("create");

    // 競合する同時更新: 古い rev での更新は 409（受け入れ条件 9.5-2）。
    store
        .update_record(&c, table.id, rec.id, json!({"amount": 200}), 1, None)
        .await
        .expect("first update");
    let stale = store
        .update_record(&c, table.id, rec.id, json!({"amount": 300}), 1, None)
        .await;
    assert!(matches!(stale, Err(DataError::Conflict(_))), "{stale:?}");

    // フィールド除去（null）と追加。
    store
        .update_record(
            &c,
            table.id,
            rec.id,
            json!({"amount": null, "status": "draft"}),
            2,
            None,
        )
        .await
        .expect("second update");

    // リビジョンがフィールド単位差分で辿れる（受け入れ条件 9.5-1）。
    let revs = store
        .list_revisions(&c, table.id, rec.id, None, 50, None)
        .await
        .expect("revisions");
    assert_eq!(
        revs.iter().map(|r| r.rev).collect::<Vec<_>>(),
        vec![3, 2, 1],
        "rev 降順"
    );
    assert_eq!(revs[2].change_kind, "create");
    assert!(revs[2]
        .patch
        .iter()
        .any(|p| p.field == "title" && p.old == Value::Null && p.new == json!("初版")));
    let second = &revs[1];
    assert_eq!(second.change_kind, "update");
    assert_eq!(second.patch.len(), 1, "変更フィールドのみ");
    assert_eq!(second.patch[0].field, "amount");
    assert_eq!(second.patch[0].old, json!(100));
    assert_eq!(second.patch[0].new, json!(200));
    let third = &revs[0];
    assert!(
        third
            .patch
            .iter()
            .any(|p| p.field == "amount" && p.new == Value::Null),
        "除去が残る"
    );
    assert!(third
        .patch
        .iter()
        .any(|p| p.field == "status" && p.new == json!("draft")));

    // 変更なし patch はリビジョンを増やさない。
    let same = store
        .update_record(&c, table.id, rec.id, json!({"title": "初版"}), 3, None)
        .await
        .expect("noop update");
    assert_eq!(same.rev, 3);

    // 削除も changelog に残る。
    store
        .delete_record(&c, table.id, rec.id, 3, None)
        .await
        .expect("delete");
    let revs = store
        .list_revisions(&c, table.id, rec.id, None, 50, None)
        .await
        .expect("revisions after delete");
    assert_eq!(revs[0].change_kind, "delete");
    assert_eq!(revs[0].rev, 4);
}

#[tokio::test]
async fn schema_update_additive_and_index_reapply() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let table = store
        .create_table(
            &c,
            NewDataTable {
                name: "evolve".into(),
                schema: TableSchema {
                    fields: vec![field("title", FieldType::Text)],
                    status_field: None,
                    row_policy: None,
                },
            },
            None,
        )
        .await
        .expect("table");

    // additive 追加＋indexed 付与は成功し schema_version が上がる。
    let mut title = field("title", FieldType::Text);
    title.indexed = true;
    let mut price = field("price", FieldType::Number);
    price.indexed = true;
    let updated = store
        .update_table_schema(
            &c,
            table.id,
            TableSchema {
                fields: vec![title, price],
                status_field: None,
                row_policy: None,
            },
            Some(1),
            None,
        )
        .await
        .expect("schema update");
    assert_eq!(updated.schema_version, 2);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM data_index_registry WHERE tenant_id = $1 AND table_id = $2",
    )
    .bind(&tenant)
    .bind(table.id)
    .fetch_one(&pool)
    .await
    .expect("registry count");
    assert_eq!(count, 2, "改訂で式インデックスが差分適用される");

    // 型変更・削除は拒否。schema_version 不一致は 409。
    let breaking = store
        .update_table_schema(
            &c,
            table.id,
            TableSchema {
                fields: vec![field("title", FieldType::Number)],
                status_field: None,
                row_policy: None,
            },
            None,
            None,
        )
        .await;
    assert!(
        matches!(breaking, Err(DataError::Invalid(_))),
        "{breaking:?}"
    );
    let stale = store
        .update_table_schema(
            &c,
            table.id,
            TableSchema {
                fields: vec![field("title", FieldType::Text)],
                status_field: None,
                row_policy: None,
            },
            Some(1),
            None,
        )
        .await;
    assert!(matches!(stale, Err(DataError::Conflict(_))), "{stale:?}");
}

#[tokio::test]
async fn deleted_table_denies_record_access() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool);
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let table = store
        .create_table(
            &c,
            NewDataTable {
                name: "gone".into(),
                schema: expense_schema(),
            },
            None,
        )
        .await
        .expect("table");
    let rec = store
        .create_record(&c, table.id, json!({"title": "残存"}), None)
        .await
        .expect("record");
    store
        .delete_table(&c, table.id, None)
        .await
        .expect("delete table");

    // 削除後は FGA タプルが残っていても record 経路すべてが 404（fail-closed・Codex P1）。
    assert!(matches!(
        store.get_record(&c, table.id, rec.id, None).await,
        Err(DataError::NotFound)
    ));
    assert!(matches!(
        store.delete_record(&c, table.id, rec.id, 1, None).await,
        Err(DataError::NotFound)
    ));
    assert!(matches!(
        store
            .list_revisions(&c, table.id, rec.id, None, 10, None)
            .await,
        Err(DataError::NotFound)
    ));
}

#[tokio::test]
async fn date_normalized_and_ref_table_immutable() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool);
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let mut due = field("due", FieldType::Date);
    due.indexed = true;
    let table = store
        .create_table(
            &c,
            NewDataTable {
                name: "dates".into(),
                schema: TableSchema {
                    fields: vec![due],
                    status_field: None,
                    row_policy: None,
                },
            },
            None,
        )
        .await
        .expect("table");
    // ゼロ詰めなしの日付は正準形（ゼロ詰め）へ正規化して保存する（CodeRabbit 指摘）。
    let rec = store
        .create_record(&c, table.id, json!({"due": "2026-7-5"}), None)
        .await
        .expect("record");
    assert_eq!(rec.data["due"], json!("2026-07-05"));

    // record_ref の参照先変更は additive 契約違反として拒否（Codex/CodeRabbit 指摘）。
    let customer = store
        .create_table(
            &c,
            NewDataTable {
                name: "cust-a".into(),
                schema: TableSchema {
                    fields: vec![field("name", FieldType::Text)],
                    status_field: None,
                    row_policy: None,
                },
            },
            None,
        )
        .await
        .expect("cust-a");
    let other = store
        .create_table(
            &c,
            NewDataTable {
                name: "cust-b".into(),
                schema: TableSchema {
                    fields: vec![field("name", FieldType::Text)],
                    status_field: None,
                    row_policy: None,
                },
            },
            None,
        )
        .await
        .expect("cust-b");
    let mut ref_f = field("customer", FieldType::RecordRef);
    ref_f.ref_table = Some(customer.id);
    let orders = store
        .create_table(
            &c,
            NewDataTable {
                name: "orders-imm".into(),
                schema: TableSchema {
                    fields: vec![ref_f.clone()],
                    status_field: None,
                    row_policy: None,
                },
            },
            None,
        )
        .await
        .expect("orders");
    let mut moved = ref_f;
    moved.ref_table = Some(other.id);
    let res = store
        .update_table_schema(
            &c,
            orders.id,
            TableSchema {
                fields: vec![moved],
                status_field: None,
                row_policy: None,
            },
            None,
            None,
        )
        .await;
    assert!(matches!(res, Err(DataError::Invalid(_))), "{res:?}");
}

/// 実 OpenFGA でのテーブル ReBAC（第1層）検証: 未共有ユーザーは CRUD も履歴も拒否。
#[tokio::test]
async fn table_rebac_with_live_openfga() {
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
        store_name: format!("shiki-data-test-{}", Uuid::new_v4()),
    };
    let fga = OpenFgaClient::connect(reqwest::Client::new(), &config, &model)
        .await
        .expect("OpenFGA 接続");
    let fga: Arc<dyn AuthzClient> = Arc::new(fga);
    let store = DataStore::new(pool, Arc::clone(&fga), Arc::new(FixedResolver));

    let tenant = unique_tenant();
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");

    let table = store
        .create_table(
            &alice,
            NewDataTable {
                name: "acl".into(),
                schema: expense_schema(),
            },
            None,
        )
        .await
        .expect("create table");
    let rec = store
        .create_record(&alice, table.id, json!({"title": "秘密"}), None)
        .await
        .expect("record");

    // 未共有の bob は get/list/create/revisions すべて拒否（fail-closed）。
    assert!(matches!(
        store.get_table(&bob, table.id, None).await,
        Err(DataError::Forbidden)
    ));
    assert!(matches!(
        store.get_record(&bob, table.id, rec.id, None).await,
        Err(DataError::Forbidden)
    ));
    assert!(matches!(
        store
            .create_record(&bob, table.id, json!({"title": "x"}), None)
            .await,
        Err(DataError::Forbidden)
    ));
    assert!(matches!(
        store
            .list_revisions(&bob, table.id, rec.id, None, 10, None)
            .await,
        Err(DataError::Forbidden)
    ));

    // viewer 共有（tuple 直書き＝共有 API は Task 9.3 の個別共有と合流予定）で読める。
    // editor でないので書込は拒否のまま。
    fga.write_tuple(
        &bob.subject(),
        Relation::Viewer,
        &alice.ns().data_table(&table.id.to_string()),
    )
    .await
    .expect("share viewer");
    store
        .get_record(&bob, table.id, rec.id, None)
        .await
        .expect("bob reads after share");
    store
        .list_revisions(&bob, table.id, rec.id, None, 10, None)
        .await
        .expect("bob reads revisions");
    assert!(matches!(
        store
            .create_record(&bob, table.id, json!({"title": "x"}), None)
            .await,
        Err(DataError::Forbidden)
    ));

    // 一覧は FGA 実効集合ベース（bob にも table が見える）。
    let tables = store.list_tables(&bob, 10).await.expect("list");
    assert!(tables.iter().any(|t| t.id == table.id));
}

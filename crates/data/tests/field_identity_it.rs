//! フィールド識別子 3 層＋内部整数キーの結合テスト（Phase 15 Task 15.1 受け入れ条件）。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）。authz は全許可モック（行述語の材料は空集合）。
//!
//! - 日本語の表示名で作成・表示・変更でき、変更時に `data_record` が 1 行も更新されない
//! - `key` の変更が拒否される（不変性）
//! - v1 形式のテーブルの移行がテーブル単位で冪等・中断後に再開でき、移行後も v1 の操作
//!   （CRUD・索引付きフィルタ/ソート・集計・row_policy・field_policy・lookup・computed・FSM 遷移・
//!   リビジョン）が通る
//! - 既存レコードに `record_int` が採番され、応答・監査・outbox・エラー文言に現れない（negative）
//! - `tenant_int` / `table_int` が配置レジストリ・採番サービスで振られ、行と整合する

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::too_many_lines,
    clippy::many_single_char_names
)]

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use data::{
    Aggregate, CmpOp, DataError, DataQuery, DataStore, FieldDef, FieldType, FsmBody, FsmTransition,
    ListRecordsOptions, Metric, NewDataTable, PolicyExpr, PolicyOperand, QueryFilter, QuerySort,
    RecordFilter, RecordSort, RefResolver, TableSchema, FIELD_KEY_MIGRATION_QUEUE,
};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

/// 全許可モック。`tables` は list_objects(data_table) が返す可視テーブル（一覧の検査用）。
#[derive(Default)]
struct AllowAll {
    tables: Vec<String>,
}

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
        t: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(if t == ObjectType::DataTable {
            self.tables.clone()
        } else {
            vec![]
        })
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
        Ok(false)
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
    DataStore::new(pool, Arc::new(AllowAll::default()), Arc::new(FixedResolver))
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

fn schema(fields: Vec<FieldDef>) -> TableSchema {
    TableSchema {
        fields,
        status_field: None,
        row_policy: None,
        field_policy: vec![],
        aggregate_min_rows: None,
        fsm_ref: None,
    }
}

/// `data_record` 行の (id → (xmin, rev, updated_at)) スナップショット（更新されていないことの検査）。
async fn record_versions(pool: &PgPool, tenant: &str, table: Uuid) -> HashMap<Uuid, String> {
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, xmin::text || '/' || rev::text || '/' || updated_at::text \
         FROM data_record WHERE tenant_id = $1 AND table_id = $2",
    )
    .bind(tenant)
    .bind(table)
    .fetch_all(pool)
    .await
    .expect("record versions");
    rows.into_iter().collect()
}

// ---------------------------------------------------------------------------
// 受け入れ条件 1・2: 日本語の表示名・key の不変性
// ---------------------------------------------------------------------------

#[tokio::test]
async fn japanese_display_name_rename_touches_no_record() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");

    let mut applicant = FieldDef::new("applicant", FieldType::UserRef);
    applicant.display_name = "申請者".into();
    applicant.description = Some("経費を申請した人".into());
    let mut status = FieldDef::new("status", FieldType::Select);
    status.display_name = "承認ステータス".into();
    status.options = vec!["承認待ち".into(), "承認済み".into()];
    status.indexed = true;
    let mut title = FieldDef::new("title", FieldType::Text);
    title.searchable = true;
    let table = store
        .create_table(
            &c,
            NewDataTable {
                name: "経費精算".into(),
                schema: schema(vec![applicant, status, title]),
            },
            None,
        )
        .await
        .expect("日本語の表示名で作成できる");
    // 採番: 宣言順に 16 から・表示名はそのまま・key は参照名。
    let ids: Vec<(u16, &str, &str)> = table
        .schema
        .fields
        .iter()
        .map(|f| (f.id, f.key.as_str(), f.display_name.as_str()))
        .collect();
    assert_eq!(
        ids,
        vec![
            (16, "applicant", "申請者"),
            (17, "status", "承認ステータス"),
            (18, "title", "title"),
        ]
    );
    let key_format: i16 =
        sqlx::query_scalar("SELECT key_format FROM data_table WHERE tenant_id = $1 AND id = $2")
            .bind(&tenant)
            .bind(table.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(key_format, 2, "新規テーブルは最初から 3 層形式");

    for i in 0..5 {
        store
            .create_record(
                &c,
                table.id,
                json!({"applicant": "alice", "status": "承認待ち", "title": format!("r{i}")}),
                None,
            )
            .await
            .expect("create");
    }
    // 保存形は "f{id}"・外部形は key（表示名でも f{id} でもない）。
    let stored: Value = sqlx::query_scalar(
        "SELECT data FROM data_record WHERE tenant_id = $1 AND table_id = $2 LIMIT 1",
    )
    .bind(&tenant)
    .bind(table.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let stored_keys: Vec<&String> = stored.as_object().unwrap().keys().collect();
    assert!(
        stored_keys
            .iter()
            .all(|k| ["f16", "f17", "f18"].contains(&k.as_str())),
        "{stored}"
    );
    let page = store
        .list_records(
            &c,
            table.id,
            &ListRecordsOptions {
                filter: Some(RecordFilter {
                    field: "status".into(),
                    value: json!("承認待ち"),
                }),
                limit: 50,
                ..Default::default()
            },
            None,
        )
        .await
        .expect("key でフィルタできる");
    assert_eq!(page.items.len(), 5);
    assert_eq!(page.items[0].data["applicant"], json!("alice"));

    // 表示名を変更する → スキーマのみ更新・data_record は 1 行も動かない。
    let before = record_versions(&pool, &tenant, table.id).await;
    let mut next = table.schema.clone();
    next.fields[0].display_name = "申請者氏名".into();
    next.fields[1].display_name = "ステータス（承認）".into();
    next.fields[2].display_name = "件名".into();
    next.fields.reverse(); // 宣言の並びを変えても id で照合される
    let updated = store
        .update_table_schema(&c, table.id, next, Some(table.schema_version), None)
        .await
        .expect("表示名の変更");
    assert_eq!(updated.schema_version, table.schema_version + 1);
    assert_eq!(
        updated.schema.field("applicant").unwrap().display_name,
        "申請者氏名"
    );
    assert_eq!(
        updated.schema.field("applicant").unwrap().id,
        16,
        "id は不変"
    );
    let after = record_versions(&pool, &tenant, table.id).await;
    assert_eq!(before, after, "表示名の変更で data_record が更新された");
    // 表示（取得）にも反映され、データは従来どおり key で読める。
    let got = store.get_table(&c, table.id, None).await.unwrap();
    assert_eq!(got.schema.field("title").unwrap().display_name, "件名");
    let rec = store
        .get_record(&c, table.id, page.items[0].id, None)
        .await
        .unwrap();
    assert_eq!(rec.data["status"], json!("承認待ち"));
}

#[tokio::test]
async fn key_change_is_rejected() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let table = store
        .create_table(
            &c,
            NewDataTable {
                name: "keys".into(),
                schema: schema(vec![
                    FieldDef::new("applicant", FieldType::UserRef),
                    FieldDef::new("memo", FieldType::Text),
                ]),
            },
            None,
        )
        .await
        .unwrap();

    // ① 同じ id のまま key を変える → 422。
    let mut next = table.schema.clone();
    next.fields[0].key = "requester".into();
    let r = store
        .update_table_schema(&c, table.id, next, None, None)
        .await;
    assert!(
        matches!(&r, Err(DataError::Invalid(m)) if m.contains("key は変更できません")),
        "{r:?}"
    );
    // ② v1 流のリネーム（id 省略・name だけ変える）→ 旧フィールドの削除として 422。
    let v1_style: TableSchema = serde_json::from_value(json!({
        "fields": [
            { "name": "requester", "type": "user_ref" },
            { "name": "memo", "type": "text" }
        ]
    }))
    .unwrap();
    let r = store
        .update_table_schema(&c, table.id, v1_style, None, None)
        .await;
    assert!(matches!(r, Err(DataError::Invalid(_))), "{r:?}");
    // ③ 存在しない id の指定（ID の偽造・再利用）→ 422。
    let mut forged = table.schema.clone();
    let mut extra = FieldDef::new("extra", FieldType::Text);
    extra.id = 999;
    forged.fields.push(extra);
    let r = store
        .update_table_schema(&c, table.id, forged, None, None)
        .await;
    assert!(matches!(r, Err(DataError::Invalid(_))), "{r:?}");
    // スキーマは変わっていない。
    let got = store.get_table(&c, table.id, None).await.unwrap();
    assert_eq!(got.schema, table.schema);
    assert_eq!(got.schema_version, table.schema_version);

    // v1 互換: name だけの入力（id 省略）でも既存 key と照合して id を引き継ぎ、追加は新規採番。
    let additive: TableSchema = serde_json::from_value(json!({
        "fields": [
            { "name": "applicant", "type": "user_ref" },
            { "name": "memo", "type": "text" },
            { "name": "amount", "type": "number" }
        ]
    }))
    .unwrap();
    let up = store
        .update_table_schema(&c, table.id, additive, None, None)
        .await
        .expect("additive は通る");
    assert_eq!(up.schema.field("applicant").unwrap().id, 16);
    assert_eq!(up.schema.field("amount").unwrap().id, 18);
}

// ---------------------------------------------------------------------------
// 受け入れ条件 3・4: v1 形式からの移行（冪等・中断再開）と移行後の v1 操作
// ---------------------------------------------------------------------------

/// v1 形式（JSONB キー = フィールド名）のテーブルを直接 SQL で作る（移行前の既存データの再現）。
async fn seed_legacy_table(
    pool: &PgPool,
    tenant: &str,
    name: &str,
    schema: &Value,
    records: &[(&str, Value)],
    legacy_index_on: Option<&str>,
) -> Uuid {
    let mut conn = pool.acquire().await.unwrap();
    let placement = storage::tenant::ensure_placement(&mut conn, tenant)
        .await
        .unwrap();
    let table_int = storage::tenant::allocate_table_int(&mut conn)
        .await
        .unwrap();
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO data_table (tenant_id, org, name, schema, created_by, tenant_int, table_int, \
           key_format, next_field_id) \
         VALUES ($1, 'acme', $2, $3, 'alice', $4, $5, 1, 16) RETURNING id",
    )
    .bind(tenant)
    .bind(name)
    .bind(schema)
    .bind(placement.tenant_int)
    .bind(table_int)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    for (owner, data) in records {
        // v1 のレコード（record_int は列 default で採番される＝既存行と同じ扱い）。
        let rid: Uuid = sqlx::query_scalar(
            "INSERT INTO data_record (tenant_id, table_id, org, data, owner, tenant_int, table_int) \
             VALUES ($1, $2, 'acme', $3, $4, $5, $6) RETURNING id",
        )
        .bind(tenant)
        .bind(id)
        .bind(data)
        .bind(owner)
        .bind(placement.tenant_int)
        .bind(table_int)
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        // v1 の作成リビジョン（差分はフィールド名＝key で記録されている）。
        let patch: Vec<Value> = data
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| json!({"field": k, "old": null, "new": v}))
            .collect();
        sqlx::query(
            "INSERT INTO data_record_revision \
               (tenant_id, record_id, table_id, rev, changed_by, change_kind, patch) \
             VALUES ($1, $2, $3, 1, $4, 'create', $5)",
        )
        .bind(tenant)
        .bind(rid)
        .bind(id)
        .bind(owner)
        .bind(Value::Array(patch))
        .execute(&mut *conn)
        .await
        .unwrap();
    }
    if let Some(field) = legacy_index_on {
        // v1 の式インデックス（フィールド名を埋め込んだ部分インデックス）＋台帳。
        let idx = format!("dr_{}_legacy01", id.simple());
        sqlx::query(&format!(
            "CREATE INDEX {idx} ON data_record ((data ->> '{field}')) \
             WHERE tenant_id = '{tenant}' AND table_id = '{id}'"
        ))
        .execute(&mut *conn)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO data_index_registry (tenant_id, table_id, field, index_name, kind) \
             VALUES ($1, $2, $3, $4, 'btree_text')",
        )
        .bind(tenant)
        .bind(id)
        .bind(field)
        .bind(&idx)
        .execute(&mut *conn)
        .await
        .unwrap();
    }
    id
}

/// 経費（v1）: row_policy（申請者本人）・field_policy（amount は経理のみ）・status 索引・
/// computed・status_field。
fn legacy_expense_schema() -> Value {
    json!({
        "fields": [
            { "name": "title", "type": "text", "required": true },
            { "name": "amount", "type": "number", "indexed": true },
            { "name": "status", "type": "select", "indexed": true,
              "options": ["draft", "submitted", "approved"] },
            { "name": "applicant", "type": "user_ref", "indexed": true },
            { "name": "label", "type": "computed",
              "computed": { "op": "concat", "fields": ["title"] } }
        ],
        "status_field": "status",
        "row_policy": { "read": { "field_cmp": {
            "field": "applicant", "op": "eq", "value": "user_id" } } },
        "field_policy": [ { "field": "amount",
            "readable_by": { "has_role": { "role": "keiri", "subtree": true } } } ]
    })
}

#[tokio::test]
async fn legacy_tables_migrate_idempotently_and_v1_operations_still_work() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");

    let expense = seed_legacy_table(
        &pool,
        &tenant,
        "expense",
        &legacy_expense_schema(),
        &[
            ("alice", json!({"title": "出張", "amount": 1200, "status": "draft", "applicant": "alice"})),
            ("alice", json!({"title": "書籍", "amount": 3000, "status": "submitted", "applicant": "alice"})),
            ("bob", json!({"title": "会食", "amount": 9000, "status": "draft", "applicant": "bob"})),
        ],
        Some("status"),
    )
    .await;
    // 参照先（lookup 用）と参照元。
    let customer = seed_legacy_table(
        &pool,
        &tenant,
        "customer",
        &json!({"fields": [{ "name": "cname", "type": "text" }]}),
        &[("alice", json!({"cname": "山田商事"}))],
        None,
    )
    .await;
    let cust_id: Uuid =
        sqlx::query_scalar("SELECT id FROM data_record WHERE tenant_id = $1 AND table_id = $2")
            .bind(&tenant)
            .bind(customer)
            .fetch_one(&pool)
            .await
            .unwrap();
    let order = seed_legacy_table(
        &pool,
        &tenant,
        "order",
        &json!({"fields": [
            { "name": "cust", "type": "record_ref", "ref_table": customer },
            { "name": "cust_name", "type": "lookup",
              "lookup": { "via_field": "cust", "target_field": "cname" } }
        ]}),
        &[("alice", json!({"cust": cust_id.to_string()}))],
        None,
    )
    .await;
    // 移行できない（壊れた）v1 テーブル: 中断のシミュレーション用（key 規約違反）。
    let broken = seed_legacy_table(
        &pool,
        &tenant,
        "broken",
        &json!({"fields": [{ "name": "Bad Name", "type": "text" }]}),
        &[("alice", json!({"Bad Name": "x"}))],
        None,
    )
    .await;

    let record_ints_before: Vec<(Uuid, i64, i64, String)> = sqlx::query_as(
        "SELECT id, record_int, rev, updated_at::text FROM data_record \
         WHERE tenant_id = $1 AND table_id = $2 ORDER BY id",
    )
    .bind(&tenant)
    .bind(expense)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        record_ints_before.len(),
        3,
        "既存レコードに record_int が採番済み"
    );

    // 移行前の一覧表示: 決定的に引き上げた id が見える（移行後と同じ）。
    let lister = DataStore::new(
        pool.clone(),
        Arc::new(AllowAll {
            tables: vec![alice.ns().data_table(&expense.to_string()).to_string()],
        }),
        Arc::new(FixedResolver),
    );
    let visible = lister.list_tables(&alice, 200).await.unwrap();
    let pre = visible.iter().find(|t| t.id == expense).unwrap();
    assert_eq!(pre.schema.field("title").unwrap().id, 16);
    assert_eq!(pre.schema.field("applicant").unwrap().id, 19);

    // --- 中断: 壊れたテーブルの移行は失敗し、ロールバックされて v1 のまま残る ---
    let r = store.migrate_table_field_keys(&tenant, broken).await;
    assert!(matches!(r, Err(DataError::Invalid(_))), "{r:?}");
    let (fmt, data): (i16, Value) = sqlx::query_as(
        "SELECT t.key_format, r.data FROM data_table t JOIN data_record r \
           ON r.tenant_id = t.tenant_id AND r.table_id = t.id \
         WHERE t.tenant_id = $1 AND t.id = $2",
    )
    .bind(&tenant)
    .bind(broken)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (fmt, data),
        (1, json!({"Bad Name": "x"})),
        "途中失敗は無変更"
    );

    // --- テーブル単位: expense だけ先に移行（もう一度呼んでも何もしない＝冪等） ---
    assert!(store
        .migrate_table_field_keys(&tenant, expense)
        .await
        .unwrap());
    let snapshot = record_versions(&pool, &tenant, expense).await;
    assert!(!store
        .migrate_table_field_keys(&tenant, expense)
        .await
        .unwrap());
    assert_eq!(snapshot, record_versions(&pool, &tenant, expense).await);

    // 移行結果: JSONB キーは f{id}・record_int / rev / updated_at は不変。
    let after: Vec<(Uuid, i64, i64, String, Value)> = sqlx::query_as(
        "SELECT id, record_int, rev, updated_at::text, data FROM data_record \
         WHERE tenant_id = $1 AND table_id = $2 ORDER BY id",
    )
    .bind(&tenant)
    .bind(expense)
    .fetch_all(&pool)
    .await
    .unwrap();
    for ((id0, ri0, rev0, up0), (id1, ri1, rev1, up1, data)) in
        record_ints_before.iter().zip(after.iter())
    {
        assert_eq!((id0, ri0, rev0, up0), (id1, ri1, rev1, up1));
        let keys: Vec<&String> = data.as_object().unwrap().keys().collect();
        assert!(
            keys.iter()
                .all(|k| ["f16", "f17", "f18", "f19"].contains(&k.as_str())),
            "{data}"
        );
    }
    // 旧式インデックスは消え、新キーの式インデックスが台帳に載る。
    let legacy_left: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_indexes WHERE indexname = $1)")
            .bind(format!("dr_{}_legacy01", expense.simple()))
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!legacy_left, "v1 の式インデックスが残っている");
    let defs: Vec<String> = sqlx::query_scalar(
        "SELECT i.indexdef FROM data_index_registry r JOIN pg_indexes i ON i.indexname = r.index_name \
         WHERE r.tenant_id = $1 AND r.table_id = $2 ORDER BY r.field",
    )
    .bind(&tenant)
    .bind(expense)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(defs.len(), 3, "amount/applicant/status の 3 本: {defs:?}");
    assert!(defs.iter().all(|d| d.contains("'f1")), "{defs:?}");

    // --- ジョブ経由の再開: 投入 → 配信したまま落ちた（ack なし）→ 可視タイムアウト後に再配信 ---
    store
        .enqueue_field_key_migration_if_needed()
        .await
        .expect("enqueue");
    {
        let mut conn = pool.acquire().await.unwrap();
        let claimed = jobq::claim(
            &mut conn,
            FIELD_KEY_MIGRATION_QUEUE,
            std::time::Duration::from_mins(10),
            10,
        )
        .await
        .unwrap();
        // ワーカーが処理前に落ちた扱い。可視タイムアウトの経過を模擬する。
        for j in &claimed {
            sqlx::query("UPDATE job_queue SET visible_at = now() WHERE id = $1")
                .bind(j.id)
                .execute(&mut *conn)
                .await
                .unwrap();
        }
    }
    // 壊れたテーブルはバッチを止めるので、運用者が直した想定で修復してから再開する。
    sqlx::query("UPDATE data_table SET schema = $3 WHERE tenant_id = $1 AND id = $2")
        .bind(&tenant)
        .bind(broken)
        .bind(json!({"fields": [{ "name": "bad_name", "type": "text" }]}))
        .execute(&pool)
        .await
        .unwrap();
    // 他テストの v1 テーブルも同じスイープで処理され得るので、自テナント分の完了を待つ。
    for _ in 0..50 {
        let left: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM data_table WHERE tenant_id = $1 AND key_format = 1",
        )
        .bind(&tenant)
        .fetch_one(&pool)
        .await
        .unwrap();
        if left == 0 {
            break;
        }
        match store.consume_field_key_migration_once().await {
            Ok(Some(_)) => {}
            Ok(None) => {
                store.enqueue_field_key_migration_if_needed().await.unwrap();
            }
            Err(e) => panic!("移行バッチが失敗: {e}"),
        }
    }
    let left: Vec<String> =
        sqlx::query_scalar("SELECT name FROM data_table WHERE tenant_id = $1 AND key_format = 1")
            .bind(&tenant)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(left.is_empty(), "未移行のテーブル: {left:?}");
    // expense はジョブで再処理されても変わらない（冪等）。
    assert_eq!(snapshot, record_versions(&pool, &tenant, expense).await);
    // 壊れていたテーブルは修復後の key で移行され、値は保たれる（v1 のキー "Bad Name" は宣言外）。
    let b = store.get_table(&alice, broken, None).await.unwrap();
    assert_eq!(b.schema.fields[0].key, "bad_name");

    // --- 移行後も v1 の操作が通る ---
    // row_policy: alice は自分の 2 件だけ、bob は 1 件。
    let mine = store
        .list_records(
            &alice,
            expense,
            &ListRecordsOptions {
                sort: Some(RecordSort {
                    field: "status".into(),
                    descending: false,
                }),
                limit: 50,
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(mine.items.len(), 2);
    assert_eq!(mine.items[0].data["status"], json!("draft"));
    // field_policy: 経理ロールでない alice には amount が見えない（filter に使うと 403）。
    assert!(mine.items.iter().all(|r| r.data.get("amount").is_none()));
    let masked_filter = store
        .run_query(
            &alice,
            expense,
            &DataQuery {
                filter: Some(QueryFilter {
                    field: "amount".into(),
                    value: json!(1200),
                }),
                ..Default::default()
            },
            None,
        )
        .await;
    assert!(
        matches!(masked_filter, Err(DataError::Forbidden)),
        "{masked_filter:?}"
    );
    // computed: 行内評価は key で動く。
    assert_eq!(mine.items[0].data["label"], json!("出張"));
    let theirs = store
        .list_records(
            &bob,
            expense,
            &ListRecordsOptions {
                limit: 50,
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(theirs.items.len(), 1);

    // 宣言的クエリ（索引付きフィルタ＋ソート）と集計（K 抑制）。
    let q = store
        .run_query(
            &alice,
            expense,
            &DataQuery {
                filter: Some(QueryFilter {
                    field: "status".into(),
                    value: json!("submitted"),
                }),
                sort: Some(QuerySort {
                    field: "applicant".into(),
                    descending: true,
                }),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(q.items.as_ref().unwrap().len(), 1);
    let agg = store
        .run_query(
            &alice,
            expense,
            &DataQuery {
                aggregate: Some(Aggregate {
                    group_by: vec!["status".into()],
                    metric: Metric::Count,
                    field: None,
                }),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    let groups = agg.groups.unwrap();
    assert!(
        groups.iter().all(|g| g.key.contains_key("status")),
        "{groups:?}"
    );

    // 更新・FSM 遷移・リビジョン（v1 の差分履歴も key のまま読める）。
    let target = mine.items[0].clone();
    let up = store
        .update_record(
            &alice,
            expense,
            target.id,
            json!({"title": "出張（修正）"}),
            target.rev,
            None,
        )
        .await
        .unwrap();
    let fsm = FsmBody {
        states: vec!["draft".into(), "submitted".into(), "approved".into()],
        transitions: vec![FsmTransition {
            from: "draft".into(),
            to: "submitted".into(),
            actor: PolicyExpr::FieldCmp {
                field: "applicant".into(),
                op: CmpOp::Eq,
                value: PolicyOperand::UserId,
            },
        }],
    };
    let moved = store
        .transition_record(
            &alice,
            expense,
            target.id,
            "submitted",
            up.rev,
            &fsm,
            "status",
            None,
        )
        .await
        .unwrap();
    assert_eq!(moved.data["status"], json!("submitted"));
    assert_eq!(moved.data["title"], json!("出張（修正）"));
    let revs = store
        .list_revisions(&alice, expense, target.id, None, 10, None)
        .await
        .unwrap();
    let kinds: Vec<&str> = revs.iter().map(|r| r.change_kind.as_str()).collect();
    assert_eq!(kinds, vec!["transition", "update", "create"]);
    assert!(
        revs[2].patch.iter().any(|p| p.field == "title"),
        "v1 の差分も key で読める"
    );
    // amount は経理以外にはマスクされる（差分の値は伏せる）。
    assert!(revs[2]
        .patch
        .iter()
        .filter(|p| p.field == "amount")
        .all(|p| p.new.is_null()));

    // lookup: 参照先（移行済み）の値を key で射影する。
    let orders = store
        .list_records(
            &alice,
            order,
            &ListRecordsOptions {
                limit: 10,
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(orders.items[0].data["cust_name"], json!("山田商事"));

    // スキーマ改訂（additive）も通り、新フィールドは next_field_id から採番される。
    let t = store.get_table(&alice, expense, None).await.unwrap();
    let mut next = t.schema.clone();
    next.fields.push(FieldDef::new("note", FieldType::Text));
    let up = store
        .update_table_schema(&alice, expense, next, None, None)
        .await
        .unwrap();
    assert_eq!(up.schema.field("note").unwrap().id, 21);
}

// ---------------------------------------------------------------------------
// 受け入れ条件 6: record_int は外へ出さない（negative test）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn record_int_never_leaks_to_responses_audit_outbox_or_errors() {
    let Some(base) = setup().await else { return };
    // 識別しやすい桁（13 桁）へ進める。新しいセッションはこの値以降からキャッシュを取る。
    sqlx::query(
        "SELECT setval('data_record_int_seq', \
           greatest((SELECT last_value FROM data_record_int_seq), 8100000000000) + 1000)",
    )
    .execute(&base)
    .await
    .unwrap();
    base.close().await;
    let pool = setup().await.unwrap();
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");

    let mut status = FieldDef::new("status", FieldType::Select);
    status.options = vec!["draft".into(), "submitted".into()];
    status.indexed = true;
    let mut code = FieldDef::new("code", FieldType::Text);
    code.unique = true;
    let mut sch = schema(vec![FieldDef::new("title", FieldType::Text), status, code]);
    sch.status_field = Some("status".into());
    let table = store
        .create_table(
            &c,
            NewDataTable {
                name: "leak".into(),
                schema: sch,
            },
            None,
        )
        .await
        .unwrap();

    let mut outputs: Vec<String> = Vec::new();
    outputs.push(serde_json::to_string(&table).unwrap());
    let a = store
        .create_record(
            &c,
            table.id,
            json!({"title": "a", "status": "draft", "code": "C1"}),
            None,
        )
        .await
        .unwrap();
    let b = store
        .create_record(
            &c,
            table.id,
            json!({"title": "b", "status": "draft", "code": "C2"}),
            None,
        )
        .await
        .unwrap();
    outputs.push(serde_json::to_string(&a).unwrap());
    let a2 = store
        .update_record(&c, table.id, a.id, json!({"title": "a2"}), 1, None)
        .await
        .unwrap();
    outputs.push(serde_json::to_string(&a2).unwrap());
    outputs.push(
        serde_json::to_string(&store.get_record(&c, table.id, b.id, None).await.unwrap()).unwrap(),
    );
    let page = store
        .list_records(
            &c,
            table.id,
            &ListRecordsOptions {
                limit: 50,
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    outputs.push(serde_json::to_string(&page.items).unwrap());
    let q = store
        .run_query(&c, table.id, &DataQuery::default(), None)
        .await
        .unwrap();
    outputs.push(serde_json::to_string(&q).unwrap());
    let revs = store
        .list_revisions(&c, table.id, a.id, None, 10, None)
        .await
        .unwrap();
    outputs.push(serde_json::to_string(&revs).unwrap());
    let fsm = FsmBody {
        states: vec!["draft".into(), "submitted".into()],
        transitions: vec![FsmTransition {
            from: "draft".into(),
            to: "submitted".into(),
            actor: PolicyExpr::Public,
        }],
    };
    let t = store
        .transition_record(
            &c,
            table.id,
            a.id,
            "submitted",
            a2.rev,
            &fsm,
            "status",
            None,
        )
        .await
        .unwrap();
    outputs.push(serde_json::to_string(&t).unwrap());
    outputs.push(serde_json::to_string(&store.list_tables(&c, 50).await.unwrap()).unwrap());
    // エラー文言（rev 競合・unique 違反・不存在・入力不正・不正なフィルタ）。
    let errs = [
        store
            .update_record(&c, table.id, b.id, json!({"title": "x"}), 99, None)
            .await
            .unwrap_err(),
        store
            .create_record(&c, table.id, json!({"title": "dup", "code": "C2"}), None)
            .await
            .unwrap_err(),
        store
            .get_record(&c, table.id, Uuid::new_v4(), None)
            .await
            .unwrap_err(),
        store
            .create_record(&c, table.id, json!({"nope": 1}), None)
            .await
            .unwrap_err(),
        store
            .delete_record(&c, table.id, b.id, 42, None)
            .await
            .unwrap_err(),
    ];
    for e in &errs {
        outputs.push(e.to_string());
    }

    let ints: Vec<i64> = sqlx::query_scalar(
        "SELECT record_int FROM data_record WHERE tenant_id = $1 AND table_id = $2",
    )
    .bind(&tenant)
    .bind(table.id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(ints.len(), 2, "全レコードに record_int が採番される");
    assert!(ints.iter().all(|i| *i > 8_100_000_000_000), "{ints:?}");
    let (tenant_int, table_int): (i32, i64) = sqlx::query_as(
        "SELECT tenant_int, table_int FROM data_table WHERE tenant_id = $1 AND id = $2",
    )
    .bind(&tenant)
    .bind(table.id)
    .fetch_one(&pool)
    .await
    .unwrap();

    // 監査ログ・outbox（SSE / workflow トリガの配信元）も検査対象に含める。
    let audits: Vec<String> = sqlx::query_scalar(
        "SELECT action || ' ' || object_type || ' ' || object_id || ' ' || coalesce(metadata::text, '') \
         FROM audit_log WHERE tenant_id = $1",
    )
    .bind(&tenant)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(!audits.is_empty(), "監査が記録されていること");
    outputs.extend(audits);
    let outbox: Vec<String> =
        sqlx::query_scalar("SELECT payload::text FROM storage_event_outbox WHERE tenant_id = $1")
            .bind(&tenant)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(
        !outbox.is_empty(),
        "遷移イベントが outbox に積まれていること"
    );
    outputs.extend(outbox);

    for out in &outputs {
        for name in ["record_int", "tenant_int", "table_int"] {
            assert!(!out.contains(name), "{name} が外部に出ている: {out}");
        }
        for i in &ints {
            assert!(
                !out.contains(&i.to_string()),
                "record_int {i} が外部に出ている: {out}"
            );
        }
        assert!(
            !out.contains(&format!("\"{table_int}\"")) && !out.contains(&format!(":{table_int},")),
            "table_int が外部に出ている: {out}"
        );
        let _ = tenant_int;
    }
}

// ---------------------------------------------------------------------------
// 内部整数キーのレジストリ（tenant_int は不変・table_int はグローバル一意）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn internal_keys_are_registered_and_consistent() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let mut tables = Vec::new();
    for n in ["a", "b"] {
        let t = store
            .create_table(
                &c,
                NewDataTable {
                    name: n.into(),
                    schema: schema(vec![FieldDef::new("title", FieldType::Text)]),
                },
                None,
            )
            .await
            .unwrap();
        store
            .create_record(&c, t.id, json!({"title": n}), None)
            .await
            .unwrap();
        tables.push(t.id);
    }
    let rows: Vec<(i32, i64)> = sqlx::query_as(
        "SELECT tenant_int, table_int FROM data_table WHERE tenant_id = $1 ORDER BY table_int",
    )
    .bind(&tenant)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, rows[1].0, "同一テナントは同じ tenant_int");
    assert_ne!(rows[0].1, rows[1].1, "table_int はテーブルごとに一意");
    let placement: i32 =
        sqlx::query_scalar("SELECT tenant_int FROM tenant_placement WHERE tenant_id = $1")
            .bind(&tenant)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(placement, rows[0].0, "tenant_int は配置レジストリの値");
    // data_record の内部キーは所属テーブルと一致する（索引テーブルの前提）。
    let mismatched: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM data_record r JOIN data_table t \
           ON t.tenant_id = r.tenant_id AND t.id = r.table_id \
         WHERE r.tenant_id = $1 AND (r.tenant_int <> t.tenant_int OR r.table_int <> t.table_int)",
    )
    .bind(&tenant)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(mismatched, 0);
    // 冪等: 再度の ensure は同じ値（再採番しない）。
    let mut conn = pool.acquire().await.unwrap();
    let again = storage::tenant::ensure_placement(&mut conn, &tenant)
        .await
        .unwrap();
    assert_eq!(again.tenant_int, placement);
}

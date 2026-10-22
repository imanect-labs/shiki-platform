//! pivot 索引の結合テスト（Task 15.2）の共通部品。authz は全許可モック。
#![allow(
    dead_code,
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stderr
)]

use std::sync::Arc;

use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use data::{DataStore, FieldDef, FieldType, NewDataTable, RefResolver, TableSchema};
use serde_json::Value;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

pub struct AllowAll;

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

pub struct FixedResolver;

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

pub async fn setup() -> Option<PgPool> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
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
    Some(pool)
}

pub fn store_with(pool: PgPool) -> DataStore {
    DataStore::new(pool, Arc::new(AllowAll), Arc::new(FixedResolver))
}

pub fn ctx(tenant: &str, user: &str) -> AuthContext {
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

pub fn unique_tenant() -> String {
    format!("t-{}", Uuid::new_v4())
}

pub fn schema(fields: Vec<FieldDef>) -> TableSchema {
    TableSchema {
        fields,
        status_field: None,
        row_policy: None,
        field_policy: vec![],
        aggregate_min_rows: None,
        fsm_ref: None,
    }
}

pub fn field(key: &str, ty: FieldType) -> FieldDef {
    FieldDef::new(key, ty)
}

pub async fn create_table(
    store: &DataStore,
    c: &AuthContext,
    name: &str,
    s: TableSchema,
) -> data::DataTable {
    store
        .create_table(
            c,
            NewDataTable {
                name: name.into(),
                schema: s,
            },
            None,
        )
        .await
        .expect("create table")
}

/// テーブルの内部整数キー（テスト専用に DB から直接引く・API は返さない）。
pub async fn keys_of(pool: &PgPool, tenant: &str, table: Uuid) -> (i32, i64) {
    sqlx::query_as("SELECT tenant_int, table_int FROM data_table WHERE tenant_id = $1 AND id = $2")
        .bind(tenant)
        .bind(table)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// レコードの record_int（テスト専用）。
pub async fn record_int_of(pool: &PgPool, tenant_int: i32, table_int: i64, id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT record_int FROM data_record WHERE tenant_int = $1 AND table_int = $2 AND id = $3",
    )
    .bind(tenant_int)
    .bind(table_int)
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// 台帳の状態（field_id, index_table, state）。
pub async fn ledger(pool: &PgPool, tenant_int: i32, table_int: i64) -> Vec<(i16, i16, String)> {
    sqlx::query_as(
        "SELECT field_id, index_table, state FROM data_index_state \
         WHERE tenant_int = $1 AND table_int = $2 ORDER BY field_id, index_table",
    )
    .bind(tenant_int)
    .bind(table_int)
    .fetch_all(pool)
    .await
    .unwrap()
}

/// EXPLAIN (FORMAT JSON) の計画に現れる `prefix` で始まる関係名（パーティション）の一覧。
pub fn relations_in_plan(plan: &Value, prefix: &str) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(v: &Value, prefix: &str, out: &mut Vec<String>) {
        match v {
            Value::Object(m) => {
                if let Some(Value::String(r)) = m.get("Relation Name") {
                    if r.starts_with(prefix) {
                        out.push(r.clone());
                    }
                }
                for x in m.values() {
                    walk(x, prefix, out);
                }
            }
            Value::Array(a) => {
                for x in a {
                    walk(x, prefix, out);
                }
            }
            _ => {}
        }
    }
    walk(plan, prefix, &mut out);
    out
}

/// 種別ごとのフィールド（text 索引＋部分一致・number 索引＋unique・multi_select 索引・select unique）。
pub fn rich_schema() -> TableSchema {
    let mut title = field("title", FieldType::Text);
    title.indexed = true;
    title.searchable = true;
    let mut amount = field("amount", FieldType::Number);
    amount.indexed = true;
    amount.unique = true;
    let mut tags = field("tags", FieldType::MultiSelect);
    tags.indexed = true;
    tags.options = vec!["a".into(), "b".into(), "c".into(), "d".into()];
    let mut status = field("status", FieldType::Select);
    status.indexed = true;
    status.options = vec!["draft".into(), "submitted".into(), "approved".into()];
    let mut due = field("due", FieldType::Date);
    due.indexed = true;
    let memo = field("memo", FieldType::Text); // 未索引
    schema(vec![title, amount, tags, status, due, memo])
}

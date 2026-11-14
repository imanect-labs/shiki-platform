//! テーブル一覧の名前の検索と続きの位置（`DataStore::list_tables_page`・GAP-42）の結合テスト
//! （実 Postgres ＋実 OpenFGA）。
//!
//! - 検索も続きも viewer 実効集合の中だけで絞る（見えないテーブルの名前には当たらない）
//! - 続きの位置で読み進めると、見えるテーブルを重ならず漏れなく、更新の新しい順に一巡する
//! - `%`・`_` は文字そのもの（全件に当たる語を作らせない）。読めない位置は `cursor_invalid`
//! - 閲覧者向けの 1 ページは、一覧と同じく読める列だけを返す
//!
//! 各テストは自分のテナント（`t-<uuid>`）に閉じ、最後にそのテナントを撤去する。

#![allow(
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal, Relation};
use data::{
    DataError, DataStore, FieldDef, FieldPolicy, FieldType, InvalidCode, NewDataTable, PolicyExpr,
    RefResolver, TableListQuery, TableSchema,
};
use sqlx::postgres::PgPoolOptions;
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

async fn setup() -> Option<(DataStore, Arc<dyn AuthzClient>)> {
    let (Ok(db_url), Ok(base_url)) = (
        shiki_testenv::var("STORAGE_TEST_DATABASE_URL"),
        shiki_testenv::var("OPENFGA_TEST_URL"),
    ) else {
        eprintln!("STORAGE_TEST_DATABASE_URL / OPENFGA_TEST_URL 未設定のためスキップ");
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
    let model: serde_json::Value =
        serde_json::from_str(include_str!("../../authz/model/authorization-model.json"))
            .expect("model json");
    let fga = OpenFgaClient::connect(
        reqwest::Client::new(),
        &OpenFgaConfig {
            base_url,
            store_name: format!("shiki-data-table-list-{}", Uuid::new_v4()),
        },
        &model,
    )
    .await
    .expect("OpenFGA 接続");
    let authz: Arc<dyn AuthzClient> = Arc::new(fga);
    Some((
        DataStore::new(pool, Arc::clone(&authz), Arc::new(AllowRefs)),
        authz,
    ))
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

fn schema(fields: Vec<FieldDef>) -> TableSchema {
    TableSchema {
        fields,
        status_field: None,
        row_policy: None,
        field_policy: vec![],
        aggregate_min_rows: None,
        fsm_ref: None,
        rag: None,
    }
}

async fn table(store: &DataStore, owner: &AuthContext, name: &str, s: TableSchema) -> Uuid {
    store
        .create_table(
            owner,
            NewDataTable {
                name: name.into(),
                schema: s,
            },
            None,
        )
        .await
        .expect("create table")
        .id
}

async fn grant_viewer(authz: &Arc<dyn AuthzClient>, who: &AuthContext, table: Uuid) {
    authz
        .write_tuple(
            &who.subject(),
            Relation::Viewer,
            &who.ns().data_table(&table.to_string()),
        )
        .await
        .expect("grant");
}

fn page(q: Option<&str>, cursor: Option<String>, limit: i64) -> TableListQuery {
    TableListQuery {
        q: q.map(str::to_string),
        cursor,
        limit,
    }
}

fn names(items: &[data::DataTable]) -> Vec<String> {
    items.iter().map(|t| t.name.clone()).collect()
}

/// 検索と続きは見えるテーブルの中だけ。読み進めると重ならず漏れなく、新しい順に一巡する。
#[tokio::test]
async fn search_and_cursor_stay_within_the_viewer_set() {
    let Some((store, authz)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let plain = || schema(vec![FieldDef::new("name", FieldType::Text)]);
    // bob に見せる 5 つ（うち 3 つが「経費」）と、見せない「経費」1 つ。
    let mut shown = Vec::new();
    for name in [
        "経費-申請-A",
        "備品",
        "経費-申請-B",
        "10%_off キャンペーン",
        "経費-精算",
    ] {
        let id = table(&store, &alice, name, plain()).await;
        grant_viewer(&authz, &bob, id).await;
        shown.push(id);
    }
    let hidden = table(&store, &alice, "経費-秘密", plain()).await;

    // 名前の検索（大文字小文字・前後の空白を問わない）は見えるものだけに当たる。
    let hit = store
        .list_tables_page(&bob, &page(Some(" 経費 "), None, 50))
        .await
        .unwrap();
    assert_eq!(
        names(&hit.items),
        vec!["経費-精算", "経費-申請-B", "経費-申請-A"],
        "新しい順・見えない「経費-秘密」は出ない"
    );
    assert!(hit.next_cursor.is_none());
    assert!(hit.items.iter().all(|t| t.id != hidden));
    // owner の alice には見えないテーブルも当たる（viewer 実効集合が違う）。
    let mine = store
        .list_tables_page(&alice, &page(Some("経費"), None, 50))
        .await
        .unwrap();
    assert_eq!(mine.items.len(), 4);
    assert_eq!(mine.items[0].id, hidden, "一番新しい");

    // LIKE の記号は文字そのもの（`%` は全件に、`_` は任意の 1 文字に当たらない）。
    let pct = store
        .list_tables_page(&bob, &page(Some("%"), None, 50))
        .await
        .unwrap();
    assert_eq!(names(&pct.items), vec!["10%_off キャンペーン"]);
    let under = store
        .list_tables_page(&bob, &page(Some("経費_"), None, 50))
        .await
        .unwrap();
    assert!(under.items.is_empty(), "{:?}", names(&under.items));
    let case = store
        .list_tables_page(&bob, &page(Some("OFF"), None, 50))
        .await
        .unwrap();
    assert_eq!(names(&case.items), vec!["10%_off キャンペーン"]);

    // 2 件ずつ読み進める: 3 ページで 5 つを重ならず漏れなく、新しい順に。
    let mut seen = Vec::new();
    let mut cursor = None;
    let mut pages = 0;
    loop {
        let p = store
            .list_tables_page(&bob, &page(None, cursor.clone(), 2))
            .await
            .unwrap();
        assert!(p.items.len() <= 2);
        seen.extend(p.items.iter().map(|t| t.id));
        pages += 1;
        match p.next_cursor {
            Some(c) => cursor = Some(c),
            None => break,
        }
        assert!(pages < 10, "続きが終わらない");
    }
    assert_eq!(pages, 3);
    let mut want = shown.clone();
    want.reverse();
    assert_eq!(seen, want, "新しい順に一巡する");
    assert_eq!(seen.iter().collect::<HashSet<_>>().len(), 5, "重ならない");

    // 検索と続きを組み合わせても、検索に当たるものだけを読み進める。
    let first = store
        .list_tables_page(&bob, &page(Some("経費"), None, 2))
        .await
        .unwrap();
    assert_eq!(names(&first.items), vec!["経費-精算", "経費-申請-B"]);
    let rest = store
        .list_tables_page(&bob, &page(Some("経費"), first.next_cursor, 2))
        .await
        .unwrap();
    assert_eq!(names(&rest.items), vec!["経費-申請-A"]);
    assert!(rest.next_cursor.is_none(), "ちょうど終わったら続きは無い");

    // 件数は 1〜200 に丸める（0 は 1 件）。
    let one = store
        .list_tables_page(&bob, &page(None, None, 0))
        .await
        .unwrap();
    assert_eq!(one.items.len(), 1);
    assert!(one.next_cursor.is_some());

    // 読めない位置は cursor_invalid、長すぎる語は 400。
    for bad in ["garbage", "MTIzNA"] {
        let e = store
            .list_tables_page(&bob, &page(None, Some(bad.into()), 2))
            .await
            .unwrap_err();
        assert_eq!(e.invalid_code(), Some(InvalidCode::CursorInvalid), "{e:?}");
    }
    let e = store
        .list_tables_page(&bob, &page(Some(&"あ".repeat(129)), None, 2))
        .await
        .unwrap_err();
    assert!(matches!(e, DataError::InvalidInput(_)), "{e:?}");

    store.purge_tenant(&tenant).await.unwrap();
    let left = store
        .list_tables_page(&alice, &page(None, None, 50))
        .await
        .unwrap();
    assert!(left.items.is_empty(), "後片付けで消える");
}

/// 閲覧者向けの 1 ページは、一覧と同じく読める列だけ・ポリシーの定義なしで返す。
#[tokio::test]
async fn viewer_pages_are_redacted_like_the_list() {
    let Some((store, authz)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let mut s = schema(vec![
        FieldDef::new("name", FieldType::Text),
        FieldDef::new("secret_code", FieldType::Text),
    ]);
    s.field_policy.push(FieldPolicy {
        field: "secret_code".into(),
        readable_by: PolicyExpr::HasRole {
            role: "bmgr".into(),
            subtree: false,
        },
    });
    let id = table(&store, &alice, "staff", s).await;
    grant_viewer(&authz, &bob, id).await;

    let p = store
        .list_tables_for_viewer_page(&bob, &page(Some("sta"), None, 10))
        .await
        .unwrap();
    assert_eq!(p.items.len(), 1);
    let keys: Vec<&str> = p.items[0]
        .schema
        .fields
        .iter()
        .map(|f| f.key.as_str())
        .collect();
    assert_eq!(keys, vec!["name"]);
    let text = serde_json::to_string(&p.items).unwrap();
    for leak in ["secret_code", "bmgr"] {
        assert!(!text.contains(leak), "{leak}: {text}");
    }
    // owner（スキーマ管理者）には全体。
    let owner = store
        .list_tables_for_viewer_page(&alice, &page(Some("sta"), None, 10))
        .await
        .unwrap();
    assert_eq!(owner.items[0].schema.fields.len(), 2);

    store.purge_tenant(&tenant).await.unwrap();
}

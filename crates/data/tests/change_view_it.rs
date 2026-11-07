//! レコード変更の見え方（`DataStore::change_event_view`）と一括の目印（`bulk_flags`）の結合テスト
//! （GAP-07 の T1・実 Postgres ＋実 OpenFGA）。
//!
//! - 行述語で見えない行の変更は `None`（発火しない）
//! - 読めない列（マスク）だけの更新は `None`。見える変更でも、読めない列は `changed`・`changes`・`record` に載らない
//! - FSM 遷移は遷移元・遷移先を返す
//! - 削除済みの行は `None`。rev が進んだ後は今の値を返す
//! - テーブルの viewer でない主体は `NotFound`
//! - 一括 upsert の作成行・更新行には一括の目印が付き、1 件の書込には付かない

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::too_many_lines
)]

use std::sync::Arc;

use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal, Relation};
use data::{
    BulkImportOptions, BulkUpsertRow, ChangeEventKind, ChangeEventRef, CmpOp, DataError, DataStore,
    FieldDef, FieldPolicy, FieldType, FsmBody, FsmRef, FsmStore, FsmTransition, NewDataTable,
    PolicyExpr, PolicyOperand, RowPolicy, TableSchema,
};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

struct AllowRefs;
#[async_trait::async_trait]
impl data::RefResolver for AllowRefs {
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
        .expect("pg");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    let model: serde_json::Value =
        serde_json::from_str(include_str!("../../authz/model/authorization-model.json")).unwrap();
    let config = OpenFgaConfig {
        base_url,
        store_name: format!("shiki-data-change-view-{}", Uuid::new_v4()),
    };
    let fga = OpenFgaClient::connect(reqwest::Client::new(), &config, &model)
        .await
        .expect("fga");
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
        authz::TenantId::parse(tenant).unwrap(),
    )
}

/// 申請の表: 行は申請者（owner）と担当者だけが読める・メモは経理（role keiri）だけが読める。
fn schema() -> TableSchema {
    let mut status = FieldDef::new("status", FieldType::Select);
    status.options = vec!["draft".into(), "submitted".into()];
    status.indexed = true;
    let mut assignee = FieldDef::new("assignee", FieldType::UserRef);
    assignee.indexed = true;
    TableSchema {
        fields: vec![
            FieldDef::new("title", FieldType::Text),
            FieldDef::new("amount", FieldType::Number),
            FieldDef::new("memo", FieldType::Text),
            assignee,
            status,
        ],
        status_field: Some("status".into()),
        row_policy: Some(RowPolicy {
            read: PolicyExpr::Any(vec![
                PolicyExpr::IsOwner,
                PolicyExpr::FieldCmp {
                    field: "assignee".into(),
                    op: CmpOp::Eq,
                    value: PolicyOperand::UserId,
                },
            ]),
            write: None,
        }),
        field_policy: vec![FieldPolicy {
            field: "memo".into(),
            readable_by: PolicyExpr::HasRole {
                role: "keiri".into(),
                subtree: true,
            },
        }],
        aggregate_min_rows: None,
        fsm_ref: None,
        rag: None,
    }
}

fn fsm() -> FsmBody {
    FsmBody {
        states: vec!["draft".into(), "submitted".into()],
        transitions: vec![FsmTransition {
            from: "draft".into(),
            to: "submitted".into(),
            actor: PolicyExpr::IsOwner,
        }],
    }
}

fn change(table_id: Uuid, record_id: Uuid, rev: i64, kind: ChangeEventKind) -> ChangeEventRef {
    ChangeEventRef {
        table_id,
        record_id,
        rev,
        kind,
    }
}

/// テナントのレコード変更の outbox（id 昇順）: (id, record_id, change)。
async fn outbox(pool: &PgPool, tenant: &str) -> Vec<(i64, Uuid, String)> {
    sqlx::query_as(
        "SELECT id, node_id, payload->>'change' FROM storage_event_outbox \
         WHERE tenant_id = $1 AND payload->>'event_type' = 'data.record.changed' ORDER BY id",
    )
    .bind(tenant)
    .fetch_all(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn change_view_follows_row_and_field_visibility() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice"); // 申請者（経理）
    let bob = ctx(&tenant, "bob"); // 担当者（経理でない）
    let carol = ctx(&tenant, "carol"); // テーブルの viewer・行は読めない
    let dave = ctx(&tenant, "dave"); // テーブルの viewer でない
    let store = DataStore::new(pool.clone(), Arc::clone(&authz), Arc::new(AllowRefs));
    let artifacts = Arc::new(artifact::ArtifactStore::new(
        pool.clone(),
        Arc::clone(&authz),
    ));
    let fsms = FsmStore::new(Arc::clone(&artifacts), store.clone());
    authz
        .write_tuple(
            &alice.subject(),
            Relation::Member,
            &alice.ns().role("keiri"),
        )
        .await
        .unwrap();
    let table = store
        .create_table(
            &alice,
            NewDataTable {
                name: "requests".into(),
                schema: schema(),
            },
            None,
        )
        .await
        .unwrap();
    let fsm_id = fsms
        .create(&alice, "flow", table.id, &fsm(), None)
        .await
        .unwrap();
    let mut pinned = schema();
    pinned.fsm_ref = Some(FsmRef {
        artifact_id: fsm_id,
        version: 1,
    });
    store
        .update_table_schema(&alice, table.id, pinned, Some(1), None)
        .await
        .unwrap();
    for who in [&bob, &carol] {
        authz
            .write_tuple(
                &who.subject(),
                Relation::Viewer,
                &alice.ns().data_table(&table.id.to_string()),
            )
            .await
            .unwrap();
    }
    let rec = store
        .create_record(
            &alice,
            table.id,
            json!({ "title": "備品の購入", "amount": 1200, "memo": "社内のみ",
                    "assignee": "bob", "status": "draft" }),
            None,
        )
        .await
        .unwrap();

    // 作成: 担当者には見える（読めない列は載らない）。行の読めない主体・テーブルの読めない主体には見えない。
    let created = change(table.id, rec.id, 1, ChangeEventKind::Create);
    let v = store
        .change_event_view(&bob, &created, None)
        .await
        .unwrap()
        .expect("担当者に見える");
    assert_eq!(v.change, "create");
    assert_eq!(v.actor, "alice");
    assert!(v.changed.contains(&"title".to_string()), "{:?}", v.changed);
    assert!(
        !v.changed.contains(&"memo".to_string()),
        "読めない列: {:?}",
        v.changed
    );
    assert!(!v.changes.contains_key("memo"));
    assert!(v.record.data.get("memo").is_none(), "{}", v.record.data);
    assert_eq!(v.record.data["title"], "備品の購入");
    assert!(store
        .change_event_view(&carol, &created, None)
        .await
        .unwrap()
        .is_none());
    assert!(matches!(
        store.change_event_view(&dave, &created, None).await,
        Err(DataError::NotFound)
    ));

    // 読めない列だけの更新は担当者には見えない（経理には見える）。
    let memo = store
        .update_record(
            &alice,
            table.id,
            rec.id,
            json!({ "memo": "差し替え" }),
            1,
            None,
        )
        .await
        .unwrap();
    let memo_ev = change(table.id, rec.id, memo.rev, ChangeEventKind::Update);
    assert!(store
        .change_event_view(&bob, &memo_ev, None)
        .await
        .unwrap()
        .is_none());
    let mine = store
        .change_event_view(&alice, &memo_ev, None)
        .await
        .unwrap()
        .expect("経理には見える");
    assert_eq!(mine.changed, vec!["memo".to_string()]);
    assert_eq!(
        mine.changes["memo"],
        json!({ "old": "社内のみ", "new": "差し替え" })
    );

    // 見える列の更新は、変わった列と前後の値を返す。
    let upd = store
        .update_record(
            &alice,
            table.id,
            rec.id,
            json!({ "amount": 1500 }),
            memo.rev,
            None,
        )
        .await
        .unwrap();
    let upd_ev = change(table.id, rec.id, upd.rev, ChangeEventKind::Update);
    let v = store
        .change_event_view(&bob, &upd_ev, None)
        .await
        .unwrap()
        .expect("見える列の変更");
    assert_eq!(v.change, "update");
    assert_eq!(v.changed, vec!["amount".to_string()]);
    assert_eq!(v.changes["amount"], json!({ "old": 1200, "new": 1500 }));
    assert_eq!(v.from, None);

    // 遷移は遷移元・遷移先を返す（状態列の更新としての変更も同じ rev）。
    let moved = fsms
        .transition(&alice, table.id, rec.id, "submitted", upd.rev, None)
        .await
        .unwrap();
    for kind in [ChangeEventKind::Transition, ChangeEventKind::Update] {
        let v = store
            .change_event_view(&bob, &change(table.id, rec.id, moved.rev, kind), None)
            .await
            .unwrap()
            .expect("遷移が見える");
        assert_eq!(v.change, "transition");
        assert_eq!(v.from.as_deref(), Some("draft"));
        assert_eq!(v.to.as_deref(), Some("submitted"));
        assert_eq!(v.changed, vec!["status".to_string()]);
    }

    // rev が進んだ後に古い変更を見ると、record は今の値（relay の時点の値）。
    let v = store
        .change_event_view(&bob, &upd_ev, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(v.record.rev, moved.rev);
    assert_eq!(v.record.data["status"], "submitted");

    // 担当を外すと、同じ変更でも見えない（呼んだ時点の行で判定する）。
    let reassigned = store
        .update_record(
            &alice,
            table.id,
            rec.id,
            json!({ "assignee": "alice" }),
            moved.rev,
            None,
        )
        .await
        .unwrap();
    assert!(store
        .change_event_view(&bob, &upd_ev, None)
        .await
        .unwrap()
        .is_none());

    // 削除した行の変更は見えない。
    store
        .delete_record(&alice, table.id, rec.id, reassigned.rev, None)
        .await
        .unwrap();
    assert!(store
        .change_event_view(&alice, &upd_ev, None)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn bulk_rows_are_flagged_for_creates_and_updates() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let store = DataStore::new(pool.clone(), Arc::clone(&authz), Arc::new(AllowRefs));
    let mut sch = schema();
    sch.row_policy = None;
    sch.field_policy = vec![];
    sch.status_field = None;
    let table = store
        .create_table(
            &alice,
            NewDataTable {
                name: "bulk".into(),
                schema: sch,
            },
            None,
        )
        .await
        .unwrap();
    let one = store
        .create_record(&alice, table.id, json!({ "title": "一件" }), None)
        .await
        .unwrap();
    let rows = vec![
        BulkUpsertRow {
            key: None,
            record_id: Some(one.id),
            expected_rev: None,
            data: json!({ "title": "一括で更新" }),
        },
        BulkUpsertRow {
            key: Some("n1".into()),
            record_id: None,
            expected_rev: None,
            data: json!({ "title": "一括で作成" }),
        },
    ];
    let import_id = store
        .start_bulk_upsert(&alice, table.id, rows, BulkImportOptions::default(), None)
        .await
        .unwrap();
    // 取込ジョブのワーカーに拾わせず、この場で最後まで進める。
    sqlx::query("DELETE FROM job_queue WHERE queue = $1 AND payload->>'import_id' = $2")
        .bind(data::BULK_IMPORT_QUEUE)
        .bind(import_id.to_string())
        .execute(&pool)
        .await
        .unwrap();
    let step = store
        .run_bulk_import(&tenant, table.id, import_id)
        .await
        .unwrap();
    assert_eq!(step.status, "done");
    store
        .update_record(&alice, table.id, one.id, json!({ "amount": 3 }), 2, None)
        .await
        .unwrap();
    let events = outbox(&pool, &tenant).await;
    let ids: Vec<i64> = events.iter().map(|e| e.0).collect();
    let flags = store.bulk_flags(&tenant, &ids).await.unwrap();
    let kinds: Vec<(String, bool)> = events
        .iter()
        .map(|(id, _, change)| (change.clone(), flags[id]))
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("create".to_string(), false),
            ("create".to_string(), true),
            ("update".to_string(), true),
            ("update".to_string(), false),
        ],
        "1 件の書込は目印なし・一括の作成と更新は目印あり"
    );
}

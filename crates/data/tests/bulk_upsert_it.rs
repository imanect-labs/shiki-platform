//! 一括 upsert（Task 15.7・15.11 からの申し送り）の結合テスト（実 Postgres ＋実 OpenFGA）。
//!
//! 一括書込パスの更新行が **1 件の更新と同じ入口**を通ることを確かめる:
//! - 行述語つきロック（不可視・不在は「見つからない」）・書込述語（row_policy.write）
//! - `rev` の照合（指定した行だけ）・マスク列への書込拒否（PIT-19）
//! - 同じ検証器（required の除去・unique）・同じ副作用（索引の差分更新・リビジョン・outbox・監査）
//! - 行の誤りはその行だけを拒否し、同じチャンクの他の行（作成・更新）は適用される

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::print_stderr
)]

mod common;

use std::sync::Arc;

use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthzClient, Relation};
use common::{ctx, field, schema, setup, unique_tenant, FixedResolver};
use data::{
    BulkImportOptions, BulkUpsertRow, CmpOp, CondOp, Condition, DataQuery, DataStore, FieldPolicy,
    FieldType, NewDataTable, PolicyExpr, PolicyOperand, RowPolicy,
};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

async fn fga(pool: PgPool) -> Option<(DataStore, Arc<dyn AuthzClient>)> {
    let Ok(base_url) = std::env::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return None;
    };
    let model: Value =
        serde_json::from_str(include_str!("../../authz/model/authorization-model.json")).unwrap();
    let fga: Arc<dyn AuthzClient> = Arc::new(
        OpenFgaClient::connect(
            reqwest::Client::new(),
            &OpenFgaConfig {
                base_url,
                store_name: format!("shiki-data-upsert-test-{}", Uuid::new_v4()),
            },
            &model,
        )
        .await
        .expect("OpenFGA 接続"),
    );
    Some((
        DataStore::new(pool, Arc::clone(&fga), Arc::new(FixedResolver)),
        fga,
    ))
}

/// 取込のジョブをキューから外す（このテストは自分で最後まで進める）。
async fn detach_job(pool: &PgPool, import_id: Uuid) {
    sqlx::query("DELETE FROM job_queue WHERE queue = $1 AND payload->>'import_id' = $2")
        .bind(data::BULK_IMPORT_QUEUE)
        .bind(import_id.to_string())
        .execute(pool)
        .await
        .unwrap();
}

fn upd(id: Uuid, rev: Option<i64>, patch: Value) -> BulkUpsertRow {
    BulkUpsertRow {
        key: None,
        record_id: Some(id),
        expected_rev: rev,
        data: patch,
    }
}

#[tokio::test]
async fn bulk_upsert_updates_through_the_single_update_entry() {
    let Some(pool) = setup().await else { return };
    let Some((store, fga)) = fga(pool.clone()).await else {
        return;
    };
    let tenant = unique_tenant();
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");

    let mut title = field("title", FieldType::Text);
    title.indexed = true;
    title.required = true;
    let mut amount = field("amount", FieldType::Number);
    amount.indexed = true;
    amount.unique = true;
    let mut assignee = field("assignee", FieldType::UserRef);
    assignee.indexed = true;
    let salary = field("salary", FieldType::Number);
    let mut s = schema(vec![title, amount, assignee, salary]);
    // 読取は全員・書込は担当者本人だけ。給与列は hr ロールだけが読める（それ以外はマスク）。
    s.row_policy = Some(RowPolicy {
        read: PolicyExpr::Public,
        write: Some(PolicyExpr::FieldCmp {
            field: "assignee".into(),
            op: CmpOp::Eq,
            value: PolicyOperand::UserId,
        }),
    });
    s.field_policy = vec![FieldPolicy {
        field: "salary".into(),
        readable_by: PolicyExpr::HasRole {
            role: "hr".into(),
            subtree: false,
        },
    }];
    let table = store
        .create_table(
            &alice,
            NewDataTable {
                name: "staff".into(),
                schema: s,
            },
            None,
        )
        .await
        .unwrap();
    fga.write_tuple(
        &bob.subject(),
        Relation::Editor,
        &alice.ns().data_table(&table.id.to_string()),
    )
    .await
    .unwrap();

    let mk = |t: &str, a: i64, who: &str| json!({"title": t, "amount": a, "assignee": who});
    let mut ids = Vec::new();
    for (t, a, who) in [
        ("r1", 1, "alice"),
        ("r2", 2, "bob"),
        ("r3", 3, "alice"),
        ("r4", 4, "alice"),
        ("r5", 5, "alice"),
        ("r6", 6, "alice"),
    ] {
        ids.push(
            store
                .create_record(&alice, table.id, mk(t, a, who), None)
                .await
                .unwrap()
                .id,
        );
    }
    // 給与（alice には読めない列）を書いた行を作っておく（作成はマスク列を書ける・読めないだけ）。
    sqlx::query(
        "UPDATE data_record SET data = data || jsonb_build_object('f' || $3::text, 500) \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(&tenant)
    .bind(ids[0])
    .bind(
        store
            .get_table(&alice, table.id, None)
            .await
            .unwrap()
            .schema
            .field("salary")
            .unwrap()
            .id
            .to_string(),
    )
    .execute(&pool)
    .await
    .unwrap();
    let missing = Uuid::new_v4();

    let rows = vec![
        // ① 適用される更新（rev 一致）。
        upd(ids[0], Some(1), json!({"title": "r1-upd"})),
        // ② 書込述語（担当者は bob）で拒否。
        upd(ids[1], None, json!({"title": "stolen"})),
        // ③ rev 不一致で拒否。
        upd(ids[2], Some(99), json!({"title": "stale"})),
        // ④ マスク列（salary）への書込で拒否。
        upd(ids[3], None, json!({"salary": 100})),
        // ⑤ 不在のレコード。
        upd(missing, None, json!({"title": "ghost"})),
        // ⑥ 作成行（amount = 10）。作成はチャンクの更新より先に適用される。
        BulkUpsertRow {
            key: Some("new".into()),
            record_id: None,
            expected_rev: None,
            data: json!({"title": "new", "amount": 10, "assignee": "alice"}),
        },
        // ⑦ ⑥ と unique が衝突する更新で拒否。
        upd(ids[4], None, json!({"amount": 10})),
        // ⑧ required の除去で拒否。
        upd(ids[5], None, json!({"title": null})),
    ];
    let import_id = store
        .start_bulk_upsert(
            &alice,
            table.id,
            rows,
            BulkImportOptions::default(),
            Some("trace-upsert"),
        )
        .await
        .unwrap();
    detach_job(&pool, import_id).await;
    let step = store
        .run_bulk_import(&tenant, table.id, import_id)
        .await
        .unwrap();
    assert_eq!(step.status, "done");
    let st = store
        .bulk_import_status(&alice, table.id, import_id)
        .await
        .unwrap();
    assert_eq!(st.rows_total, 8);
    assert_eq!(st.rows_invalid, 0, "更新行は検証段では形だけを見る");
    assert_eq!(st.rows_applied, 2, "① と ⑥ だけが適用される: {st:?}");
    assert_eq!(st.rows_rejected, 6, "{st:?}");
    let errors = store
        .bulk_import_errors(&alice, table.id, import_id, 0, 100)
        .await
        .unwrap();
    let by_row: std::collections::HashMap<i64, String> =
        errors.into_iter().map(|e| (e.row_no, e.message)).collect();
    assert!(by_row[&2].contains("権限"), "{by_row:?}");
    assert!(by_row[&3].contains("rev"), "{by_row:?}");
    assert!(by_row[&4].contains("権限"), "マスク列: {by_row:?}");
    assert!(by_row[&5].contains("見つかりません"), "{by_row:?}");
    assert!(by_row[&7].contains("unique"), "{by_row:?}");
    assert!(by_row.contains_key(&8), "required: {by_row:?}");
    assert!(!by_row.contains_key(&1) && !by_row.contains_key(&6));

    // ① は 1 件の更新と同じ結果（rev+1・索引の差分更新でクエリに乗る）。
    let r1 = store
        .get_record(&alice, table.id, ids[0], None)
        .await
        .unwrap();
    assert_eq!(r1.rev, 2);
    assert_eq!(r1.data["title"], "r1-upd");
    let q = DataQuery {
        filter: Some(Condition::cmp("title", CondOp::Eq, Some(json!("r1-upd")))),
        ..DataQuery::default()
    };
    let hit = store.run_query(&alice, table.id, &q, None).await.unwrap();
    assert_eq!(
        hit.items.unwrap().iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![ids[0]],
        "更新後の値の索引行が書かれている"
    );
    // 拒否された行は変わらない。
    for (i, title) in [(1usize, "r2"), (2, "r3"), (4, "r5"), (5, "r6")] {
        let r = store
            .get_record(&alice, table.id, ids[i], None)
            .await
            .unwrap();
        assert_eq!(r.rev, 1, "{title}");
        assert_eq!(r.data["title"], title);
    }
    // ⑥ は作成された。
    let q = DataQuery {
        filter: Some(Condition::cmp("amount", CondOp::Eq, Some(json!(10)))),
        ..DataQuery::default()
    };
    let created = store
        .run_query(&alice, table.id, &q, None)
        .await
        .unwrap()
        .items
        .unwrap();
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].data["title"], "new");

    // 1 件の更新の応答も読めない列を返さない（PIT-19・Task 15.7 で塞いだ）。
    let r1b = store
        .update_record(
            &alice,
            table.id,
            ids[0],
            json!({"title": "r1-again"}),
            2,
            None,
        )
        .await
        .unwrap();
    assert!(r1b.data.get("salary").is_none(), "{:?}", r1b.data);
    let r1_raw: Value =
        sqlx::query_scalar("SELECT data FROM data_record WHERE tenant_id = $1 AND id = $2")
            .bind(&tenant)
            .bind(ids[0])
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        r1_raw
            .as_object()
            .unwrap()
            .values()
            .any(|v| v == &json!(500)),
        "保存値には給与が残っている（応答だけが伏せる）"
    );

    // リビジョン・outbox・監査も 1 件の更新と同じ入口で書かれている。
    let (rev_kind,): (String,) = sqlx::query_as(
        "SELECT change_kind FROM data_record_revision \
         WHERE tenant_id = $1 AND record_id = $2 AND rev = 2",
    )
    .bind(&tenant)
    .bind(ids[0])
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(rev_kind, "update");
    let audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND action = 'data.record.update' \
           AND object_id = $2",
    )
    .bind(&tenant)
    .bind(ids[0].to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        audits, 2,
        "更新 1 件ごとの監査（一括の 1 件＋直後の 1 件の更新）"
    );
    let changes: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM storage_event_outbox WHERE tenant_id = $1 \
           AND payload->>'event_type' = 'data.record.changed' AND payload->>'record_id' = $2",
    )
    .bind(&tenant)
    .bind(ids[0].to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(changes, 3, "作成と更新 2 回の変更イベント（SSE の源）");
}

#[tokio::test]
async fn bulk_upsert_rejects_malformed_input_up_front() {
    let Some(pool) = setup().await else { return };
    let Some((store, _)) = fga(pool.clone()).await else {
        return;
    };
    let tenant = unique_tenant();
    let alice = ctx(&tenant, "alice");
    let mut title = field("title", FieldType::Text);
    title.indexed = true;
    let table = store
        .create_table(
            &alice,
            NewDataTable {
                name: "t".into(),
                schema: schema(vec![title]),
            },
            None,
        )
        .await
        .unwrap();
    let id = Uuid::new_v4();
    // 同じレコードを 2 回更新する取込は全体を拒否。
    let err = store
        .start_bulk_upsert(
            &alice,
            table.id,
            vec![
                upd(id, None, json!({"title": "a"})),
                upd(id, None, json!({"title": "b"})),
            ],
            BulkImportOptions::default(),
            None,
        )
        .await
        .unwrap_err();
    assert!(err.is_invalid(), "{err:?}");
    // 作成行に expected_rev は付けられない。
    let err = store
        .start_bulk_upsert(
            &alice,
            table.id,
            vec![BulkUpsertRow {
                key: None,
                record_id: None,
                expected_rev: Some(1),
                data: json!({"title": "a"}),
            }],
            BulkImportOptions::default(),
            None,
        )
        .await
        .unwrap_err();
    assert!(err.is_invalid(), "{err:?}");
}

/// 取込 ID を決めて始める入口は冪等（ワークフローの再実行で二重に取り込まない）。
#[tokio::test]
async fn bulk_upsert_with_id_is_idempotent() {
    let Some(pool) = setup().await else { return };
    let Some((store, _)) = fga(pool.clone()).await else {
        return;
    };
    let tenant = unique_tenant();
    let alice = ctx(&tenant, "alice");
    let mut title = field("title", FieldType::Text);
    title.indexed = true;
    let table = store
        .create_table(
            &alice,
            NewDataTable {
                name: "t".into(),
                schema: schema(vec![title]),
            },
            None,
        )
        .await
        .unwrap();
    let other = store
        .create_table(
            &alice,
            NewDataTable {
                name: "u".into(),
                schema: schema(vec![field("x", FieldType::Text)]),
            },
            None,
        )
        .await
        .unwrap();
    let id = Uuid::new_v4();
    let rows = || {
        vec![BulkUpsertRow {
            key: None,
            record_id: None,
            expected_rev: None,
            data: json!({"title": "once"}),
        }]
    };
    for _ in 0..2 {
        let got = store
            .start_bulk_upsert_with_id(
                &alice,
                table.id,
                id,
                rows(),
                BulkImportOptions::default(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(got, id);
    }
    // 別のテーブルで同じ ID は 409。
    let err = store
        .start_bulk_upsert_with_id(
            &alice,
            other.id,
            id,
            rows(),
            BulkImportOptions::default(),
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, data::DataError::Conflict(_)), "{err:?}");
    detach_job(&pool, id).await;
    store.run_bulk_import(&tenant, table.id, id).await.unwrap();
    let q = DataQuery {
        filter: Some(Condition::cmp("title", CondOp::Eq, Some(json!("once")))),
        ..DataQuery::default()
    };
    let n = store
        .run_query(&alice, table.id, &q, None)
        .await
        .unwrap()
        .items
        .unwrap()
        .len();
    assert_eq!(n, 1, "2 回始めても 1 行だけ");
}

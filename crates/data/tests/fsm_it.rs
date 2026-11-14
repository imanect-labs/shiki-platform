//! FSM 宣言的ガードの結合テスト（Task 9.10 受け入れ条件）。
//!
//! 承認フロー（draft→submitted→approved/rejected）が定義どおり遷移し、定義外遷移・無権限
//! 遷移が拒否され、全遷移がリビジョン＋監査＋outbox に残ることを実 Postgres＋実 OpenFGA で検証。

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
    CmpOp, DataError, DataStore, FieldDef, FieldType, FsmBody, FsmRef, FsmStore, FsmTransition,
    NewDataTable, PolicyExpr, PolicyOperand, TableSchema,
};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
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
    let db_url = shiki_testenv::var("STORAGE_TEST_DATABASE_URL").ok()?;
    let base_url = shiki_testenv::var("OPENFGA_TEST_URL").ok()?;
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
        store_name: format!("shiki-data-fsm-test-{}", Uuid::new_v4()),
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

fn field(name: &str, ty: FieldType) -> FieldDef {
    FieldDef::new(name, ty)
}

fn expense_schema() -> TableSchema {
    let applicant = field("applicant", FieldType::UserRef);
    let approver = field("approver", FieldType::UserRef);
    let mut status = field("status", FieldType::Select);
    status.options = vec![
        "draft".into(),
        "submitted".into(),
        "approved".into(),
        "rejected".into(),
    ];
    status.indexed = true;
    TableSchema {
        fields: vec![
            applicant,
            approver,
            status,
            field("amount", FieldType::Number),
        ],
        status_field: Some("status".into()),
        row_policy: None,
        field_policy: vec![],
        aggregate_min_rows: None,
        fsm_ref: None,
        rag: None,
    }
}

fn approval_fsm() -> FsmBody {
    // draft→submitted は申請者本人、submitted→approved/rejected は承認者本人。
    let is_applicant = PolicyExpr::FieldCmp {
        field: "applicant".into(),
        op: CmpOp::Eq,
        value: PolicyOperand::UserId,
    };
    let is_approver = PolicyExpr::FieldCmp {
        field: "approver".into(),
        op: CmpOp::Eq,
        value: PolicyOperand::UserId,
    };
    FsmBody {
        states: vec![
            "draft".into(),
            "submitted".into(),
            "approved".into(),
            "rejected".into(),
        ],
        transitions: vec![
            FsmTransition {
                from: "draft".into(),
                to: "submitted".into(),
                actor: is_applicant,
            },
            FsmTransition {
                from: "submitted".into(),
                to: "approved".into(),
                actor: is_approver.clone(),
            },
            FsmTransition {
                from: "submitted".into(),
                to: "rejected".into(),
                actor: is_approver,
            },
        ],
    }
}

#[tokio::test]
async fn approval_flow_transitions_and_guards() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice"); // 申請者
    let bob = ctx(&tenant, "bob"); // 承認者
    let store = DataStore::new(pool.clone(), Arc::clone(&authz), Arc::new(AllowRefs));
    let artifacts = Arc::new(artifact::ArtifactStore::new(
        pool.clone(),
        Arc::clone(&authz),
    ));
    let fsms = FsmStore::new(Arc::clone(&artifacts), store.clone());

    // テーブル作成 → FSM 保存 → テーブルへ fsm_ref をピン。
    let table = store
        .create_table(
            &alice,
            NewDataTable {
                name: "expense".into(),
                schema: expense_schema(),
            },
            None,
        )
        .await
        .expect("table");
    let fsm_id = fsms
        .create(&alice, "approval", table.id, &approval_fsm(), None)
        .await
        .expect("fsm");
    let mut schema = expense_schema();
    schema.fsm_ref = Some(FsmRef {
        artifact_id: fsm_id,
        version: 1,
    });
    store
        .update_table_schema(&alice, table.id, schema, Some(1), None)
        .await
        .expect("pin fsm");
    // bob をテーブル editor に（遷移は editor 必要）。
    authz
        .write_tuple(
            &bob.subject(),
            Relation::Editor,
            &alice.ns().data_table(&table.id.to_string()),
        )
        .await
        .expect("bob editor");

    let rec = store
        .create_record(
            &alice,
            table.id,
            json!({"applicant": "alice", "approver": "bob", "status": "draft", "amount": 100}),
            None,
        )
        .await
        .expect("record");

    // status を直接 update しようとすると拒否（遷移 API 経由でのみ・Task 9.10）。
    let direct = store
        .update_record(
            &alice,
            table.id,
            rec.id,
            json!({"status": "approved"}),
            rec.rev,
            None,
        )
        .await;
    assert!(matches!(&direct, Err(e) if e.is_invalid()), "{direct:?}");

    let fsm = approval_fsm();
    // draft→submitted は申請者 alice のみ（承認者 bob は actor 述語外＝403）。
    let bob_submit = store
        .transition_record(
            &bob,
            table.id,
            rec.id,
            "submitted",
            rec.rev,
            &fsm,
            "status",
            None,
        )
        .await;
    assert!(
        matches!(bob_submit, Err(DataError::Forbidden)),
        "{bob_submit:?}"
    );
    let submitted = store
        .transition_record(
            &alice,
            table.id,
            rec.id,
            "submitted",
            rec.rev,
            &fsm,
            "status",
            None,
        )
        .await
        .expect("alice submits");
    assert_eq!(submitted.data["status"], json!("submitted"));
    assert_eq!(submitted.rev, rec.rev + 1);

    // 定義外遷移（submitted→draft）は拒否。
    let bad = store
        .transition_record(
            &alice,
            table.id,
            rec.id,
            "draft",
            submitted.rev,
            &fsm,
            "status",
            None,
        )
        .await;
    assert!(matches!(&bad, Err(e) if e.is_invalid()), "{bad:?}");

    // submitted→approved は承認者 bob のみ（申請者 alice は 403）。
    let alice_approve = store
        .transition_record(
            &alice,
            table.id,
            rec.id,
            "approved",
            submitted.rev,
            &fsm,
            "status",
            None,
        )
        .await;
    assert!(
        matches!(alice_approve, Err(DataError::Forbidden)),
        "{alice_approve:?}"
    );
    let approved = store
        .transition_record(
            &bob,
            table.id,
            rec.id,
            "approved",
            submitted.rev,
            &fsm,
            "status",
            None,
        )
        .await
        .expect("bob approves");
    assert_eq!(approved.data["status"], json!("approved"));

    // rev 不一致は 409。
    let stale = store
        .transition_record(
            &bob,
            table.id,
            rec.id,
            "rejected",
            submitted.rev,
            &fsm,
            "status",
            None,
        )
        .await;
    assert!(
        matches!(stale, Err(DataError::RevConflict { current }) if current == approved.rev),
        "{stale:?}"
    );

    // リビジョンに transition が残る（フィールド単位差分・受け入れ条件）。
    let revs = store
        .list_revisions(&alice, table.id, rec.id, None, 10, None)
        .await
        .expect("revs");
    let transitions: Vec<_> = revs
        .iter()
        .filter(|r| r.change_kind == "transition")
        .collect();
    assert_eq!(transitions.len(), 2, "submit と approve の 2 遷移");
    assert!(transitions.iter().any(|r| r
        .patch
        .iter()
        .any(|p| p.field == "status" && p.new == json!("approved"))));

    // outbox に transition イベントが 2 件（副作用でなくイベントのみ・event_type 付き）。
    let count: i64 = sqlx::query(
        "SELECT count(*) FROM storage_event_outbox \
         WHERE tenant_id = $1 AND node_id = $2 AND payload->>'event_type' = $3",
    )
    .bind(&tenant)
    .bind(rec.id)
    .bind(data::TRANSITION_EVENT_TYPE)
    .fetch_one(&pool)
    .await
    .expect("outbox count")
    .get(0);
    assert_eq!(count, 2, "遷移ごとに outbox イベントが 1 件");

    // 監査に transition の allow/deny が残る。
    let audit_count: i64 = sqlx::query(
        "SELECT count(*) FROM audit_log \
         WHERE tenant_id = $1 AND object_id = $2 AND action = 'data.record.transition'",
    )
    .bind(&tenant)
    .bind(rec.id.to_string())
    .fetch_one(&pool)
    .await
    .expect("audit count")
    .get(0);
    assert!(audit_count >= 4, "allow×2 + deny×2 以上");
}

/// テーブルにピンされた FSM は**テーブル側の参照として**読む（検証 C-P1-1）。FSM の作成者でない
/// テーブル editor が、FSM のアーティファクトの閲覧権限なしに遷移できる。テーブルの viewer だけでは
/// 遷移できず（403）、テーブルが見えない主体には存在も分からない（404）。単一の入口
/// （`FsmStore::transition`）は HTTP・ワークフロー・スクリプト・ゲートウェイが共有する。
#[tokio::test]
async fn table_editor_transitions_without_fsm_artifact_access() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice"); // テーブルと FSM の作成者
    let bob = ctx(&tenant, "bob"); // テーブル editor（FSM の閲覧権限なし）
    let carol = ctx(&tenant, "carol"); // テーブル viewer
    let dave = ctx(&tenant, "dave"); // 権限なし
    let store = DataStore::new(pool.clone(), Arc::clone(&authz), Arc::new(AllowRefs));
    let artifacts = Arc::new(artifact::ArtifactStore::new(
        pool.clone(),
        Arc::clone(&authz),
    ));
    let fsms = FsmStore::new(Arc::clone(&artifacts), store.clone());
    let table = store
        .create_table(
            &alice,
            NewDataTable {
                name: "expense".into(),
                schema: expense_schema(),
            },
            None,
        )
        .await
        .unwrap();
    let fsm_id = fsms
        .create(&alice, "approval", table.id, &approval_fsm(), None)
        .await
        .unwrap();
    let mut schema = expense_schema();
    schema.fsm_ref = Some(FsmRef {
        artifact_id: fsm_id,
        version: 1,
    });
    store
        .update_table_schema(&alice, table.id, schema, Some(1), None)
        .await
        .unwrap();
    let obj = alice.ns().data_table(&table.id.to_string());
    authz
        .write_tuple(&bob.subject(), Relation::Editor, &obj)
        .await
        .unwrap();
    authz
        .write_tuple(&carol.subject(), Relation::Viewer, &obj)
        .await
        .unwrap();
    // bob は FSM のアーティファクトを読めない（前提）。
    assert!(fsms.get(&bob, fsm_id, Some(1), None).await.is_err());
    let rec = store
        .create_record(
            &bob,
            table.id,
            json!({"applicant": "bob", "approver": "alice", "status": "draft", "amount": 1}),
            None,
        )
        .await
        .unwrap();
    // テーブル editor は自分の行を遷移できる（actor 述語は当該行で評価）。
    let moved = fsms
        .transition(&bob, table.id, rec.id, "submitted", rec.rev, None)
        .await
        .expect("table editor transitions without FSM artifact access");
    assert_eq!(moved.data["status"], json!("submitted"));
    // 定義外の遷移は構造化 400（fsm_invalid）。
    let bad = fsms
        .transition(&bob, table.id, rec.id, "draft", moved.rev, None)
        .await;
    assert_eq!(
        bad.as_ref().err().and_then(DataError::invalid_code),
        Some(data::InvalidCode::FsmInvalid),
        "{bad:?}"
    );
    // テーブル viewer は遷移できない（403）。
    let r = fsms
        .transition(&carol, table.id, rec.id, "approved", moved.rev, None)
        .await;
    assert!(matches!(r, Err(DataError::Forbidden)), "{r:?}");
    // テーブルが見えない主体には存在も分からない。
    let r = fsms
        .transition(&dave, table.id, rec.id, "approved", moved.rev, None)
        .await;
    assert!(
        matches!(r, Err(DataError::NotFound | DataError::Forbidden)),
        "{r:?}"
    );
    // 承認者 alice（作成者）は FSM の通常経路でも遷移できる。
    let done = fsms
        .transition(&alice, table.id, rec.id, "approved", moved.rev, None)
        .await
        .unwrap();
    assert_eq!(done.data["status"], json!("approved"));

    // 作成時の状態は FSM の初期状態（states の先頭）に固定する（検証 2-C-P2-3）。
    let r = store
        .create_record(
            &bob,
            table.id,
            json!({"applicant": "bob", "approver": "alice", "status": "approved", "amount": 1}),
            None,
        )
        .await;
    assert_eq!(
        r.as_ref().err().and_then(DataError::invalid_code),
        Some(data::InvalidCode::FsmInvalid),
        "{r:?}"
    );
    let fresh = store
        .create_record(
            &bob,
            table.id,
            json!({"applicant": "bob", "approver": "alice", "amount": 2}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(fresh.data["status"], json!("draft"), "省略は初期状態");
    // 一括取込の作成行も同じ（違う状態は行の不合格・省略は初期状態）。
    let imp = store
        .start_bulk_import(
            &bob,
            table.id,
            vec![
                data::BulkImportRow {
                    key: Some("bad".into()),
                    data: json!({"applicant": "bob", "approver": "alice", "status": "approved", "amount": 3}),
                },
                data::BulkImportRow {
                    key: Some("ok".into()),
                    data: json!({"applicant": "bob", "approver": "alice", "amount": 4}),
                },
            ],
            data::BulkImportOptions::default(),
            None,
        )
        .await
        .unwrap();
    store.run_bulk_import(&tenant, table.id, imp).await.unwrap();
    let errs = store
        .bulk_import_errors(&bob, table.id, imp, 0, 10)
        .await
        .unwrap();
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert_eq!(errs[0].key.as_deref(), Some("bad"), "{errs:?}");

    // いま実行できる遷移先（グリッドの状態セルのメニュー・検証 2-C-P2-4）。
    let a = fsms
        .allowed_transitions(&bob, table.id, fresh.id, None)
        .await
        .unwrap();
    assert_eq!(a.current.as_deref(), Some("draft"));
    assert_eq!(
        a.transitions,
        vec!["submitted".to_string()],
        "申請者は提出だけ"
    );
    assert_eq!(a.rev, fresh.rev);
    let a = fsms
        .allowed_transitions(&alice, table.id, fresh.id, None)
        .await
        .unwrap();
    assert!(
        a.transitions.is_empty(),
        "承認者は下書きを動かせない: {a:?}"
    );
    let sub = fsms
        .transition(&bob, table.id, fresh.id, "submitted", fresh.rev, None)
        .await
        .unwrap();
    let a = fsms
        .allowed_transitions(&alice, table.id, sub.id, None)
        .await
        .unwrap();
    assert_eq!(
        a.transitions,
        vec!["approved".to_string(), "rejected".to_string()]
    );
    // viewer は候補も引けない（遷移と同じ 403）・見えない主体には存在も分からない。
    let r = fsms
        .allowed_transitions(&carol, table.id, sub.id, None)
        .await;
    assert!(matches!(r, Err(DataError::Forbidden)), "{r:?}");
    let r = fsms
        .allowed_transitions(&dave, table.id, sub.id, None)
        .await;
    assert!(
        matches!(r, Err(DataError::NotFound | DataError::Forbidden)),
        "{r:?}"
    );
}

/// このファイルの後ろのテストが作る OpenFGA のストア（テストの終わりに消す）。
struct TestStore {
    base_url: String,
    store_id: String,
}

impl TestStore {
    async fn delete(&self) {
        let r = reqwest::Client::new()
            .delete(format!("{}/stores/{}", self.base_url, self.store_id))
            .send()
            .await
            .expect("OpenFGA delete store");
        assert!(r.status().is_success(), "delete store: {}", r.status());
    }
}

/// [`setup`] と同じ環境に、後で消すストアの情報を添える。
async fn setup_cleaned() -> Option<(PgPool, Arc<dyn AuthzClient>, TestStore)> {
    let db_url = shiki_testenv::var("STORAGE_TEST_DATABASE_URL").ok()?;
    let base_url = shiki_testenv::var("OPENFGA_TEST_URL").ok()?;
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
        base_url: base_url.clone(),
        store_name: format!("shiki-data-fsm-test-{}", Uuid::new_v4()),
    };
    let fga = OpenFgaClient::connect(reqwest::Client::new(), &config, &model)
        .await
        .expect("fga");
    let store = TestStore {
        base_url,
        store_id: fga.store_id().to_string(),
    };
    Some((pool, Arc::new(fga), store))
}

/// テナントのテーブル・レコード・状態の進め方の定義を消す。
async fn purge(pool: &PgPool, store: &DataStore, tenant: &str) {
    store.purge_tenant(tenant).await.unwrap();
    sqlx::query("DELETE FROM artifact WHERE tenant_id = $1")
        .bind(tenant)
        .execute(pool)
        .await
        .unwrap();
}

/// 申請の表に、承認の進め方（submitted → approved）を付けて指す。bob に編集者を渡す。
async fn pinned_table(
    pool: &PgPool,
    authz: &Arc<dyn AuthzClient>,
    alice: &AuthContext,
    bob: &AuthContext,
) -> (DataStore, FsmStore, Uuid, Uuid) {
    let store = DataStore::new(pool.clone(), Arc::clone(authz), Arc::new(AllowRefs));
    let artifacts = Arc::new(artifact::ArtifactStore::new(
        pool.clone(),
        Arc::clone(authz),
    ));
    let fsms = FsmStore::new(Arc::clone(&artifacts), store.clone());
    let table = store
        .create_table(
            alice,
            NewDataTable {
                name: "expense".into(),
                schema: expense_schema(),
            },
            None,
        )
        .await
        .unwrap();
    let fsm_id = fsms
        .create(alice, "approval", table.id, &approval_fsm(), None)
        .await
        .unwrap();
    let mut schema = expense_schema();
    schema.fsm_ref = Some(FsmRef {
        artifact_id: fsm_id,
        version: 1,
    });
    store
        .update_table_schema(alice, table.id, schema, Some(1), None)
        .await
        .unwrap();
    authz
        .write_tuple(
            &bob.subject(),
            Relation::Editor,
            &alice.ns().data_table(&table.id.to_string()),
        )
        .await
        .unwrap();
    (store, fsms, table.id, fsm_id)
}

/// BUG-153: 前から保存されていた定義に同じ組の進め方が 2 つあると、遷移の候補は 2 つ目の進められる人にも出すのに、
/// 遷移は 1 つ目しか見ないので進めなかった。遷移の判定を候補と同じ（どれか 1 つに当てはまれば進める）にする。
#[tokio::test]
async fn duplicate_pairs_in_a_legacy_definition_move_like_the_candidates() {
    let Some((pool, authz, fga)) = setup_cleaned().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let (store, fsms, table, fsm_id) = pinned_table(&pool, &authz, &alice, &bob).await;
    // 前から保存されていた形: submitted → approved が 2 つ（1 つ目は申請者・2 つ目は承認者）。新しい保存では断るので、
    // 保存済みの版の本文を書き換えて作る。
    let mut legacy = approval_fsm();
    legacy.transitions[1].actor = PolicyExpr::FieldCmp {
        field: "applicant".into(),
        op: CmpOp::Eq,
        value: PolicyOperand::UserId,
    };
    legacy.transitions.push(FsmTransition {
        from: "submitted".into(),
        to: "approved".into(),
        actor: PolicyExpr::FieldCmp {
            field: "approver".into(),
            op: CmpOp::Eq,
            value: PolicyOperand::UserId,
        },
    });
    sqlx::query(
        "UPDATE artifact_version SET body = $3 WHERE tenant_id = $1 AND artifact_id = $2 AND version = 1",
    )
    .bind(&tenant)
    .bind(fsm_id)
    .bind(serde_json::to_value(&legacy).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    let rec = store
        .create_record(
            &alice,
            table,
            json!({"applicant": "alice", "approver": "bob", "amount": 1}),
            None,
        )
        .await
        .unwrap();
    let sub = fsms
        .transition(&alice, table, rec.id, "submitted", rec.rev, None)
        .await
        .unwrap();
    // 承認者 bob は 1 つ目（申請者）には当てはまらず、2 つ目（承認者）に当てはまる: 候補に出て、実際に進める。
    let a = fsms
        .allowed_transitions(&bob, table, sub.id, None)
        .await
        .unwrap();
    assert!(a.transitions.contains(&"approved".to_string()), "{a:?}");
    let moved = fsms
        .transition(&bob, table, sub.id, "approved", sub.rev, None)
        .await;
    // どちらの進められる人にも当てはまらない編集者（dave）には、候補に出ず、進めない。
    let dave = ctx(&tenant, "dave");
    authz
        .write_tuple(
            &dave.subject(),
            Relation::Editor,
            &alice.ns().data_table(&table.to_string()),
        )
        .await
        .unwrap();
    let rec2 = store
        .create_record(
            &alice,
            table,
            json!({"applicant": "alice", "approver": "bob", "amount": 2}),
            None,
        )
        .await
        .unwrap();
    let sub2 = fsms
        .transition(&alice, table, rec2.id, "submitted", rec2.rev, None)
        .await
        .unwrap();
    let a2 = fsms
        .allowed_transitions(&dave, table, sub2.id, None)
        .await
        .unwrap();
    let denied = fsms
        .transition(&dave, table, sub2.id, "approved", sub2.rev, None)
        .await;
    purge(&pool, &store, &tenant).await;
    fga.delete().await;

    let moved = moved.expect("2 つ目の進め方の進められる人は進める");
    assert_eq!(moved.data["status"], json!("approved"));
    assert!(!a2.transitions.contains(&"approved".to_string()), "{a2:?}");
    assert!(matches!(denied, Err(DataError::Forbidden)), "{denied:?}");
}

/// BUG-155: 「追加だけ」の人が状態を進めようとすると、テーブルが無いのと同じ 404 だった。書き換え・削除・取込と
/// 同じ 403 にそろえる。レコードを引く前に断るので、無いレコードでも同じ 403（レコードの有無は分からない）。
/// テーブルが見えない人には、これまでどおり 404。
#[tokio::test]
async fn add_only_people_get_the_same_403_as_other_writes_when_moving_states() {
    let Some((pool, authz, fga)) = setup_cleaned().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let carol = ctx(&tenant, "carol"); // 追加だけ
    let dave = ctx(&tenant, "dave"); // 権限なし
    let (store, fsms, table, _) = pinned_table(&pool, &authz, &alice, &bob).await;
    store
        .share_table(
            &alice,
            table,
            &storage::ShareTarget::User { id: "carol".into() },
            data::TableShareRole::Submitter,
            None,
        )
        .await
        .unwrap();
    let own = store
        .create_record(
            &carol,
            table,
            json!({"applicant": "carol", "approver": "bob", "amount": 1}),
            None,
        )
        .await
        .unwrap();
    let on_own = fsms
        .transition(&carol, table, own.id, "submitted", own.rev, None)
        .await;
    let on_missing = fsms
        .transition(&carol, table, Uuid::new_v4(), "submitted", 1, None)
        .await;
    let candidates = fsms.allowed_transitions(&carol, table, own.id, None).await;
    let update = store
        .update_record(&carol, table, own.id, json!({"amount": 2}), own.rev, None)
        .await;
    let hidden = fsms
        .transition(&dave, table, own.id, "submitted", own.rev, None)
        .await;
    purge(&pool, &store, &tenant).await;
    fga.delete().await;

    assert!(matches!(update, Err(DataError::Forbidden)), "{update:?}");
    assert!(matches!(on_own, Err(DataError::Forbidden)), "{on_own:?}");
    assert!(
        matches!(on_missing, Err(DataError::Forbidden)),
        "{on_missing:?}"
    );
    assert!(
        matches!(candidates, Err(DataError::Forbidden)),
        "{candidates:?}"
    );
    assert!(matches!(hidden, Err(DataError::NotFound)), "{hidden:?}");
}

/// BUG-154: レコードの追加のダイアログが出す最初の状態は、閲覧者の文脈から読む。定義はテーブル側の参照として
/// 読むので、定義を見る権限の無い編集者・追加だけの人にも出る。状態の列が読めない人には出さない。
#[tokio::test]
async fn viewer_context_tells_the_initial_state_without_access_to_the_definition() {
    let Some((pool, authz, fga)) = setup_cleaned().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let carol = ctx(&tenant, "carol");
    let (store, fsms, table, fsm_id) = pinned_table(&pool, &authz, &alice, &bob).await;
    store
        .share_table(
            &alice,
            table,
            &storage::ShareTarget::User { id: "carol".into() },
            data::TableShareRole::Submitter,
            None,
        )
        .await
        .unwrap();
    // 前提: bob と carol は定義そのものを読めない。
    let bob_reads = fsms.get(&bob, fsm_id, Some(1), None).await;
    let for_bob = store.table_viewer_context(&bob, table, None).await.unwrap();
    let for_carol = store
        .table_viewer_context(&carol, table, None)
        .await
        .unwrap();
    // 定義を読めないとき（ここでは定義を読む口の無い構成）も、閲覧者の文脈は返す（最初の状態は補助の表示なので
    // 出さないだけ・テーブルの画面を開けなくしない）。
    let bare = DataStore::new(pool.clone(), Arc::clone(&authz), Arc::new(AllowRefs));
    let degraded = bare.table_viewer_context(&bob, table, None).await;
    // 状態の列を経理だけに見せると、ほかの人には出さない。
    let current = store.get_table(&alice, table, None).await.unwrap();
    let mut masked = current.schema.clone();
    masked.field_policy = vec![data::FieldPolicy {
        field: "status".into(),
        readable_by: PolicyExpr::HasRole {
            role: "keiri".into(),
            subtree: true,
        },
    }];
    store
        .update_table_schema(&alice, table, masked, Some(current.schema_version), None)
        .await
        .unwrap();
    let after_mask = store.table_viewer_context(&bob, table, None).await.unwrap();
    // 状態の進め方の無いテーブルには出さない。
    let plain = store
        .create_table(
            &alice,
            NewDataTable {
                name: "plain".into(),
                schema: TableSchema {
                    status_field: None,
                    ..expense_schema()
                },
            },
            None,
        )
        .await
        .unwrap();
    let no_fsm = store
        .table_viewer_context(&alice, plain.id, None)
        .await
        .unwrap();
    purge(&pool, &store, &tenant).await;
    fga.delete().await;

    assert!(bob_reads.is_err(), "{bob_reads:?}");
    assert_eq!(for_bob.initial_state.as_deref(), Some("draft"));
    assert_eq!(for_carol.initial_state.as_deref(), Some("draft"));
    assert!(for_carol.add_only);
    assert_eq!(after_mask.initial_state, None);
    assert_eq!(no_fsm.initial_state, None);
    let degraded = degraded.expect("定義を読めなくても閲覧者の文脈は返す");
    assert_eq!(degraded.initial_state, None);
    assert!(degraded.can_edit);
}

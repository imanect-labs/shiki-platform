//! FSM 宣言的ガードの結合テスト（Task 9.10 受け入れ条件）。
//!
//! 承認フロー（draft→submitted→approved/rejected）が定義どおり遷移し、定義外遷移・無権限
//! 遷移が拒否され、全遷移がリビジョン＋監査＋outbox に残ることを実 Postgres＋実 OpenFGA で検証。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
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
    let db_url = std::env::var("STORAGE_TEST_DATABASE_URL").ok()?;
    let base_url = std::env::var("OPENFGA_TEST_URL").ok()?;
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
    assert!(matches!(direct, Err(DataError::Invalid(_))), "{direct:?}");

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
    assert!(matches!(bad, Err(DataError::Invalid(_))), "{bad:?}");

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
    assert!(matches!(stale, Err(DataError::Conflict(_))), "{stale:?}");

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

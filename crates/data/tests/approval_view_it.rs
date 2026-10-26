//! 承認待ち（ワークフローの human.approval・GAP-08）が使う読取口の結合テスト（実 Postgres ＋実 OpenFGA）。
//!
//! - `FsmStore::approval_target`: いまの状態・rev・FSM でその状態から進める状態（遷移の定義だけ）と、読める
//!   ユーザー／ロールの列の値。マスクされた担当者の列は入らない。行述語で見えない行は 404、テーブルの viewer で
//!   ない主体は 404、状態の列が読めなければ field_unavailable
//! - `DataStore::get_records`: `ids` と同じ順で、見えない・無い行は `None`。マスク列は落とす
//! - `DataStore::filter_visible_records`: 同じ判定で、テーブルが見えなければ `None`（Deny の監査を残さない）

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
    CmpOp, DataError, DataStore, FieldDef, FieldPolicy, FieldType, FsmBody, FsmRef, FsmStore,
    FsmTransition, NewDataTable, PolicyExpr, PolicyOperand, RowPolicy, TableSchema,
};
use serde_json::json;
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

/// 申請の表: 行は申請者（owner）と承認者が読める。代理（deputy）とメモは経理だけが読める。
fn schema(fsm: Option<FsmRef>) -> TableSchema {
    let mut status = FieldDef::new("status", FieldType::Select);
    status.options = ["draft", "submitted", "approved", "rejected"]
        .map(String::from)
        .to_vec();
    status.indexed = true;
    let mut approver = FieldDef::new("approver", FieldType::UserRef);
    approver.indexed = true;
    let keiri_only = PolicyExpr::HasRole {
        role: "keiri".into(),
        subtree: true,
    };
    TableSchema {
        fields: vec![
            FieldDef::new("title", FieldType::Text),
            FieldDef::new("memo", FieldType::Text),
            approver,
            FieldDef::new("deputy", FieldType::UserRef),
            FieldDef::new("dept", FieldType::RoleRef),
            status,
        ],
        status_field: Some("status".into()),
        row_policy: Some(RowPolicy {
            read: PolicyExpr::Any(vec![
                PolicyExpr::IsOwner,
                PolicyExpr::FieldCmp {
                    field: "approver".into(),
                    op: CmpOp::Eq,
                    value: PolicyOperand::UserId,
                },
            ]),
            write: None,
        }),
        field_policy: vec![
            FieldPolicy {
                field: "memo".into(),
                readable_by: keiri_only.clone(),
            },
            FieldPolicy {
                field: "deputy".into(),
                readable_by: keiri_only,
            },
        ],
        aggregate_min_rows: None,
        fsm_ref: fsm,
        rag: None,
    }
}

#[tokio::test]
async fn approval_reads_follow_row_and_field_visibility() {
    let (Ok(db_url), Ok(base_url)) = (
        std::env::var("STORAGE_TEST_DATABASE_URL"),
        std::env::var("OPENFGA_TEST_URL"),
    ) else {
        eprintln!("STORAGE_TEST_DATABASE_URL / OPENFGA_TEST_URL 未設定のためスキップ");
        return;
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
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
    let authz: Arc<dyn AuthzClient> = Arc::new(
        OpenFgaClient::connect(
            reqwest::Client::new(),
            &OpenFgaConfig {
                base_url,
                store_name: format!("shiki-data-approval-view-{}", Uuid::new_v4()),
            },
            &model,
        )
        .await
        .expect("fga"),
    );
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice"); // 申請者（経理）
    let bob = ctx(&tenant, "bob"); // 承認者（経理でない）
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
                schema: schema(None),
            },
            None,
        )
        .await
        .unwrap();
    let t = |from: &str, to: &str| FsmTransition {
        from: from.into(),
        to: to.into(),
        actor: PolicyExpr::IsOwner,
    };
    let body = FsmBody {
        states: ["draft", "submitted", "approved", "rejected"]
            .map(String::from)
            .to_vec(),
        transitions: vec![
            t("draft", "submitted"),
            t("submitted", "approved"),
            t("submitted", "rejected"),
            t("submitted", "approved"),
        ],
    };
    let fsm_id = fsms
        .create(&alice, "flow", table.id, &body, None)
        .await
        .unwrap();
    store
        .update_table_schema(
            &alice,
            table.id,
            schema(Some(FsmRef {
                artifact_id: fsm_id,
                version: 1,
            })),
            Some(1),
            None,
        )
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
    let r = store
        .create_record(
            &alice,
            table.id,
            json!({ "title": "出張旅費", "memo": "社外秘", "approver": "bob", "deputy": "erin",
                    "dept": "sales", "status": "draft" }),
            None,
        )
        .await
        .unwrap();
    let r = fsms
        .transition(&alice, table.id, r.id, "submitted", r.rev, None)
        .await
        .unwrap();
    let other = store
        .create_record(
            &alice,
            table.id,
            json!({ "title": "私用", "approver": "alice", "status": "draft" }),
            None,
        )
        .await
        .unwrap();

    // 承認者（bob）: 状態・rev・進める状態（定義順・重複なし）。読めない代理の列は入らない。
    let v = fsms
        .approval_target(&bob, table.id, r.id, None)
        .await
        .unwrap();
    assert_eq!(v.status_field, "status");
    assert_eq!(v.current.as_deref(), Some("submitted"));
    assert_eq!(v.rev, r.rev);
    assert_eq!(v.targets, ["approved", "rejected"]);
    assert_eq!(
        v.refs.get("approver"),
        Some(&(false, Some("bob".to_string())))
    );
    assert_eq!(v.refs.get("dept"), Some(&(true, Some("sales".to_string()))));
    assert!(!v.refs.contains_key("deputy"), "マスクした列: {:?}", v.refs);
    // 経理（alice）には代理の列も読める。
    let v = fsms
        .approval_target(&alice, table.id, r.id, None)
        .await
        .unwrap();
    assert_eq!(
        v.refs.get("deputy"),
        Some(&(false, Some("erin".to_string())))
    );
    // 行が見えない・テーブルの viewer でない主体は 404。
    for who in [&carol, &dave] {
        let e = fsms
            .approval_target(who, table.id, r.id, None)
            .await
            .unwrap_err();
        assert!(matches!(e, DataError::NotFound), "{e:?}");
    }

    // まとめ取り: ids と同じ順・見えない行と無い行は None・マスク列は落とす。
    let missing = Uuid::new_v4();
    let got = store
        .get_records(&bob, table.id, &[other.id, r.id, missing], None)
        .await
        .unwrap();
    assert_eq!(got.len(), 3);
    assert!(got[0].is_none(), "bob に見えない行");
    let mine = got[1].as_ref().unwrap();
    assert_eq!(mine.id, r.id);
    assert_eq!(mine.data["title"], "出張旅費");
    assert!(mine.data.get("memo").is_none() && mine.data.get("deputy").is_none());
    assert!(got[2].is_none());
    let got = store
        .get_records(&alice, table.id, &[r.id, other.id], None)
        .await
        .unwrap();
    assert_eq!(got[0].as_ref().unwrap().data["memo"], "社外秘");
    assert_eq!(got[1].as_ref().unwrap().id, other.id);
    let e = store
        .get_records(&dave, table.id, &[r.id], None)
        .await
        .unwrap_err();
    assert!(matches!(e, DataError::NotFound), "{e:?}");
    assert!(store
        .get_records(&bob, table.id, &[], None)
        .await
        .unwrap()
        .is_empty());
    // 同じ id を重ねても、それぞれの位置に同じ行が入る（1 本のクエリで引いて id で並べ直す）。
    let got = store
        .get_records(&bob, table.id, &[r.id, missing, r.id], None)
        .await
        .unwrap();
    assert_eq!(
        got.iter()
            .map(|g| g.as_ref().map(|x| x.id))
            .collect::<Vec<_>>(),
        [Some(r.id), None, Some(r.id)]
    );

    // 候補の絞り込み（受信箱）: 判定は get_records と同じ（見えない行・無い行は None・マスク列は落とす）で、
    // 開いたテーブルの定義も返す。テーブルが見えなければ誤りではなく None で、Deny の監査を残さない（BUG-43）。
    let filtered = store
        .filter_visible_records(&bob, table.id, &[other.id, r.id, missing])
        .await
        .unwrap()
        .expect("bob にはテーブルが見える");
    assert_eq!(filtered.table.id, table.id);
    assert_eq!(
        filtered.table.schema.status_field.as_deref(),
        Some("status")
    );
    assert!(filtered.records[0].is_none() && filtered.records[2].is_none());
    let mine = filtered.records[1].as_ref().unwrap();
    assert_eq!(mine.data["title"], "出張旅費");
    assert!(mine.data.get("memo").is_none() && mine.data.get("deputy").is_none());
    let denials = |who: String| {
        let pool = pool.clone();
        let tenant = tenant.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND actor = $2 \
                   AND decision = 'deny'",
            )
            .bind(tenant)
            .bind(who)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let before = denials(dave.principal.id.clone()).await;
    for _ in 0..3 {
        assert!(store
            .filter_visible_records(&dave, table.id, &[r.id])
            .await
            .unwrap()
            .is_none());
        assert!(store
            .filter_visible_records(&bob, Uuid::new_v4(), &[r.id])
            .await
            .unwrap()
            .is_none());
    }
    assert_eq!(denials(dave.principal.id.clone()).await, before);
    // 本人が求めた読取（get_records）は従来どおり Deny を残す。
    let _ = store.get_records(&dave, table.id, &[r.id], None).await;
    assert_eq!(denials(dave.principal.id.clone()).await, before + 1);
}

//! SSE ファンアウトの計測（Phase 15 Task 15.6 受け入れ条件・data-platform §9.4・PIT-70 ⑤）:
//! **`IsOwner` ＋ `$user.id` 比較 ＋ 個別共有を含む行ポリシーで、購読者 1,000 人・ロール 3 種のとき
//! 述語評価（ロール共通部の SQL）が 3 回**であること。
//!
//! 素朴に「畳み込み後の SQL ＋ バインドのハッシュ」で束ねると principal id・共有集合がバインドに入り
//! 1,000 グループになる（削減が効かない）。ここでは評価回数そのものを数え、加えて各購読者が受けた
//! イベントが行述語の答えと一致することも確かめる（グループ化で正しさを失っていない）。
//!
//! authz は決定的な疑似実装（ロール・共有の材料を返す）。Postgres は実物（`STORAGE_TEST_DATABASE_URL`）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stderr,
    clippy::too_many_lines
)]

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use data::{
    CmpOp, DataStore, FieldDef, FieldType, NewDataTable, PolicyExpr, PolicyOperand, RefResolver,
    RowPolicy, StreamConfig, StreamEvent, TableSchema,
};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

const USERS: usize = 1_000;
const ROLES: usize = 3;

/// 主体 → (ロール, 共有されたレコードの object 文字列)。
#[derive(Default)]
struct FakeAuthz {
    roles: HashMap<String, String>,
    shares: Mutex<HashMap<String, Vec<String>>>,
}

#[async_trait]
impl AuthzClient for FakeAuthz {
    async fn check(
        &self,
        s: &Subject,
        r: Relation,
        o: &FgaObject,
        _c: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(match r {
            Relation::Member => self.roles.get(s.as_str()).is_some_and(|x| x == o.as_str()),
            _ => true, // テーブル viewer 等（第 1 層は全員通す＝行述語だけが差を作る）
        })
    }
    async fn write_tuple(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn delete_tuple(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn read_tuples(
        &self,
        _: &FgaObject,
        _: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        Ok(vec![])
    }
    async fn list_objects(
        &self,
        s: &Subject,
        r: Relation,
        t: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(match (r, t) {
            (Relation::Member, ObjectType::Role) => {
                self.roles.get(s.as_str()).cloned().into_iter().collect()
            }
            (Relation::Viewer, ObjectType::DataRecord) => self
                .shares
                .lock()
                .unwrap()
                .get(s.as_str())
                .cloned()
                .unwrap_or_default(),
            _ => vec![],
        })
    }
    async fn delete_object_tuples(&self, _: &FgaObject) -> Result<u32, AuthzError> {
        Ok(0)
    }
    async fn read_subject_objects(
        &self,
        s: &Subject,
        t: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(match t {
            ObjectType::Role => self.roles.get(s.as_str()).cloned().into_iter().collect(),
            _ => vec![],
        })
    }
}

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

#[tokio::test]
async fn thousand_subscribers_three_roles_evaluate_the_predicate_three_times() {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return;
    };
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&db_url)
        .await
        .unwrap();
    sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
    let tenant = format!("t-{}", Uuid::new_v4());
    let users: Vec<AuthContext> = (0..USERS).map(|i| ctx(&tenant, &format!("u{i}"))).collect();
    let writer = ctx(&tenant, "writer");
    let mut fake = FakeAuthz::default();
    for (i, u) in users.iter().enumerate() {
        fake.roles.insert(
            u.subject().as_str().to_string(),
            u.ns().role(&format!("r{}", i % ROLES)).as_str().to_string(),
        );
    }
    let fake = Arc::new(fake);
    let store = DataStore::new(pool.clone(), fake.clone(), Arc::new(AllowRefs)).with_stream_config(
        StreamConfig {
            manual: true,
            authz_ttl: Duration::from_hours(1),
            ..StreamConfig::default()
        },
    );
    // IsOwner ∨ applicant = $user ∨ dept ∈ $user.roles（＋個別共有）。
    let mut applicant = FieldDef::new("applicant", FieldType::UserRef);
    applicant.indexed = true;
    let mut dept = FieldDef::new("dept", FieldType::RoleRef);
    dept.indexed = true;
    let table = store
        .create_table(
            &writer,
            NewDataTable {
                name: "fanout".into(),
                schema: TableSchema {
                    fields: vec![applicant, dept, FieldDef::new("title", FieldType::Text)],
                    status_field: None,
                    row_policy: Some(RowPolicy {
                        read: PolicyExpr::Any(vec![
                            PolicyExpr::IsOwner,
                            PolicyExpr::FieldCmp {
                                field: "applicant".into(),
                                op: CmpOp::Eq,
                                value: PolicyOperand::UserId,
                            },
                            PolicyExpr::FieldCmp {
                                field: "dept".into(),
                                op: CmpOp::In,
                                value: PolicyOperand::UserRoles { subtree: false },
                            },
                        ]),
                        write: None,
                    }),
                    field_policy: vec![],
                    aggregate_min_rows: None,
                    fsm_ref: None,
                },
            },
            None,
        )
        .await
        .unwrap();
    // r1 の部署・申請者 u7・u500 へ個別共有。見えるのは r1 の 333 人＋u7＋u500。
    let rec = store
        .create_record(
            &writer,
            table.id,
            json!({"applicant": "u7", "dept": "r1", "title": "fanout"}),
            None,
        )
        .await
        .unwrap();
    fake.shares.lock().unwrap().insert(
        users[500].subject().as_str().to_string(),
        vec![users[500]
            .ns()
            .data_record(&rec.id.to_string())
            .as_str()
            .to_string()],
    );
    let t0 = Instant::now();
    let mut streams = Vec::with_capacity(USERS);
    for u in &users {
        // 採番前に繋ぐ（位置 0 から＝この作成を受ける）。
        streams.push(
            store
                .subscribe_records(u, table.id, Some(0), None)
                .await
                .unwrap(),
        );
    }
    eprintln!("購読 {USERS} 件の開始: {:?}", t0.elapsed());
    // 共有 DB の他テストの outbox が溜まっていても、この作成が採番されるまでリレーを回し切る。
    while store.relay_record_changes_once().await.unwrap() > 0 {}
    let before = store.stream_predicate_evaluations();
    let t1 = Instant::now();
    store.stream_tick().await.unwrap();
    let tick = t1.elapsed();
    let evaluations = store.stream_predicate_evaluations() - before;
    eprintln!("購読者 {USERS} 人・ロール {ROLES} 種: 述語評価 {evaluations} 回・1 バッチ {tick:?}");
    assert_eq!(
        evaluations, ROLES as u64,
        "ロール共通部はグループごとに 1 回"
    );
    // 正しさ: 受けた購読者の集合が行述語の答えと一致する。
    let mut got: HashSet<usize> = HashSet::new();
    for (i, s) in streams.iter_mut().enumerate() {
        while let Ok(Some(m)) = tokio::time::timeout(Duration::from_millis(5), s.next()).await {
            if let StreamEvent::Upserted(u) = &m.event {
                assert_eq!(u.record.id, rec.id);
                got.insert(i);
            }
        }
    }
    let want: HashSet<usize> = (0..USERS)
        .filter(|i| i % ROLES == 1 || *i == 7 || *i == 500)
        .collect();
    assert_eq!(got, want);
    assert_eq!(store.stream_verify_drops(), 0, "事後検査と一致");
}

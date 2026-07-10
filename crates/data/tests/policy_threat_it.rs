//! 行レベル認可の脅威モデルテスト行列（Task 9.3 受け入れ条件・PIT-17〜21）。
//!
//! 「WHERE 強制注入は必要条件にすぎない」（PIT-21）を受け、漏れ口ごとに対策を検証する:
//! ① 不可視行の 404 形状一致（存在オラクルなし） ② クライアントフィルタでの述語バイパス不能
//! ③ 集計（count）からの不可視行除外 ④ lookup 越しの釣り出し失敗（PIT-20）
//! ⑤ 共有上限フォールバックが可視減方向（PIT-18） ⑥ rev オラクル封じ ⑦ 共有剥奪の即時反映。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）＋実 OpenFGA（`OPENFGA_TEST_URL`）で実行する。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use async_trait::async_trait;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use data::{
    CmpOp, DataError, DataStore, FieldDef, FieldType, ListRecordsOptions, NewDataTable, PolicyExpr,
    PolicyOperand, RecordFilter, RecordShareRole, RefResolver, RowPolicy, TableSchema,
};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
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

async fn setup() -> Option<(PgPool, Arc<dyn AuthzClient>)> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let Ok(base_url) = std::env::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
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
    let config = OpenFgaConfig {
        base_url,
        store_name: format!("shiki-data-policy-test-{}", Uuid::new_v4()),
    };
    let fga = OpenFgaClient::connect(reqwest::Client::new(), &config, &model)
        .await
        .expect("OpenFGA 接続");
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

/// 申請者本人 or 経理ロール（subtree）だけが読める経費テーブル。
fn expense_schema_with_policy() -> TableSchema {
    let applicant = field("applicant", FieldType::UserRef);
    let mut amount = field("amount", FieldType::Number);
    amount.indexed = true;
    let mut status = field("status", FieldType::Select);
    status.options = vec!["draft".into(), "submitted".into()];
    status.indexed = true;
    TableSchema {
        fields: vec![applicant, amount, status, field("memo", FieldType::Text)],
        status_field: None,
        row_policy: Some(RowPolicy {
            read: PolicyExpr::Any(vec![
                PolicyExpr::FieldCmp {
                    field: "applicant".into(),
                    op: CmpOp::Eq,
                    value: PolicyOperand::UserId,
                },
                PolicyExpr::HasRole {
                    role: "keiri".into(),
                    subtree: true,
                },
            ]),
            write: Some(PolicyExpr::FieldCmp {
                field: "applicant".into(),
                op: CmpOp::Eq,
                value: PolicyOperand::UserId,
            }),
        }),
        field_policy: vec![],
        aggregate_min_rows: None,
        fsm_ref: None,
    }
}

/// 共通フィクスチャ: alice がテーブルを作り bob に viewer/editor 共有。
/// alice の行と bob の行を 1 件ずつ入れる。
struct Fixture {
    store: DataStore,
    alice: AuthContext,
    bob: AuthContext,
    table_id: Uuid,
    alice_rec: Uuid,
    bob_rec: Uuid,
}

async fn fixture(pool: PgPool, authz: Arc<dyn AuthzClient>) -> Fixture {
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let store = DataStore::new(pool, Arc::clone(&authz), Arc::new(AllowRefs));
    let table = store
        .create_table(
            &alice,
            NewDataTable {
                name: "expense".into(),
                schema: expense_schema_with_policy(),
            },
            None,
        )
        .await
        .expect("table");
    // bob はテーブル editor（第1層は通る＝第2層の行述語だけが防波堤になる状況を作る）。
    authz
        .write_tuple(
            &bob.subject(),
            Relation::Editor,
            &alice.ns().data_table(&table.id.to_string()),
        )
        .await
        .expect("share table to bob");
    let alice_rec = store
        .create_record(
            &alice,
            table.id,
            json!({"applicant": "alice", "amount": 100, "status": "draft", "memo": "機密"}),
            None,
        )
        .await
        .expect("alice rec");
    let bob_rec = store
        .create_record(
            &bob,
            table.id,
            json!({"applicant": "bob", "amount": 200, "status": "draft"}),
            None,
        )
        .await
        .expect("bob rec");
    Fixture {
        store,
        alice,
        bob,
        table_id: table.id,
        alice_rec: alice_rec.id,
        bob_rec: bob_rec.id,
    }
}

/// ① 不可視行の get は「存在しない」と完全同形（404 オラクルなし）。
/// ⑥ 不可視行への update/delete も rev の値に依らず 404（rev オラクル封じ）。
#[tokio::test]
async fn invisible_row_indistinguishable_from_missing() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let f = fixture(pool, authz).await;

    // bob は自分の行だけ読める。
    f.store
        .get_record(&f.bob, f.table_id, f.bob_rec, None)
        .await
        .expect("bob reads own");
    let invisible = f
        .store
        .get_record(&f.bob, f.table_id, f.alice_rec, None)
        .await;
    let missing = f
        .store
        .get_record(&f.bob, f.table_id, Uuid::new_v4(), None)
        .await;
    // 変種の一致だけでなく **Debug 表現の完全一致**をアサート（メッセージ差分によるオラクルも防ぐ）。
    assert_eq!(
        format!("{invisible:?}"),
        format!("{missing:?}"),
        "不可視と不存在は完全同形の応答であること"
    );

    // ⑥ update: 正しい rev / 誤った rev のどちらでも 404（409 を返すと rev が漏れる）。
    for rev in [1, 99] {
        let r = f
            .store
            .update_record(
                &f.bob,
                f.table_id,
                f.alice_rec,
                json!({"amount": 1}),
                rev,
                None,
            )
            .await;
        assert!(matches!(r, Err(DataError::NotFound)), "rev={rev}: {r:?}");
    }
    let r = f
        .store
        .delete_record(&f.bob, f.table_id, f.alice_rec, 1, None)
        .await;
    assert!(matches!(r, Err(DataError::NotFound)), "{r:?}");

    // 可視だが write 述語外（bob は applicant=bob の行だけ書ける）: alice が bob の行を
    // 読めるロール（経理）でも書けない、の対称は roles テストで検証する。
}

/// ② クライアント指定フィルタで述語をバイパスできない（何を渡しても AND 合成のまま）。
#[tokio::test]
async fn client_filters_cannot_bypass_predicate() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let f = fixture(pool, authz).await;

    // 未宣言フィールド・未索引フィールドは実行前に 400（実行時キャストエラーのリークなし）。
    for bad_field in ["nope", "memo"] {
        let r = f
            .store
            .list_records(
                &f.bob,
                f.table_id,
                &ListRecordsOptions {
                    filter: Some(RecordFilter {
                        field: bad_field.into(),
                        value: json!("x"),
                    }),
                    ..Default::default()
                },
                None,
            )
            .await;
        assert!(
            matches!(r, Err(DataError::Invalid(_))),
            "{bad_field}: {r:?}"
        );
    }
    // 型不一致（number フィールドに文字列）も実行前 400。
    let r = f
        .store
        .list_records(
            &f.bob,
            f.table_id,
            &ListRecordsOptions {
                filter: Some(RecordFilter {
                    field: "amount".into(),
                    value: json!("' OR 1=1 --"),
                }),
                ..Default::default()
            },
            None,
        )
        .await;
    assert!(matches!(r, Err(DataError::Invalid(_))), "{r:?}");

    // SQL 風の値はリテラルとして扱われる（注入不能・0 件になるだけ）。
    let page = f
        .store
        .list_records(
            &f.bob,
            f.table_id,
            &ListRecordsOptions {
                filter: Some(RecordFilter {
                    field: "status".into(),
                    value: json!("draft' OR '1'='1"),
                }),
                ..Default::default()
            },
            None,
        )
        .await
        .expect("literal filter");
    assert!(page.items.is_empty());

    // 一致するフィルタでも自分の可視行しか返らない。
    let page = f
        .store
        .list_records(
            &f.bob,
            f.table_id,
            &ListRecordsOptions {
                filter: Some(RecordFilter {
                    field: "status".into(),
                    value: json!("draft"),
                }),
                ..Default::default()
            },
            None,
        )
        .await
        .expect("filtered");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].id, f.bob_rec);
}

/// ③ 集計（count）にも述語が適用され、不可視行が件数から漏れない（PIT-17 の基礎保証）。
#[tokio::test]
async fn aggregates_exclude_invisible_rows() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let f = fixture(pool, authz).await;
    let bob_count = f
        .store
        .count_records(&f.bob, f.table_id, None, None)
        .await
        .expect("bob count");
    assert_eq!(bob_count, 1, "bob には自分の 1 件だけ");
    let alice_count = f
        .store
        .count_records(&f.alice, f.table_id, None, None)
        .await
        .expect("alice count");
    assert_eq!(
        alice_count, 1,
        "alice にも自分の 1 件だけ（作成者でも述語は素通りしない）"
    );
}

/// ロール述語（HasRole/UserRoles・FGA 実効集合）が効く。書込述語も独立に効く。
#[tokio::test]
async fn role_predicates_and_write_policy() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let f = fixture(pool, authz.clone()).await;
    // charlie を経理ロールへ（role:keiri の member）。テーブル viewer も付与。
    let charlie = ctx(&f.alice.tenant_id, "charlie");
    authz
        .write_tuple(
            &charlie.subject(),
            Relation::Member,
            &f.alice.ns().role("keiri"),
        )
        .await
        .expect("charlie in keiri");
    authz
        .write_tuple(
            &charlie.subject(),
            Relation::Viewer,
            &f.alice.ns().data_table(&f.table_id.to_string()),
        )
        .await
        .expect("charlie table viewer");
    // 経理は全行読める。
    let count = f
        .store
        .count_records(&charlie, f.table_id, None, None)
        .await
        .expect("keiri count");
    assert_eq!(count, 2, "経理ロールは全行可視");
    // だが write 述語（applicant==$user.id）で他人の行は書けない（403・可視なので 404 でない）。
    let r = f
        .store
        .update_record(
            &charlie,
            f.table_id,
            f.alice_rec,
            json!({"amount": 999}),
            1,
            None,
        )
        .await;
    assert!(matches!(r, Err(DataError::Forbidden)), "{r:?}");
}

/// ④ lookup 越しに不可視テーブルの値を釣れない（PIT-20）。
#[tokio::test]
async fn lookup_does_not_leak_invisible_reference() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let store = DataStore::new(pool, Arc::clone(&authz), Arc::new(AllowRefs));

    // 参照先 salary テーブル: 行ポリシー = 本人のみ（bob からは alice の行が不可視）。
    let salary = store
        .create_table(
            &alice,
            NewDataTable {
                name: "salary".into(),
                schema: TableSchema {
                    fields: vec![
                        field("person", FieldType::UserRef),
                        field("grade", FieldType::Text),
                    ],
                    status_field: None,
                    row_policy: Some(RowPolicy {
                        read: PolicyExpr::FieldCmp {
                            field: "person".into(),
                            op: CmpOp::Eq,
                            value: PolicyOperand::UserId,
                        },
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
        .expect("salary table");
    let alice_salary = store
        .create_record(
            &alice,
            salary.id,
            json!({"person": "alice", "grade": "S1"}),
            None,
        )
        .await
        .expect("salary rec");

    // 参照元 members テーブル: bob にも可視（policy なし）。lookup で salary.grade を射影。
    let mut ref_f = field("salary_ref", FieldType::RecordRef);
    ref_f.ref_table = Some(salary.id);
    let mut lk = field("grade", FieldType::Lookup);
    lk.lookup = Some(data::LookupDef {
        via_field: "salary_ref".into(),
        target_field: "grade".into(),
    });
    let members = store
        .create_table(
            &alice,
            NewDataTable {
                name: "members".into(),
                schema: TableSchema {
                    fields: vec![field("name", FieldType::Text), ref_f, lk],
                    status_field: None,
                    row_policy: None,
                    field_policy: vec![],
                    aggregate_min_rows: None,
                    fsm_ref: None,
                },
            },
            None,
        )
        .await
        .expect("members table");
    // bob へ members と salary の**テーブル** viewer を付与（行述語だけが防波堤の状況）。
    for (t, rel) in [
        (members.id, Relation::Viewer),
        (salary.id, Relation::Viewer),
    ] {
        authz
            .write_tuple(&bob.subject(), rel, &alice.ns().data_table(&t.to_string()))
            .await
            .expect("share to bob");
    }
    let row = store
        .create_record(
            &alice,
            members.id,
            json!({"name": "Alice", "salary_ref": alice_salary.id.to_string()}),
            None,
        )
        .await
        .expect("member row");

    // alice には grade が見える。
    let got = store
        .get_record(&alice, members.id, row.id, None)
        .await
        .expect("alice get");
    assert_eq!(got.data["grade"], json!("S1"));
    // bob には **null**（参照なしと区別できない＝存在オラクルなし）。行述語で防波堤。
    let got = store
        .get_record(&bob, members.id, row.id, None)
        .await
        .expect("bob get");
    assert_eq!(got.data["grade"], serde_json::Value::Null);

    // bob は不可視の salary 行を参照する行を**作れない**（参照持ち込み拒否・存在探索不能）。
    authz
        .write_tuple(
            &bob.subject(),
            Relation::Editor,
            &alice.ns().data_table(&members.id.to_string()),
        )
        .await
        .expect("bob members editor");
    let probe_existing = store
        .create_record(
            &bob,
            members.id,
            json!({"name": "x", "salary_ref": alice_salary.id.to_string()}),
            None,
        )
        .await;
    let probe_missing = store
        .create_record(
            &bob,
            members.id,
            json!({"name": "x", "salary_ref": Uuid::new_v4().to_string()}),
            None,
        )
        .await;
    // 実在・不実在で応答が同形（存在オラクルなし）。
    assert_eq!(
        format!("{probe_existing:?}"),
        format!("{probe_missing:?}"),
        "参照先の実在有無で応答が変わらないこと"
    );
    assert!(matches!(probe_existing, Err(DataError::Invalid(_))));
}

/// ⑦ 個別共有で不可視行が追加で見え、解除で**即時**（TTL 内でも）不可視へ戻る。
#[tokio::test]
async fn record_share_grants_and_revokes_immediately() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let f = fixture(pool, authz).await;

    // 共有前: 不可視。
    assert!(matches!(
        f.store
            .get_record(&f.bob, f.table_id, f.alice_rec, None)
            .await,
        Err(DataError::NotFound)
    ));
    // alice（作成者）が bob へ viewer 共有 → 追加で見える。
    f.store
        .share_record(
            &f.alice,
            f.table_id,
            f.alice_rec,
            &storage::ShareTarget::User { id: "bob".into() },
            RecordShareRole::Viewer,
            None,
        )
        .await
        .expect("share");
    f.store
        .get_record(&f.bob, f.table_id, f.alice_rec, None)
        .await
        .expect("bob reads shared record");
    // viewer 共有では書けない（write 述語外＋editor 共有なし → 403）。
    let r = f
        .store
        .update_record(
            &f.bob,
            f.table_id,
            f.alice_rec,
            json!({"amount": 1}),
            1,
            None,
        )
        .await;
    assert!(matches!(r, Err(DataError::Forbidden)), "{r:?}");
    // editor 共有なら書ける。
    f.store
        .share_record(
            &f.alice,
            f.table_id,
            f.alice_rec,
            &storage::ShareTarget::User { id: "bob".into() },
            RecordShareRole::Editor,
            None,
        )
        .await
        .expect("share editor");
    f.store
        .update_record(
            &f.bob,
            f.table_id,
            f.alice_rec,
            json!({"amount": 150}),
            1,
            None,
        )
        .await
        .expect("bob edits shared record");

    // 解除 → キャッシュ TTL 内でも世代失効で直ちに不可視（⑦）。
    for role in [RecordShareRole::Viewer, RecordShareRole::Editor] {
        f.store
            .unshare_record(
                &f.alice,
                f.table_id,
                f.alice_rec,
                &storage::ShareTarget::User { id: "bob".into() },
                role,
                None,
            )
            .await
            .expect("unshare");
    }
    assert!(matches!(
        f.store
            .get_record(&f.bob, f.table_id, f.alice_rec, None)
            .await,
        Err(DataError::NotFound)
    ));

    // 共有権限のない bob は alice の行を共有できない（可視性もないため 404）。
    let r = f
        .store
        .share_record(
            &f.bob,
            f.table_id,
            f.alice_rec,
            &storage::ShareTarget::User {
                id: "charlie".into(),
            },
            RecordShareRole::Viewer,
            None,
        )
        .await;
    assert!(matches!(r, Err(DataError::NotFound)), "{r:?}");
}

/// ⑤ 共有集合の上限超過は「見えなくなる」方向（fail-closed）＋ shares_truncated 通知（PIT-18）。
///
/// 1 万件のタプルを実 FGA に書くのは重いため、read_subject_objects だけ肥大集合を返す
/// ラッパで DataStore に注入する（上限処理は material 層＝この境界の内側で起きる）。
#[tokio::test]
async fn share_overflow_fails_closed_with_truncation_flag() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let f = fixture(pool.clone(), Arc::clone(&authz)).await;

    /// 実 FGA へ委譲しつつ、data_record の直接タプル一覧だけ「上限超の巨大集合＋
    /// 実共有 id を**末尾**」で返す（超過分は落ちる＝実共有も見えない方向を検証）。
    struct OverflowShares {
        inner: Arc<dyn AuthzClient>,
        tenant: String,
        real: Uuid,
    }
    #[async_trait]
    impl AuthzClient for OverflowShares {
        async fn check(
            &self,
            s: &Subject,
            r: Relation,
            o: &FgaObject,
            c: Consistency,
        ) -> Result<bool, AuthzError> {
            self.inner.check(s, r, o, c).await
        }
        async fn write_tuple(
            &self,
            s: &Subject,
            r: Relation,
            o: &FgaObject,
        ) -> Result<bool, AuthzError> {
            self.inner.write_tuple(s, r, o).await
        }
        async fn delete_tuple(
            &self,
            s: &Subject,
            r: Relation,
            o: &FgaObject,
        ) -> Result<bool, AuthzError> {
            self.inner.delete_tuple(s, r, o).await
        }
        async fn read_tuples(
            &self,
            o: &FgaObject,
            r: Option<Relation>,
        ) -> Result<Vec<ReadTupleKey>, AuthzError> {
            self.inner.read_tuples(o, r).await
        }
        async fn list_objects(
            &self,
            s: &Subject,
            r: Relation,
            t: ObjectType,
        ) -> Result<Vec<String>, AuthzError> {
            if t == ObjectType::DataRecord {
                // 10,000 件のダミー + 実共有 id を末尾（上限で切り詰められる位置）。
                let mut v: Vec<String> = (0..10_000)
                    .map(|_| format!("data_record:{}|{}", self.tenant, Uuid::new_v4()))
                    .collect();
                v.push(format!("data_record:{}|{}", self.tenant, self.real));
                return Ok(v);
            }
            self.inner.list_objects(s, r, t).await
        }
        async fn delete_object_tuples(&self, o: &FgaObject) -> Result<u32, AuthzError> {
            self.inner.delete_object_tuples(o).await
        }
        async fn read_subject_objects(
            &self,
            s: &Subject,
            t: ObjectType,
        ) -> Result<Vec<String>, AuthzError> {
            self.inner.read_subject_objects(s, t).await
        }
    }

    // 実共有を張っておく（実 FGA 上は可視のはずだが、集合超過で落ちる＝fail-closed）。
    f.store
        .share_record(
            &f.alice,
            f.table_id,
            f.alice_rec,
            &storage::ShareTarget::User { id: "bob".into() },
            RecordShareRole::Viewer,
            None,
        )
        .await
        .expect("share");

    let overflow_store = DataStore::new(
        pool,
        Arc::new(OverflowShares {
            inner: authz,
            tenant: f.alice.tenant_id.clone(),
            real: f.alice_rec,
        }),
        Arc::new(AllowRefs),
    );
    let page = overflow_store
        .list_records(&f.bob, f.table_id, &ListRecordsOptions::default(), None)
        .await
        .expect("list with overflow");
    assert!(page.shares_truncated, "切り詰めが通知されること");
    assert!(
        page.items.iter().all(|r| r.id != f.alice_rec),
        "超過分の共有は**見えない方向**に倒れること（見えすぎない）"
    );
}

//! 行レベル認可の脅威モデルテスト行列（Task 9.3 受け入れ条件・PIT-17〜21）。
//!
//! 「WHERE 強制注入は必要条件にすぎない」（PIT-21）を受け、漏れ口ごとに対策を検証する:
//! ① 不可視行の 404 形状一致（存在オラクルなし） ② クライアントフィルタでの述語バイパス不能
//! ③ 集計（count）からの不可視行除外 ④ lookup 越しの釣り出し失敗（PIT-20）
//! ⑤ 共有上限フォールバックが可視減方向（PIT-18） ⑥ rev オラクル封じ ⑦ 共有剥奪の即時反映。
//! ⑧ リンク越しの lookup / rollup / 数式が参照先の 4 階層を閲覧者本人の権限で適用する（PIT-20）
//! ⑨ 材料化した計算列が、参照先の権限の後付け（row_policy / field_policy / 個別共有 / ReBAC）で
//!    同一 Tx に破棄され、値が読めず・ソートに使えなくなる（PIT-71）
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）＋実 OpenFGA（`OPENFGA_TEST_URL`）で実行する。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::too_many_lines
)]

use std::sync::Arc;

use async_trait::async_trait;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use data::{
    CmpOp, DataError, DataQuery, DataStore, FieldDef, FieldPolicy, FieldType, LinkCardinality,
    LinkDef, ListRecordsOptions, LookupDef, Materialization, NewDataTable, PolicyExpr,
    PolicyOperand, RecordFilter, RecordShareRole, RefResolver, RollupDef, RollupOp, RowPolicy,
    SortKey, TableSchema,
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
    FieldDef::new(name, ty)
}

/// 申請者本人 or 経理ロール（subtree）だけが読める経費テーブル。
fn expense_schema_with_policy() -> TableSchema {
    // row_policy が参照する列は indexed 必須（Task 15.3・data-platform §6.3）。
    let mut applicant = field("applicant", FieldType::UserRef);
    applicant.indexed = true;
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
                        // row_policy が参照する列は indexed 必須（Task 15.3）。
                        FieldDef {
                            indexed: true,
                            ..field("person", FieldType::UserRef)
                        },
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

// ---------------------------------------------------------------------------
// ⑧ リンク越しの lookup / rollup / 数式（Task 15.4・PIT-20）
// ---------------------------------------------------------------------------

fn plain(fields: Vec<FieldDef>) -> TableSchema {
    TableSchema {
        fields,
        status_field: None,
        row_policy: None,
        field_policy: vec![],
        aggregate_min_rows: Some(1),
        fsm_ref: None,
    }
}

fn items_link(to: Uuid) -> FieldDef {
    let mut f = field("items", FieldType::RecordRef);
    f.ref_table = Some(to);
    f.link = Some(LinkDef {
        cardinality: LinkCardinality::ManyToMany,
        symmetric_field: None,
        symmetric_key: None,
        reverse: false,
    });
    f
}

fn total_rollup(indexed: bool) -> FieldDef {
    let mut f = field("total", FieldType::Rollup);
    f.rollup = Some(RollupDef {
        link: "items".into(),
        target: Some("amount".into()),
        op: RollupOp::Sum,
    });
    f.indexed = indexed;
    f
}

fn derived_fields(lines: Uuid) -> Vec<FieldDef> {
    let mut names = field("names", FieldType::Lookup);
    names.lookup = Some(LookupDef {
        via_field: "items".into(),
        target_field: "name".into(),
    });
    let mut n = field("n", FieldType::Rollup);
    n.rollup = Some(RollupDef {
        link: "items".into(),
        target: None,
        op: RollupOp::Count,
    });
    let mut doubled = field("doubled", FieldType::Formula);
    doubled.formula =
        Some(serde_json::from_value(json!({"mul": [{"field": "total"}, {"lit": 2}]})).unwrap());
    vec![items_link(lines), total_rollup(false), names, n, doubled]
}

fn ids(v: &[Uuid]) -> serde_json::Value {
    serde_json::Value::Array(v.iter().map(|u| json!(u.to_string())).collect())
}

async fn grant(authz: &Arc<dyn AuthzClient>, who: &AuthContext, rel: Relation, table: Uuid) {
    authz
        .write_tuple(
            &who.subject(),
            rel,
            &who.ns().data_table(&table.to_string()),
        )
        .await
        .expect("grant");
}

/// ⑧-1 参照先テーブルの viewer 権限を持たない閲覧者には、参照先の全件から計算したロールアップも
/// lookup も見えない（テーブル ReBAC＝第 1 層の迂回を塞ぐ・PIT-71 ①・既定の per_viewer）。
#[tokio::test]
async fn rollup_does_not_bypass_reference_table_rebac() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let store = DataStore::new(pool, Arc::clone(&authz), Arc::new(AllowRefs));
    let lines = store
        .create_table(
            &alice,
            NewDataTable {
                name: "lines".into(),
                schema: plain(vec![
                    field("name", FieldType::Text),
                    field("amount", FieldType::Number),
                ]),
            },
            None,
        )
        .await
        .unwrap();
    let orders = store
        .create_table(
            &alice,
            NewDataTable {
                name: "orders".into(),
                schema: plain(derived_fields(lines.id)),
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        orders.schema.field("total").unwrap().materialization,
        Some(Materialization::PerViewer),
        "包含を証明できないので既定は per_viewer"
    );
    let mut ls = Vec::new();
    for (n, a) in [("l1", 100), ("l2", 250)] {
        ls.push(
            store
                .create_record(&alice, lines.id, json!({"name": n, "amount": a}), None)
                .await
                .unwrap()
                .id,
        );
    }
    let o = store
        .create_record(&alice, orders.id, json!({"items": ids(&ls)}), None)
        .await
        .unwrap();
    let got = store
        .get_record(&alice, orders.id, o.id, None)
        .await
        .unwrap();
    assert_eq!(got.data["total"], json!(350));
    assert_eq!(got.data["names"], json!(["l1", "l2"]));
    assert_eq!(got.data["doubled"], json!(700));

    // bob は orders の viewer だが lines の viewer ではない。
    grant(&authz, &bob, Relation::Viewer, orders.id).await;
    for got in [
        store.get_record(&bob, orders.id, o.id, None).await.unwrap(),
        store
            .run_query(&bob, orders.id, &DataQuery::default(), None)
            .await
            .unwrap()
            .items
            .unwrap()
            .remove(0),
    ] {
        assert_eq!(
            got.data["total"],
            serde_json::Value::Null,
            "全件の合計が漏れない"
        );
        assert_eq!(got.data["n"], serde_json::Value::Null, "件数も漏れない");
        assert_eq!(got.data["names"], json!([]), "lookup も空");
        assert_eq!(
            got.data["doubled"],
            serde_json::Value::Null,
            "数式越しにも漏れない"
        );
    }
    // per_viewer 列は並べ替えにも使えない（順序から値を推測させない）。
    let q = DataQuery {
        sort: vec![SortKey {
            field: "total".into(),
            descending: true,
        }],
        ..DataQuery::default()
    };
    let r = store.run_query(&bob, orders.id, &q, None).await;
    assert!(matches!(r, Err(DataError::Invalid(_))), "{r:?}");
}

/// ⑧-2 参照先の行述語・個別共有・フィールドマスクを閲覧者本人の権限で透過適用する（PIT-20）。
/// 見えない参照先は null / 件数外になり、個別共有した行だけが追加で入る。
#[tokio::test]
async fn rollup_and_lookup_apply_reference_row_policy_per_viewer() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let store = DataStore::new(pool, Arc::clone(&authz), Arc::new(AllowRefs));
    let mut person = field("person", FieldType::UserRef);
    person.indexed = true;
    let mut scope = field("scope", FieldType::Select);
    scope.options = vec!["all".into(), "private".into()];
    scope.indexed = true;
    let mut lines_schema = plain(vec![
        person,
        scope,
        field("name", FieldType::Text),
        field("amount", FieldType::Number),
        field("cost", FieldType::Number),
    ]);
    lines_schema.row_policy = Some(RowPolicy {
        read: PolicyExpr::Any(vec![
            PolicyExpr::FieldCmp {
                field: "person".into(),
                op: CmpOp::Eq,
                value: PolicyOperand::UserId,
            },
            PolicyExpr::FieldCmp {
                field: "scope".into(),
                op: CmpOp::Eq,
                value: PolicyOperand::Lit(json!("all")),
            },
        ]),
        write: None,
    });
    // 原価（cost）は keiri ロールだけが読める。
    lines_schema.field_policy = vec![FieldPolicy {
        field: "cost".into(),
        readable_by: PolicyExpr::HasRole {
            role: "keiri".into(),
            subtree: false,
        },
    }];
    let lines = store
        .create_table(
            &alice,
            NewDataTable {
                name: "lines".into(),
                schema: lines_schema,
            },
            None,
        )
        .await
        .unwrap();
    let mut fields = derived_fields(lines.id);
    let mut cost = field("cost_total", FieldType::Rollup);
    cost.rollup = Some(RollupDef {
        link: "items".into(),
        target: Some("cost".into()),
        op: RollupOp::Sum,
    });
    fields.push(cost);
    let orders = store
        .create_table(
            &alice,
            NewDataTable {
                name: "orders".into(),
                schema: plain(fields),
            },
            None,
        )
        .await
        .unwrap();
    let public = store
        .create_record(
            &alice,
            lines.id,
            json!({"person": "alice", "scope": "all", "name": "公開", "amount": 100, "cost": 60}),
            None,
        )
        .await
        .unwrap();
    let private = store
        .create_record(
            &alice,
            lines.id,
            json!({"person": "alice", "scope": "private", "name": "非公開", "amount": 250, "cost": 1}),
            None,
        )
        .await
        .unwrap();
    let o = store
        .create_record(
            &alice,
            orders.id,
            json!({"items": ids(&[public.id, private.id])}),
            None,
        )
        .await
        .unwrap();
    grant(&authz, &bob, Relation::Viewer, orders.id).await;
    grant(&authz, &bob, Relation::Viewer, lines.id).await;
    let alice_view = store
        .get_record(&alice, orders.id, o.id, None)
        .await
        .unwrap();
    assert_eq!(alice_view.data["total"], json!(350));
    assert_eq!(alice_view.data["n"], json!(2));
    // alice も keiri ではないので原価は集計に入らない（マスク列は持ち出さない・PIT-19）。
    assert_eq!(alice_view.data["cost_total"], json!(0));
    // bob には非公開行が見えない → 合計・件数・lookup・数式から消える（null ではなく可視分だけ）。
    let bob_view = store.get_record(&bob, orders.id, o.id, None).await.unwrap();
    assert_eq!(bob_view.data["total"], json!(100));
    assert_eq!(bob_view.data["n"], json!(1));
    assert_eq!(bob_view.data["names"], json!(["公開"]));
    assert_eq!(bob_view.data["doubled"], json!(200));
    // 個別共有した行だけが追加で集計に入る（解除すれば即時に戻る）。
    let target = storage::ShareTarget::User { id: "bob".into() };
    store
        .share_record(
            &alice,
            lines.id,
            private.id,
            &target,
            RecordShareRole::Viewer,
            None,
        )
        .await
        .unwrap();
    let bob_view = store.get_record(&bob, orders.id, o.id, None).await.unwrap();
    assert_eq!(bob_view.data["total"], json!(350));
    store
        .unshare_record(
            &alice,
            lines.id,
            private.id,
            &target,
            RecordShareRole::Viewer,
            None,
        )
        .await
        .unwrap();
    let bob_view = store.get_record(&bob, orders.id, o.id, None).await.unwrap();
    assert_eq!(bob_view.data["total"], json!(100));
    // bob は見えない参照先へのリンクを書き込めない（存在オラクルなし・同一応答）。
    grant(&authz, &bob, Relation::Editor, orders.id).await;
    let r = store
        .create_record(&bob, orders.id, json!({"items": ids(&[private.id])}), None)
        .await;
    assert!(
        matches!(&r, Err(DataError::Invalid(m)) if m.contains("見つかりません")),
        "{r:?}"
    );
}

// ---------------------------------------------------------------------------
// ⑨ 材料化した計算列の残留（PIT-71）
// ---------------------------------------------------------------------------

/// 同一ミニアプリの 2 テーブル（参照先 lines・参照元 orders の材料化ロールアップ）。
struct Materialized {
    store: DataStore,
    pool: PgPool,
    authz: Arc<dyn AuthzClient>,
    alice: AuthContext,
    bob: AuthContext,
    lines: Uuid,
    orders: Uuid,
    line_ids: Vec<Uuid>,
    order_id: Uuid,
    total_field: i16,
}

async fn materialized_fixture(pool: PgPool, authz: Arc<dyn AuthzClient>) -> Materialized {
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let store = DataStore::new(pool.clone(), Arc::clone(&authz), Arc::new(AllowRefs));
    let app = Uuid::new_v4();
    let mut person = field("person", FieldType::UserRef);
    person.indexed = true;
    let lines = store
        .create_table_for_app(
            &alice,
            NewDataTable {
                name: "lines".into(),
                schema: plain(vec![
                    person,
                    field("name", FieldType::Text),
                    field("amount", FieldType::Number),
                ]),
            },
            app,
            None,
        )
        .await
        .unwrap();
    let orders = store
        .create_table_for_app(
            &alice,
            NewDataTable {
                name: "orders".into(),
                schema: plain(vec![items_link(lines.id), total_rollup(true)]),
            },
            app,
            None,
        )
        .await
        .unwrap();
    let total = orders.schema.field("total").unwrap();
    assert_eq!(
        total.materialization,
        Some(Materialization::Invariant),
        "前提: 同一ミニアプリ・包含の証明・行述語/マスク/個別共有なし → 材料化"
    );
    let mut line_ids = Vec::new();
    for (who, a) in [("alice", 100), ("bob", 250)] {
        line_ids.push(
            store
                .create_record(
                    &alice,
                    lines.id,
                    json!({"person": who, "name": who, "amount": a}),
                    None,
                )
                .await
                .unwrap()
                .id,
        );
    }
    let order = store
        .create_record(&alice, orders.id, json!({"items": ids(&line_ids)}), None)
        .await
        .unwrap();
    let m = Materialized {
        store,
        pool,
        authz,
        alice,
        bob,
        lines: lines.id,
        orders: orders.id,
        line_ids,
        order_id: order.id,
        total_field: i16::try_from(total.id).unwrap(),
    };
    assert_eq!(
        m.materialized_rows().await,
        2,
        "前提: 材料化した索引行（1 件）と依存登録（1 件）がある"
    );
    m.store
        .run_query(&m.alice, m.orders, &m.by_total(), None)
        .await
        .expect("前提: 材料化列でソートできる");
    m
}

impl Materialized {
    #[allow(clippy::unused_self)] // 呼び出し側を m.by_total() で揃える。
    fn by_total(&self) -> DataQuery {
        DataQuery {
            sort: vec![SortKey {
                field: "total".into(),
                descending: true,
            }],
            ..DataQuery::default()
        }
    }

    /// 材料化データ（索引行・再計算待ち・依存登録）の行数（テスト専用に DB から直接数える）。
    async fn materialized_rows(&self) -> i64 {
        let (ti, tb): (i32, i64) = sqlx::query_as(
            "SELECT tenant_int, table_int FROM data_table WHERE tenant_id = $1 AND id = $2",
        )
        .bind(&self.alice.tenant_id)
        .bind(self.orders)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM data_index_num WHERE tenant_int = $1 AND table_int = $2 \
                       AND field_id = $3) \
                  + (SELECT count(*) FROM data_index_text WHERE tenant_int = $1 AND table_int = $2 \
                       AND field_id = $3) \
                  + (SELECT count(*) FROM data_derived_pending WHERE tenant_int = $1 \
                       AND table_int = $2 AND field_id = $3) \
                  + (SELECT count(*) FROM data_derived_dep WHERE tenant_int = $1 AND table_int = $2 \
                       AND field_id = $3)",
        )
        .bind(ti)
        .bind(tb)
        .bind(self.total_field)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    /// 材料化データが破棄され、値がソートに使えず、宣言が per_viewer へ降格していること。
    async fn assert_discarded(&self, case: &str) {
        assert_eq!(
            self.materialized_rows().await,
            0,
            "{case}: 材料化データが残っている"
        );
        let t = self
            .store
            .get_table(&self.alice, self.orders, None)
            .await
            .unwrap();
        let f = t.schema.field("total").unwrap();
        assert_eq!(
            f.materialization,
            Some(Materialization::PerViewer),
            "{case}"
        );
        assert!(!f.indexed, "{case}: 降格した列は索引宣言も外れる");
        let r = self
            .store
            .run_query(&self.alice, self.orders, &self.by_total(), None)
            .await;
        assert!(
            matches!(r, Err(DataError::Invalid(_))),
            "{case}: 降格した列はソートに使えない: {r:?}"
        );
    }

    async fn total_seen_by(&self, who: &AuthContext) -> serde_json::Value {
        self.store
            .get_record(who, self.orders, self.order_id, None)
            .await
            .unwrap()
            .data["total"]
            .clone()
    }
}

/// ⑨-1 参照先へ row_policy を後付けすると、材料化データが同一 Tx で破棄され、閲覧者本人の
/// 可視行だけの値になる（後から権限を厳しくした＝安全側の操作で漏洩源が残らない）。
#[tokio::test]
async fn materialized_rollup_is_discarded_when_reference_gets_row_policy() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let m = materialized_fixture(pool, authz).await;
    // bob は両テーブルの viewer（包含は保たれる＝ReBAC は変えない）。
    grant(&m.authz, &m.bob, Relation::Viewer, m.lines).await;
    grant(&m.authz, &m.bob, Relation::Viewer, m.orders).await;
    let mut s = m
        .store
        .get_table(&m.alice, m.lines, None)
        .await
        .unwrap()
        .schema;
    s.row_policy = Some(RowPolicy {
        read: PolicyExpr::FieldCmp {
            field: "person".into(),
            op: CmpOp::Eq,
            value: PolicyOperand::UserId,
        },
        write: None,
    });
    m.store
        .update_table_schema(&m.alice, m.lines, s, None, None)
        .await
        .unwrap();
    m.assert_discarded("row_policy 後付け").await;
    // 値は閲覧者本人の可視行だけ（全件の 350 は誰にも見えない）。
    assert_eq!(m.total_seen_by(&m.alice).await, json!(100));
    assert_eq!(m.total_seen_by(&m.bob).await, json!(250));
}

/// ⑨-2 参照先にレコード個別共有を 1 件作ると、材料化データが破棄される。
#[tokio::test]
async fn materialized_rollup_is_discarded_when_reference_record_is_shared() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let m = materialized_fixture(pool, authz).await;
    m.store
        .share_record(
            &m.alice,
            m.lines,
            m.line_ids[0],
            &storage::ShareTarget::User { id: "bob".into() },
            RecordShareRole::Viewer,
            None,
        )
        .await
        .unwrap();
    m.assert_discarded("個別共有の発生").await;
    assert_eq!(m.total_seen_by(&m.alice).await, json!(350));
    // 共有を解除しても材料化には戻らない（旗は単調・安全側）。
    let t = m.store.get_table(&m.alice, m.orders, None).await.unwrap();
    let mut s = t.schema.clone();
    s.fields
        .iter_mut()
        .find(|f| f.key == "total")
        .unwrap()
        .indexed = true;
    let r = m
        .store
        .update_table_schema(&m.alice, m.orders, s, None, None)
        .await;
    assert!(matches!(r, Err(DataError::Invalid(_))), "{r:?}");
}

/// ⑨-3 テーブル ReBAC を変えて viewer 集合の包含が崩れると（参照元だけを bob に見せる）、次の読取の
/// 入口で材料化データが破棄され、bob には参照先の全件から計算した値が見えない（第 1 層の迂回を塞ぐ）。
#[tokio::test]
async fn materialized_rollup_is_discarded_when_table_rebac_changes() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let m = materialized_fixture(pool, authz).await;
    grant(&m.authz, &m.bob, Relation::Viewer, m.orders).await;
    // bob は lines を見られない → 合計は null（材料化した 350 は返らない）。
    assert_eq!(m.total_seen_by(&m.bob).await, serde_json::Value::Null);
    m.assert_discarded("ReBAC の変更").await;
    // bob のソートも同じく拒否される（順序から値を推測させない）。
    let r = m
        .store
        .run_query(&m.bob, m.orders, &m.by_total(), None)
        .await;
    assert!(matches!(r, Err(DataError::Invalid(_))), "{r:?}");
    // 明示の再検査の入口（ReBAC を変えた経路から呼ぶ）は、降格済みなら何もしない。
    assert!(!m
        .store
        .reverify_materialization(&m.alice, m.orders)
        .await
        .unwrap());
}

/// ⑨-4 ReBAC の変更をクエリ（ソート）の入口で検出した場合も、その場で破棄して拒否する。
/// 参照先へ field_policy（マスク）を後付けした場合も同様に破棄される。
#[tokio::test]
async fn materialized_rollup_is_discarded_on_query_and_on_field_policy() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let m = materialized_fixture(pool.clone(), Arc::clone(&authz)).await;
    grant(&m.authz, &m.bob, Relation::Viewer, m.orders).await;
    let r = m
        .store
        .run_query(&m.bob, m.orders, &m.by_total(), None)
        .await;
    assert!(matches!(r, Err(DataError::Invalid(_))), "{r:?}");
    m.assert_discarded("ReBAC の変更（クエリの入口）").await;

    let m = materialized_fixture(pool, authz).await;
    let mut s = m
        .store
        .get_table(&m.alice, m.lines, None)
        .await
        .unwrap()
        .schema;
    s.field_policy = vec![FieldPolicy {
        field: "amount".into(),
        readable_by: PolicyExpr::HasRole {
            role: "keiri".into(),
            subtree: false,
        },
    }];
    m.store
        .update_table_schema(&m.alice, m.lines, s, None, None)
        .await
        .unwrap();
    m.assert_discarded("field_policy 後付け").await;
    // マスクされた列は集計の材料にもならない（alice も keiri ではない）。
    assert_eq!(m.total_seen_by(&m.alice).await, json!(0));
}

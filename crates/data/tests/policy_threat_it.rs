//! 行レベル認可の脅威モデルテスト行列（Task 9.3 受け入れ条件・PIT-17〜21）。
//!
//! 「WHERE 強制注入は必要条件にすぎない」（PIT-21）を受け、漏れ口ごとに対策を検証する:
//! ① 不可視行の 404 形状一致（存在オラクルなし） ② クライアントフィルタでの述語バイパス不能
//! ③ 集計（count）からの不可視行除外 ④ lookup 越しの釣り出し失敗（PIT-20）
//! ⑤ 共有上限フォールバックが可視減方向（PIT-18） ⑥ rev オラクル封じ ⑦ 共有剥奪の即時反映。
//! ⑧ リンク越しの lookup / rollup / 数式が参照先の 4 階層を閲覧者本人の権限で適用する（PIT-20）
//! ⑨ 材料化した計算列が、参照先の権限の後付け（row_policy / field_policy / 個別共有 / ReBAC）で
//!    同一 Tx に破棄され、値が読めず・ソートに使えなくなる（PIT-71）
//! ⑩ SSE 差分配信（Task 15.6）でも、見えない行・マスク列・可視→不可視の行の値が流れない
//!    （PIT-73。購読者ごとに行述語とマスクを評価し、可視→不可視は id だけの removed）
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
    authz: Arc<dyn AuthzClient>,
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
        authz,
        alice,
        bob,
        table_id: table.id,
        alice_rec: alice_rec.id,
        bob_rec: bob_rec.id,
    }
}

impl Fixture {
    /// テーブルの editor を付与する（このテストファイルの他の検証と同じ ReBAC の形）。
    async fn authz_grant_editor(&self, who: &AuthContext, table: Uuid) {
        self.authz
            .write_tuple(
                &who.subject(),
                Relation::Editor,
                &self.alice.ns().data_table(&table.to_string()),
            )
            .await
            .unwrap();
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
    // ⑪ SQL 前段面でも同じ（不可視行は SQL でも出ない）。
    for who in [&f.alice, &f.bob] {
        sql_mirror(&f.store, who, f.table_id, "expense").await;
    }
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
    // ⑪ SQL 前段面: SQL 風の値はリテラル（バインド）で、述語は AND 合成のまま。
    let rows = sql_rows(
        &f.store,
        &f.bob,
        "SELECT _id FROM expense WHERE status = 'draft'' OR ''1''=''1'",
    )
    .await;
    assert!(rows.is_empty(), "{rows:?}");
    let rows = sql_rows(
        &f.store,
        &f.bob,
        "SELECT _id FROM expense WHERE status = 'draft' OR TRUE",
    )
    .await;
    assert_eq!(rows, vec![vec![json!(f.bob_rec.to_string())]]);
    sql_mirror(&f.store, &f.bob, f.table_id, "expense").await;
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
    // ⑪ SQL 前段面の集計も同じ（不可視行を数えない・K 未満は抑制）。
    for who in [&f.alice, &f.bob] {
        sql_mirror(&f.store, who, f.table_id, "expense").await;
    }
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
    // ⑪ SQL 前段面でもロール述語（HasRole / UserRoles）が同じに効く。
    for who in [&charlie, &f.bob, &f.alice] {
        sql_mirror(&f.store, who, f.table_id, "expense").await;
    }
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
    // ⑪ SQL 前段面: lookup の値も、参照先テーブルの行も、閲覧者本人の権限で同じに見える。
    for who in [&alice, &bob] {
        sql_mirror(&store, who, members.id, "members").await;
        sql_mirror(&store, who, salary.id, "salary").await;
    }
    // JOIN でも参照先の行述語が掛かる（bob には alice の salary 行が見えない）。
    let rows = sql_rows(
        &store,
        &bob,
        "SELECT m.name, s.grade FROM members m JOIN salary s ON m.salary_ref = s._id",
    )
    .await;
    assert!(
        rows.is_empty(),
        "JOIN 越しに不可視の参照先を釣れない: {rows:?}"
    );
    let rows = sql_rows(
        &store,
        &alice,
        "SELECT m.name, s.grade FROM members m JOIN salary s ON m.salary_ref = s._id",
    )
    .await;
    assert_eq!(rows, vec![vec![json!("Alice"), json!("S1")]]);
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
    // ⑪ SQL 前段面でも個別共有した行が追加で見える。
    sql_mirror(&f.store, &f.bob, f.table_id, "expense").await;

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
    // ⑪ 解除後は SQL 前段面でも直ちに見えない。
    sql_mirror(&f.store, &f.bob, f.table_id, "expense").await;
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
    // ⑪ SQL 前段面でも超過分は見えない方向・切り詰めを通知する。
    let r = overflow_store
        .run_sql(&f.bob, &sql_req("SELECT _id FROM expense"), None)
        .await
        .unwrap();
    assert!(r.shares_truncated);
    assert!(r
        .rows
        .iter()
        .all(|row| row[0] != json!(f.alice_rec.to_string())));
    sql_mirror(&overflow_store, &f.bob, f.table_id, "expense").await;
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
    // ⑪ SQL 前段面: 参照先テーブルの viewer でない bob には、JOIN 先も「存在しないテーブル」と同じ応答。
    sql_mirror(&store, &bob, orders.id, "orders").await;
    let joined = store
        .run_sql(
            &bob,
            &sql_req("SELECT COUNT(*) FROM orders o JOIN lines l ON o.items = l._id"),
            None,
        )
        .await;
    let missing = store
        .run_sql(
            &bob,
            &sql_req("SELECT COUNT(*) FROM orders o JOIN nopes l ON o.items = l._id"),
            None,
        )
        .await;
    assert_eq!(
        format!("{joined:?}").replace("lines", "X"),
        format!("{missing:?}").replace("nopes", "X")
    );
    // IR のリンクの半結合でも参照先は開けない（403）。
    let q: DataQuery =
        serde_json::from_value(json!({"filter": {"link": {"field": "items"}}})).unwrap();
    let r = store.run_query(&bob, orders.id, &q, None).await;
    assert!(matches!(r, Err(DataError::Forbidden)), "{r:?}");
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
    // ⑪ SQL 前段面: lookup / rollup / 数式の値も、JOIN も、閲覧者本人の行述語・マスクで同じに見える。
    for who in [&alice, &bob] {
        sql_mirror(&store, who, orders.id, "orders").await;
        sql_mirror(&store, who, lines.id, "lines").await;
    }
    let join = "SELECT COUNT(*) FROM orders o JOIN lines l ON o.items = l._id";
    assert_eq!(sql_rows(&store, &bob, join).await, vec![vec![json!(1)]]);
    assert_eq!(sql_rows(&store, &alice, join).await, vec![vec![json!(2)]]);
    // 結合先の列の射影も、見える参照先の行だけ（bob には非公開の明細の名前が出ない）。
    let names = "SELECT l.name FROM orders o JOIN lines l ON o.items = l._id";
    assert_eq!(
        sql_rows(&store, &bob, names).await,
        vec![vec![json!("公開")]]
    );
    // 結合先のマスク列は「存在しない列」と同じ応答（JOIN 越しに原価を持ち出せない・PIT-19）。
    let masked = store
        .run_sql(
            &bob,
            &sql_req("SELECT SUM(l.cost) FROM orders o JOIN lines l ON o.items = l._id"),
            None,
        )
        .await;
    let missing = store
        .run_sql(
            &bob,
            &sql_req("SELECT SUM(l.nope) FROM orders o JOIN lines l ON o.items = l._id"),
            None,
        )
        .await;
    assert_eq!(
        format!("{masked:?}").replace("cost", "X"),
        format!("{missing:?}").replace("nope", "X")
    );
    // 半結合（行述語つき EXISTS）でも非公開の参照先は条件に使えない。
    let semi =
        "SELECT COUNT(*) FROM orders o JOIN lines l ON o.items = l._id WHERE l.scope = 'private'";
    // bob には 0 件（K=1 未満なので値は抑制＝null・IR と同じ）。
    assert_eq!(
        sql_rows(&store, &bob, semi).await,
        vec![vec![serde_json::Value::Null]]
    );
    assert_eq!(sql_rows(&store, &alice, semi).await, vec![vec![json!(1)]]);
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
    // ⑪ SQL 前段面でも同じ（材料化を破棄した列は閲覧者本人の権限で計算し直した値）。
    for who in [&m.alice, &m.bob] {
        sql_mirror(&m.store, who, m.orders, "orders").await;
        sql_mirror(&m.store, who, m.lines, "lines").await;
    }
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
    // ⑪ SQL 前段面でも同じ（材料化を破棄した列は閲覧者本人の権限で計算し直した値）。
    for who in [&m.alice, &m.bob] {
        sql_mirror(&m.store, who, m.orders, "orders").await;
        sql_mirror(&m.store, who, m.lines, "lines").await;
    }
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
    // ⑪ SQL 前段面でも同じ（材料化を破棄した列は閲覧者本人の権限で計算し直した値）。
    for who in [&m.alice, &m.bob] {
        sql_mirror(&m.store, who, m.orders, "orders").await;
        sql_mirror(&m.store, who, m.lines, "lines").await;
    }
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
    // ⑪ SQL 前段面でも同じ（材料化を破棄した列は閲覧者本人の権限で計算し直した値）。
    for who in [&m.alice, &m.bob] {
        sql_mirror(&m.store, who, m.orders, "orders").await;
        sql_mirror(&m.store, who, m.lines, "lines").await;
    }
}

/// ⑩ 材料化した派生列（text）の切り詰めキー（§3.3）は、行述語を迂回する判定材料にならない。
/// 長い比較値の 422 は比較値だけで決まり（データに依存しない）、group_by の 422 は閲覧者に見える行
/// だけで決まる（見えない行の値の長さは漏れない）。受け付けた比較は見える行の中で厳密。
#[tokio::test]
async fn long_derived_text_checks_do_not_leak_invisible_rows() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let store = DataStore::new(pool, Arc::clone(&authz), Arc::new(AllowRefs));
    let mut s = expense_schema_with_policy();
    let mut body = field("body", FieldType::Text);
    body.indexed = true;
    let mut label = field("label", FieldType::Formula);
    label.formula = Some(serde_json::from_value(json!({"concat": [{"field": "body"}]})).unwrap());
    label.indexed = true;
    s.fields.push(body);
    s.fields.push(label);
    s.aggregate_min_rows = Some(1);
    let table = store
        .create_table(
            &alice,
            NewDataTable {
                name: "notes".into(),
                schema: s,
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        table.schema.field("label").unwrap().materialization,
        Some(Materialization::Invariant),
        "自テーブルの列だけの数式は閲覧者に依らない（行述語は行の可視性だけを決める）"
    );
    grant(&authz, &bob, Relation::Editor, table.id).await;
    let long = format!("{}X", "a".repeat(2000));
    store
        .create_record(
            &alice,
            table.id,
            json!({"applicant": "alice", "amount": 1, "status": "draft", "body": long}),
            None,
        )
        .await
        .unwrap();
    let bob_rec = store
        .create_record(
            &bob,
            table.id,
            json!({"applicant": "bob", "amount": 2, "status": "draft", "body": "short"}),
            None,
        )
        .await
        .unwrap();
    let group = DataQuery {
        group_by: vec!["label".into()],
        aggregate: vec![data::MetricSpec {
            metric: data::Metric::Count,
            field: None,
        }],
        ..DataQuery::default()
    };
    // bob: alice の長い行は見えないので group_by は通り、bob の行だけが数えられる。
    let r = store.run_query(&bob, table.id, &group, None).await.unwrap();
    let groups = r.groups.unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].key["label"], json!("short"));
    assert_eq!(groups[0].value, json!(1));
    // alice: 自分に見える長い行があるので 422（違う値を 1 グループに混ぜない）。
    let r = store.run_query(&alice, table.id, &group, None).await;
    assert!(matches!(r, Err(DataError::Invalid(_))), "{r:?}");
    // 長い比較値は誰が投げても・行があってもなくても同じ 422（存在・長さのオラクルにならない）。
    for who in [&alice, &bob] {
        for v in [long.clone(), format!("{}Z", "a".repeat(2000))] {
            let q = DataQuery {
                filter: Some(data::Condition::cmp(
                    "label",
                    data::CondOp::Neq,
                    Some(json!(v)),
                )),
                ..DataQuery::default()
            };
            let r = store.run_query(who, table.id, &q, None).await;
            assert!(matches!(r, Err(DataError::Invalid(_))), "{r:?}");
        }
    }
    // 受け付けた比較は見える行の中で厳密（bob の前方一致に alice の行は出ない）。
    for (op, v, want) in [
        (data::CondOp::StartsWith, json!("a".repeat(1000)), vec![]),
        (data::CondOp::Neq, json!("short"), vec![]),
        (
            data::CondOp::Exists,
            serde_json::Value::Null,
            vec![bob_rec.id],
        ),
    ] {
        let q = DataQuery {
            filter: Some(data::Condition::cmp(
                "label",
                op,
                (!v.is_null()).then_some(v),
            )),
            ..DataQuery::default()
        };
        let got: Vec<Uuid> = store
            .run_query(&bob, table.id, &q, None)
            .await
            .unwrap()
            .items
            .unwrap()
            .into_iter()
            .map(|i| i.id)
            .collect();
        assert_eq!(got, want, "{op:?}");
    }
    // ⑪ SQL 前段面でも同じ。
    for who in [&alice, &bob] {
        sql_mirror(&store, who, table.id, "notes").await;
    }
}

/// ⑩ SSE 差分配信: 見えない行・マスク列の値はストリームに乗らない。可視→不可視の行は id だけの
/// `record.removed` が届き、値は再送されない（PIT-73）。
#[tokio::test]
async fn sse_never_streams_invisible_rows_or_masked_fields() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let f = fixture(pool, authz).await;
    // マスク列つきの表（amount は経理だけが読める）。
    let mut schema = expense_schema_with_policy();
    schema.field_policy = vec![FieldPolicy {
        field: "amount".into(),
        readable_by: PolicyExpr::HasRole {
            role: "keiri".into(),
            subtree: true,
        },
    }];
    let table = f
        .store
        .create_table(
            &f.alice,
            NewDataTable {
                name: "masked-expense".into(),
                schema,
            },
            None,
        )
        .await
        .unwrap();
    f.authz_grant_editor(&f.bob, table.id).await;
    let mut stream = f
        .store
        .subscribe_records(&f.bob, table.id, None, None)
        .await
        .unwrap();
    let hidden = f
        .store
        .create_record(
            &f.alice,
            table.id,
            json!({"applicant": "alice", "amount": 7_777_777, "status": "draft", "memo": "機密メモ"}),
            None,
        )
        .await
        .unwrap();
    let mine = f
        .store
        .create_record(
            &f.alice,
            table.id,
            json!({"applicant": "bob", "amount": 5_555_555, "status": "draft", "memo": "見える"}),
            None,
        )
        .await
        .unwrap();
    let mut seen: Vec<String> = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut got_mine = false;
    while std::time::Instant::now() < deadline && !got_mine {
        if let Ok(Some(m)) =
            tokio::time::timeout(std::time::Duration::from_millis(500), stream.next()).await
        {
            if let data::StreamEvent::Upserted(u) = &m.event {
                assert_eq!(u.record.id, mine.id, "見えない行は届かない");
                assert!(u.record.data.get("amount").is_none(), "マスク列は届かない");
                got_mine = true;
            }
            seen.push(format!("{} {}", m.event.name(), m.event.data_json()));
        }
    }
    assert!(got_mine, "見える行は届く: {seen:?}");
    // bob が自分の行を alice の行へ付け替える（可視 → 不可視）。値（memo）も書き換える。
    f.store
        .update_record(
            &f.bob,
            table.id,
            mine.id,
            json!({"applicant": "alice", "memo": "付け替え後の機密"}),
            1,
            None,
        )
        .await
        .unwrap();
    let mut removed = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline && !removed {
        if let Ok(Some(m)) =
            tokio::time::timeout(std::time::Duration::from_millis(500), stream.next()).await
        {
            let line = format!("{} {}", m.event.name(), m.event.data_json());
            if let data::StreamEvent::Removed(r) = &m.event {
                assert_eq!(r.record_id, mine.id);
                assert_eq!(m.event.data_json().as_object().unwrap().len(), 1, "id だけ");
                removed = true;
            }
            seen.push(line);
        }
    }
    assert!(removed, "可視→不可視は removed が届く: {seen:?}");
    let all = seen.join("\n");
    for leak in [
        hidden.id.to_string(),
        "機密メモ".into(),
        "付け替え後の機密".into(),
        "7777777".into(),
        "5555555".into(),
    ] {
        assert!(!all.contains(&leak), "{leak} が SSE に出た: {all}");
    }
    assert!(
        all.contains("見える"),
        "見える行（マスク外の列）は届く: {all}"
    );
    assert_eq!(f.store.stream_verify_drops(), 0);
    // ⑪ SQL 前段面: マスク列（amount）は「存在しない列」と同じ応答で、行述語も同じに効く。
    for who in [&f.alice, &f.bob] {
        sql_mirror(&f.store, who, table.id, "masked-expense").await;
    }
}

// ---------------------------------------------------------------------------
// ⑪ SQL 前段面（Task 15.10）: 同じ読取を SQL でも流し、IR 直投入と結果が一致すること
// ---------------------------------------------------------------------------

fn sql_req(sql: &str) -> data::SqlRequest {
    data::SqlRequest {
        sql: sql.into(),
        ..data::SqlRequest::default()
    }
}

/// SQL で全ページを読む。
async fn sql_rows(store: &DataStore, who: &AuthContext, sql: &str) -> Vec<Vec<serde_json::Value>> {
    let mut out = Vec::new();
    let mut cursor = None;
    loop {
        let r = store
            .run_sql(
                who,
                &data::SqlRequest {
                    sql: sql.into(),
                    cursor: cursor.clone(),
                    with_count: false,
                },
                None,
            )
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e:?}"));
        out.extend(r.rows);
        match r.next_cursor {
            Some(c) => cursor = Some(c),
            None => return out,
        }
    }
}

/// IR で全ページを読む（id → 指定した列の値・値なしは除く）。
async fn ir_rows(
    store: &DataStore,
    who: &AuthContext,
    table: Uuid,
    keys: &[String],
) -> Vec<(String, serde_json::Map<String, serde_json::Value>)> {
    let mut out = Vec::new();
    let mut cursor = None;
    loop {
        let q = DataQuery {
            select: Some(keys.to_vec()),
            limit: Some(200),
            cursor: cursor.clone(),
            ..DataQuery::default()
        };
        let r = store.run_query(who, table, &q, None).await.unwrap();
        for it in r.items.unwrap() {
            let mut m = serde_json::Map::new();
            for k in keys {
                if let Some(v) = it.data.get(k).filter(|v| !v.is_null()) {
                    m.insert(k.clone(), v.clone());
                }
            }
            out.push((it.id.to_string(), m));
        }
        match r.next_cursor {
            Some(c) => cursor = Some(c),
            None => return out,
        }
    }
}

/// 同じ読取（全行の射影・件数・グループ化）を SQL 前段面と IR 直投入の両方で流し、結果が一致すること。
///
/// 行述語・フィールドマスク・集計抑制・参照先の行述語（lookup / rollup）が SQL 経路でも同一に効く
/// （SQL は IR へ変換されて同じ経路を通る・data-platform §16.1）。マスク列は SQL では「存在しない列」と
/// 同じ 422 になり、IR ではその列の値が返らない。
async fn sql_mirror(store: &DataStore, who: &AuthContext, table_id: Uuid, name: &str) {
    let quoted = format!("\"{}\"", name.replace('"', "\"\""));
    let Ok(table) = store.get_table(who, table_id, None).await else {
        // テーブルが見えない利用者には SQL でも「見つからない」（存在しないテーブルと同じ）。
        let r = store
            .run_sql(who, &sql_req(&format!("SELECT _id FROM {quoted}")), None)
            .await;
        assert!(
            matches!(&r, Err(DataError::Rejected(x)) if x.code == data::RejectCode::UnknownName),
            "{r:?}"
        );
        assert!(store
            .run_query(who, table_id, &DataQuery::default(), None)
            .await
            .is_err());
        return;
    };
    let mut keys: Vec<String> = table.schema.fields.iter().map(|f| f.key.clone()).collect();
    // 行: 見える列だけを SQL で射影できる（マスク列は「存在しない列」と同じ 422）。
    loop {
        let mut cols: Vec<&str> = vec!["_id"];
        cols.extend(keys.iter().map(String::as_str));
        let sql = data::render_sql(
            name,
            &DataQuery {
                limit: Some(200),
                ..DataQuery::default()
            },
            &cols,
        )
        .unwrap();
        let r = store.run_sql(who, &sql_req(&sql), None).await;
        match r {
            Err(DataError::Rejected(x)) if x.code == data::RejectCode::UnknownName => {
                let i = keys
                    .iter()
                    .position(|k| x.message.contains(&format!("'{k}'")))
                    .unwrap_or_else(|| panic!("{name}: {x:?}"));
                let masked = keys.remove(i);
                // IR でもその列の値は返らない（マスク）。
                for (_, m) in ir_rows(store, who, table_id, std::slice::from_ref(&masked)).await {
                    assert!(
                        !m.contains_key(&masked),
                        "{name}: マスク列 {masked} が IR で返った"
                    );
                }
            }
            Ok(_) => {
                let rows = sql_rows(store, who, &sql).await;
                let mut via_sql: Vec<(String, serde_json::Map<String, serde_json::Value>)> = rows
                    .into_iter()
                    .map(|row| {
                        let mut m = serde_json::Map::new();
                        for (k, v) in keys.iter().zip(row.iter().skip(1)) {
                            if !v.is_null() {
                                m.insert(k.clone(), v.clone());
                            }
                        }
                        (row[0].as_str().unwrap().to_string(), m)
                    })
                    .collect();
                let mut via_ir = ir_rows(store, who, table_id, &keys).await;
                via_sql.sort_by(|a, b| a.0.cmp(&b.0));
                via_ir.sort_by(|a, b| a.0.cmp(&b.0));
                assert_eq!(
                    via_sql, via_ir,
                    "{name}: SQL と IR で行・値が違う（{}）",
                    who.principal.id
                );
                break;
            }
            Err(e) => panic!("{name}: {sql}: {e:?}"),
        }
    }
    // 件数（集計抑制も同じ）。
    let ir = store
        .run_query(
            who,
            table_id,
            &DataQuery {
                aggregate: vec![data::MetricSpec {
                    metric: data::Metric::Count,
                    field: None,
                }],
                ..DataQuery::default()
            },
            None,
        )
        .await
        .unwrap();
    let sql = store
        .run_sql(
            who,
            &sql_req(&format!("SELECT COUNT(*) FROM {quoted}")),
            None,
        )
        .await
        .unwrap();
    assert_eq!(sql.suppressed, ir.suppressed, "{name}: 抑制の通知が違う");
    assert_eq!(
        sql.rows,
        vec![vec![ir.groups.unwrap()[0].value.clone()]],
        "{name}: 件数が違う"
    );
    // グループ化（select / text の索引済み列が 1 つあれば）。
    let Some(g) = table
        .schema
        .fields
        .iter()
        .find(|f| {
            f.indexed
                && keys.contains(&f.key)
                && matches!(
                    f.field_type,
                    FieldType::Select | FieldType::Text | FieldType::UserRef
                )
        })
        .map(|f| f.key.clone())
    else {
        return;
    };
    let ir = store
        .run_query(
            who,
            table_id,
            &DataQuery {
                group_by: vec![g.clone()],
                aggregate: vec![data::MetricSpec {
                    metric: data::Metric::Count,
                    field: None,
                }],
                ..DataQuery::default()
            },
            None,
        )
        .await
        .unwrap();
    let sql = store
        .run_sql(
            who,
            &sql_req(&format!(
                "SELECT \"{g}\", COUNT(*) FROM {quoted} GROUP BY \"{g}\""
            )),
            None,
        )
        .await
        .unwrap();
    let mut a: Vec<String> = sql.rows.iter().map(|r| format!("{r:?}")).collect();
    let mut b: Vec<String> = ir
        .groups
        .unwrap()
        .iter()
        .map(|grp| {
            format!(
                "{:?}",
                vec![
                    grp.key.get(&g).cloned().unwrap_or(serde_json::Value::Null),
                    grp.value.clone()
                ]
            )
        })
        .collect();
    a.sort();
    b.sort();
    assert_eq!(a, b, "{name}: グループ化の結果が違う");
    assert_eq!(sql.suppressed, ir.suppressed);
}

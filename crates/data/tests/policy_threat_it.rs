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
    clippy::too_many_lines,
    clippy::many_single_char_names
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

/// 届くはずのイベントを待つ上限（届けばすぐ抜ける）。カバレッジ計測のビルドは遅く、共有 DB に先行テストの
/// outbox が溜まっていると配信が数秒遅れるので、短すぎると誤って落ちる。
const SSE_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

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
        rag: None,
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

/// 強制戦略の回（CI の行列 `SHIKI_DATA_FORCE_STRATEGY=intersect|batch`）で、その戦略が実際に適用される
/// 形（別の列の等値 2 つの AND・単一キーのソート）でも行述語が効くこと（監査 B1-13）。
///
/// 強制戦略は適用できない形だと黙って自然な選択に戻るので、行列の回で空回りしていないことを
/// `stats.strategy`（検証用のデバッグ経路）で確かめる。環境変数が無い回も、交差駆動・バッチ駆動を
/// 明示に強制して同じ結果になることを確かめる。
#[tokio::test]
async fn intersect_and_batch_strategies_apply_and_respect_row_policy() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let f = fixture(pool, authz).await;
    let mut bob_drafts = vec![(200, f.bob_rec)];
    for (who, amount, status) in [
        (&f.bob, 300, "draft"),
        (&f.bob, 50, "draft"),
        (&f.bob, 999, "submitted"),
        (&f.alice, 500, "draft"),
    ] {
        let r = f
            .store
            .create_record(
                who,
                f.table_id,
                json!({"applicant": who.principal.id, "amount": amount, "status": status}),
                None,
            )
            .await
            .unwrap();
        if who.principal.id == "bob" && status == "draft" {
            bob_drafts.push((amount, r.id));
        }
    }
    bob_drafts.sort_by_key(|x| std::cmp::Reverse(x.0));
    let bob_expected: Vec<Uuid> = bob_drafts.iter().map(|x| x.1).collect();
    // 別の列（status・applicant）の等値 2 つの AND ＋ 単一キーのソート（交差駆動・バッチ駆動の形）。
    // alice が bob の申請で絞っても、行述語で bob の行は見えない（入口の戦略で迂回できない）。
    let q = DataQuery {
        filter: Some(data::Condition::All(vec![
            data::Condition::cmp("status", data::CondOp::Eq, Some(json!("draft"))),
            data::Condition::cmp("applicant", data::CondOp::Eq, Some(json!("bob"))),
        ])),
        sort: vec![SortKey {
            field: "amount".into(),
            descending: true,
        }],
        with_count: true,
        ..Default::default()
    };
    let ids = |r: &data::QueryResult| -> Vec<Uuid> {
        r.items.as_ref().unwrap().iter().map(|x| x.id).collect()
    };
    let env = data::QueryTuning::from_env();
    for (who, expected) in [(&f.alice, Vec::<Uuid>::new()), (&f.bob, bob_expected)] {
        // 既定の入口（CI の行列の回は環境変数の強制戦略で走る）。
        let r = f.store.run_query(who, f.table_id, &q, None).await.unwrap();
        assert_eq!(ids(&r), expected, "{}", who.principal.id);
        assert_eq!(
            r.count,
            Some(data::CountResult::Exact(expected.len() as u64)),
            "{}",
            who.principal.id
        );
        // 行列の強制戦略が交差駆動・バッチ駆動なら、実際にそれが使われたこと（自然な選択に戻っていない）。
        let want = match env.force {
            Some(data::ForcedStrategy::Intersect) => Some("intersect"),
            Some(data::ForcedStrategy::Batch) => Some("batch"),
            _ => None,
        };
        if let Some(want) = want {
            let (_, d) = f
                .store
                .run_query_debug(
                    who,
                    f.table_id,
                    &DataQuery {
                        with_count: false,
                        ..q.clone()
                    },
                    &env,
                )
                .await
                .unwrap();
            assert_eq!(
                d.stats.strategy, want,
                "強制戦略が適用されていない: {:?}",
                d.stats
            );
        }
        // 環境変数に依らず、交差駆動・バッチ駆動を明示に強制しても同じ行（行述語は省かれない）。
        for (force, label) in [
            (data::ForcedStrategy::Intersect, "intersect"),
            (data::ForcedStrategy::Batch, "batch"),
        ] {
            let tuning = data::QueryTuning {
                force: Some(force),
                ..Default::default()
            };
            let (r, d) = f
                .store
                .run_query_debug(
                    who,
                    f.table_id,
                    &DataQuery {
                        with_count: false,
                        ..q.clone()
                    },
                    &tuning,
                )
                .await
                .unwrap();
            assert_eq!(d.stats.strategy, label, "{:?}", d.stats);
            assert_eq!(ids(&r), expected, "{label}: {}", who.principal.id);
        }
    }
}

/// リンクの半結合（参照先の行述語つき）をバッチ駆動で評価しても、見えない参照先の行で参照元を
/// 釣り出せない（段 2 の一括検査でも参照先の行述語を閲覧者本人の権限で掛ける・監査 B1-13）。
#[tokio::test]
async fn batch_strategy_link_semijoin_respects_reference_row_policy() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let store = DataStore::new(pool, Arc::clone(&authz), Arc::new(AllowRefs));
    let mut person = field("person", FieldType::UserRef);
    person.indexed = true;
    let mut name = field("name", FieldType::Text);
    name.indexed = true;
    let mut lines_schema = plain(vec![person, name]);
    lines_schema.row_policy = Some(RowPolicy {
        read: PolicyExpr::FieldCmp {
            field: "person".into(),
            op: CmpOp::Eq,
            value: PolicyOperand::UserId,
        },
        write: None,
    });
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
    let mut no = field("no", FieldType::Number);
    no.indexed = true;
    let orders = store
        .create_table(
            &alice,
            NewDataTable {
                name: "orders".into(),
                schema: plain(vec![items_link(lines.id), no]),
            },
            None,
        )
        .await
        .unwrap();
    grant(&authz, &bob, Relation::Viewer, lines.id).await;
    grant(&authz, &bob, Relation::Editor, orders.id).await;
    let secret = store
        .create_record(
            &alice,
            lines.id,
            json!({"person": "alice", "name": "機密"}),
            None,
        )
        .await
        .unwrap();
    let mine = store
        .create_record(
            &alice,
            lines.id,
            json!({"person": "bob", "name": "機密"}),
            None,
        )
        .await
        .unwrap();
    let o1 = store
        .create_record(
            &alice,
            orders.id,
            json!({"items": ids(&[secret.id]), "no": 1}),
            None,
        )
        .await
        .unwrap();
    // 参照先の行は書き手に見えている必要がある（bob の行は bob がリンクする）。
    let o2 = store
        .create_record(
            &bob,
            orders.id,
            json!({"items": ids(&[mine.id]), "no": 2}),
            None,
        )
        .await
        .unwrap();
    let q = DataQuery {
        filter: Some(data::Condition::Link(data::LinkCond {
            field: "items".into(),
            filter: Some(Box::new(data::Condition::cmp(
                "name",
                data::CondOp::Eq,
                Some(json!("機密")),
            ))),
        })),
        sort: vec![SortKey {
            field: "no".into(),
            descending: false,
        }],
        ..Default::default()
    };
    let batch = data::QueryTuning {
        force: Some(data::ForcedStrategy::Batch),
        ..Default::default()
    };
    for (who, expected) in [(&alice, vec![o1.id]), (&bob, vec![o2.id])] {
        let (r, d) = store
            .run_query_debug(who, orders.id, &q, &batch)
            .await
            .unwrap();
        assert_eq!(d.stats.strategy, "batch", "{:?}", d.stats);
        let got: Vec<Uuid> = r.items.unwrap().iter().map(|x| x.id).collect();
        assert_eq!(got, expected, "{}", who.principal.id);
        // 既定の入口（行列の回は環境変数の強制戦略）でも同じ行。
        let r = store.run_query(who, orders.id, &q, None).await.unwrap();
        let got: Vec<Uuid> = r.items.unwrap().iter().map(|x| x.id).collect();
        assert_eq!(got, expected, "{}", who.principal.id);
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
        // 未宣言の列は読めない列と同じ 403 `field_unavailable`（列の存在オラクルを作らない）・
        // 未索引の列は 400。どちらも実行前に拒否する。
        if bad_field == "nope" {
            assert!(
                matches!(&r, Err(DataError::FieldUnavailable)),
                "{bad_field}: {r:?}"
            );
        } else {
            assert!(matches!(&r, Err(e) if e.is_invalid()), "{bad_field}: {r:?}");
        }
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
    assert!(matches!(&r, Err(e) if e.is_invalid()), "{r:?}");

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
                    rag: None,
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
                    rag: None,
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
    assert!(matches!(&probe_existing, Err(e) if e.is_invalid()));
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
        rag: None,
    }
}

fn items_link(to: Uuid) -> FieldDef {
    let mut f = field("items", FieldType::RecordRef);
    f.ref_table = Some(to);
    f.link = Some(LinkDef {
        cardinality: LinkCardinality::ManyToMany,
        symmetric_field: None,
        symmetric_key: None,
        symmetric_display_name: None,
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
    assert!(matches!(&r, Err(e) if e.is_invalid()), "{r:?}");
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
    // 原価（cost）は keiri ロールだけが読める。読めない列へは書けないので、原価を入れる alice は keiri。
    join_role(&authz, &alice, "keiri").await;
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
    // keiri の alice には原価の合計が見える。
    assert_eq!(alice_view.data["cost_total"], json!(61));
    // bob には非公開行が見えない → 合計・件数・lookup・数式から消える（null ではなく可視分だけ）。
    let bob_view = store.get_record(&bob, orders.id, o.id, None).await.unwrap();
    assert_eq!(bob_view.data["total"], json!(100));
    assert_eq!(bob_view.data["n"], json!(1));
    assert_eq!(bob_view.data["names"], json!(["公開"]));
    assert_eq!(bob_view.data["doubled"], json!(200));
    // keiri でない bob には、原価（参照先のマスク列）に依存するロールアップは列ごと出ない
    // （マスク列は値も「値なしで計算した結果」も持ち出さない・PIT-19）。
    assert!(
        bob_view.data.get("cost_total").is_none(),
        "{:?}",
        bob_view.data
    );
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
        matches!(&r, Err(e) if e.invalid_message().is_some_and(|m| m.contains("見つかりません"))),
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
            matches!(&r, Err(e) if e.is_invalid()),
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
    assert!(matches!(&r, Err(e) if e.is_invalid()), "{r:?}");
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
    assert!(matches!(&r, Err(e) if e.is_invalid()), "{r:?}");
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

/// ⑨-3b **参照先**のテーブル ReBAC を変えて包含が崩れたとき（参照先 lines から bob を外す）、
/// `after_table_rebac_changed(参照先)` が参照元 orders の材料化列も検査し直し、次の読取を待たずに
/// 材料化データを破棄する（PIT-71・監査 B1-5）。
#[tokio::test]
async fn materialized_rollup_is_discarded_when_reference_table_rebac_changes() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let m = materialized_fixture(pool, authz).await;
    // bob を両方の viewer にしても包含は保たれる（材料化のまま）。
    grant(&m.authz, &m.bob, Relation::Viewer, m.lines).await;
    m.store.after_table_rebac_changed(&m.alice, m.lines).await;
    grant(&m.authz, &m.bob, Relation::Viewer, m.orders).await;
    m.store.after_table_rebac_changed(&m.alice, m.orders).await;
    assert_eq!(m.materialized_rows().await, 2, "前提: 包含は保たれている");
    // 参照先から bob を外す → orders の viewer（alice, bob）⊄ lines の viewer（alice）。
    m.authz
        .delete_tuple(
            &m.bob.subject(),
            Relation::Viewer,
            &m.bob.ns().data_table(&m.lines.to_string()),
        )
        .await
        .expect("revoke");
    m.store.after_table_rebac_changed(&m.alice, m.lines).await;
    // 読取の入口を通る前に、参照元の材料化データ（索引行・依存登録）がもう無い。
    assert_eq!(
        m.materialized_rows().await,
        0,
        "参照先の ReBAC 変更で参照元の材料化データが残っている"
    );
    m.assert_discarded("参照先の ReBAC の変更").await;
    assert_eq!(m.total_seen_by(&m.bob).await, serde_json::Value::Null);
    assert_eq!(m.total_seen_by(&m.alice).await, json!(350));
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
    assert!(matches!(&r, Err(e) if e.is_invalid()), "{r:?}");
    m.assert_discarded("ReBAC の変更（クエリの入口）").await;

    let m = materialized_fixture(pool, authz).await;
    // alice は keiri（マスクの対象外）なので、降格の判定（ソート不可は 422）をそのまま見られる。
    join_role(&m.authz, &m.alice, "keiri").await;
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
    assert_eq!(m.total_seen_by(&m.alice).await, json!(350));
    // マスクされた列は集計の材料にもならない: keiri でない bob には合計の列ごと出ず、ソートは 403。
    grant(&m.authz, &m.bob, Relation::Viewer, m.orders).await;
    grant(&m.authz, &m.bob, Relation::Viewer, m.lines).await;
    let got = m
        .store
        .get_record(&m.bob, m.orders, m.order_id, None)
        .await
        .unwrap();
    assert!(got.data.get("total").is_none(), "{:?}", got.data);
    let r = m
        .store
        .run_query(&m.bob, m.orders, &m.by_total(), None)
        .await;
    assert!(matches!(r, Err(DataError::FieldUnavailable)), "{r:?}");
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
    assert!(matches!(&r, Err(e) if e.is_invalid()), "{r:?}");
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
            assert!(matches!(&r, Err(e) if e.is_invalid()), "{r:?}");
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
    // マスク列つきの表（amount は経理だけが読める）。読めない列へは書けないので、書き手の alice は経理。
    join_role(&f.authz, &f.alice, "keiri").await;
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
    let deadline = std::time::Instant::now() + SSE_WAIT;
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
    let deadline = std::time::Instant::now() + SSE_WAIT;
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
                // IR でもその列は投影できない（読めない列の投影は黙って落とさず 403・検証 C P3）。
                let r = store
                    .run_query(
                        who,
                        table_id,
                        &DataQuery {
                            select: Some(vec![masked.clone()]),
                            ..DataQuery::default()
                        },
                        None,
                    )
                    .await;
                assert!(
                    matches!(r, Err(DataError::FieldUnavailable)),
                    "{name}: マスク列 {masked} を IR で投影できた: {r:?}"
                );
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

// ---------------------------------------------------------------------------
// ⑫ 実操作検証の指摘（Phase 15・担当 B）: 書込面のマスク・リンク値の存在秘匿・派生列のマスク・
//    履歴・個別共有の宛先・取込の観測・オプティマイザの管理面・監査の語彙・参照先変更の配信
// ---------------------------------------------------------------------------

/// 同一テナントのディレクトリに居るユーザーだけを「居る」と答える参照解決（個別共有の宛先の検証用）。
struct DirectoryRefs(Vec<&'static str>);

#[async_trait]
impl RefResolver for DirectoryRefs {
    async fn user_exists(&self, _: &AuthContext, id: &str) -> Result<bool, String> {
        Ok(self.0.contains(&id))
    }
    async fn role_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
        Ok(true)
    }
    async fn file_readable(&self, _: &AuthContext, _: Uuid) -> Result<bool, String> {
        Ok(true)
    }
}

async fn join_role(authz: &Arc<dyn AuthzClient>, who: &AuthContext, role: &str) {
    authz
        .write_tuple(&who.subject(), Relation::Member, &who.ns().role(role))
        .await
        .expect("role member");
}

/// 社員番号（unique・indexed）は bmgr ロールだけが読める社員表。
fn staff_schema() -> TableSchema {
    let mut emp_no = field("emp_no", FieldType::Text);
    emp_no.unique = true;
    emp_no.indexed = true;
    let mut s = plain(vec![field("name", FieldType::Text), emp_no]);
    s.field_policy = vec![FieldPolicy {
        field: "emp_no".into(),
        readable_by: PolicyExpr::HasRole {
            role: "bmgr".into(),
            subtree: false,
        },
    }];
    s
}

/// ⑫-1（B-P0-1）読めない（マスクされた）列へは作成・一括取込（JSON・CSV）でも書けない。
/// 既存値の有無で応答が変わらない（unique 違反から値を当てさせない）。開始後にポリシーが変わっても、
/// 検証段の行エラーに列名・値を出さない。
#[tokio::test]
async fn masked_column_cannot_be_written_on_create_or_import() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let store = DataStore::new(pool, Arc::clone(&authz), Arc::new(AllowRefs));
    join_role(&authz, &alice, "bmgr").await;
    let t = store
        .create_table(
            &alice,
            NewDataTable {
                name: "staff".into(),
                schema: staff_schema(),
            },
            None,
        )
        .await
        .unwrap();
    grant(&authz, &bob, Relation::Editor, t.id).await;
    store
        .create_record(&alice, t.id, json!({"name": "a", "emp_no": "E103"}), None)
        .await
        .unwrap();
    // 既存の値でも無い値でも同じ 403（409 / 201 の差で既存値を当てられない）。
    for v in ["E103", "E999"] {
        let r = store
            .create_record(&bob, t.id, json!({"name": "b", "emp_no": v}), None)
            .await;
        assert!(matches!(r, Err(DataError::FieldUnavailable)), "{v}: {r:?}");
        let r = store
            .start_bulk_import(
                &bob,
                t.id,
                vec![data::BulkImportRow {
                    key: None,
                    data: json!({"name": "b", "emp_no": v}),
                }],
                data::BulkImportOptions::default(),
                None,
            )
            .await;
        assert!(matches!(r, Err(DataError::FieldUnavailable)), "{v}: {r:?}");
        let csv = format!("name,emp_no\nb,{v}\n");
        let r = store
            .start_bulk_import_csv(
                &bob,
                t.id,
                csv.as_bytes(),
                data::BulkImportOptions::default(),
                None,
            )
            .await;
        assert!(matches!(r, Err(DataError::FieldUnavailable)), "{v}: {r:?}");
    }
    // マスク列を含まない作成は通る（読める列だけの書込は妨げない）。
    store
        .create_record(&bob, t.id, json!({"name": "b"}), None)
        .await
        .unwrap();
    // 取込を始めた後で書き手が bmgr から外れた: 検証段で行ごとに拒否し、文言に列名・値を出さない。
    let imp = store
        .start_bulk_import(
            &alice,
            t.id,
            vec![data::BulkImportRow {
                key: Some("r1".into()),
                data: json!({"name": "c", "emp_no": "E103"}),
            }],
            data::BulkImportOptions::default(),
            None,
        )
        .await
        .unwrap();
    authz
        .delete_tuple(&alice.subject(), Relation::Member, &alice.ns().role("bmgr"))
        .await
        .unwrap();
    store.run_bulk_import(&tenant, t.id, imp).await.unwrap();
    let errs = store
        .bulk_import_errors(&alice, t.id, imp, 0, 10)
        .await
        .unwrap();
    assert_eq!(errs.len(), 1, "{errs:?}");
    for leak in ["emp_no", "E103", "重複"] {
        assert!(!errs[0].message.contains(leak), "{leak}: {errs:?}");
    }
}

/// 案件（担当者本人だけが見える）と、案件へのリンクを持つ親の表。
struct LinkFixture {
    store: DataStore,
    authz: Arc<dyn AuthzClient>,
    alice: AuthContext,
    bob: AuthContext,
    projects: Uuid,
    parent: Uuid,
    /// alice だけに見える案件。
    hidden: Vec<Uuid>,
    /// bob にも見える案件。
    shown: Vec<Uuid>,
    /// bob に権限の無い表の行。
    secret: Uuid,
    rec: Uuid,
}

async fn link_fixture(pool: PgPool, authz: Arc<dyn AuthzClient>) -> LinkFixture {
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let store = DataStore::new(pool, Arc::clone(&authz), Arc::new(AllowRefs));
    let mut person = field("person", FieldType::UserRef);
    person.indexed = true;
    let mut ps = plain(vec![person, field("name", FieldType::Text)]);
    // 作成者（alice）は全件、それ以外は担当の案件だけが見える。
    ps.row_policy = Some(RowPolicy {
        read: PolicyExpr::Any(vec![
            PolicyExpr::IsOwner,
            PolicyExpr::FieldCmp {
                field: "person".into(),
                op: CmpOp::Eq,
                value: PolicyOperand::UserId,
            },
        ]),
        write: None,
    });
    let projects = store
        .create_table(
            &alice,
            NewDataTable {
                name: "projects".into(),
                schema: ps,
            },
            None,
        )
        .await
        .unwrap();
    let secrets = store
        .create_table(
            &alice,
            NewDataTable {
                name: "secrets".into(),
                schema: plain(vec![field("name", FieldType::Text)]),
            },
            None,
        )
        .await
        .unwrap();
    let mut kids = items_link(projects.id);
    kids.key = "kids".into();
    kids.display_name = "kids".into();
    let mut lead = field("lead", FieldType::RecordRef);
    lead.ref_table = Some(projects.id);
    lead.link = Some(LinkDef {
        cardinality: LinkCardinality::ManyToOne,
        symmetric_field: None,
        symmetric_key: None,
        symmetric_display_name: None,
        reverse: false,
    });
    let mut other = items_link(secrets.id);
    other.key = "other".into();
    other.display_name = "other".into();
    let parent = store
        .create_table(
            &alice,
            NewDataTable {
                name: "parent".into(),
                schema: plain(vec![field("title", FieldType::Text), kids, lead, other]),
            },
            None,
        )
        .await
        .unwrap();
    grant(&authz, &bob, Relation::Viewer, projects.id).await;
    grant(&authz, &bob, Relation::Editor, parent.id).await;
    let mk = |who: &'static str, n: &'static str| {
        store.create_record(&alice, projects.id, json!({"person": who, "name": n}), None)
    };
    let a1 = mk("alice", "a1").await.unwrap().id;
    let b1 = mk("bob", "b1").await.unwrap().id;
    let a2 = mk("alice", "a2").await.unwrap().id;
    let b2 = mk("bob", "b2").await.unwrap().id;
    let a3 = mk("alice", "a3").await.unwrap().id;
    let secret = store
        .create_record(&alice, secrets.id, json!({"name": "s"}), None)
        .await
        .unwrap()
        .id;
    let rec = store
        .create_record(
            &alice,
            parent.id,
            json!({"title": "親", "kids": ids(&[a1, b1, a2, b2, a3]), "lead": ids(&[a1]),
                   "other": ids(&[secret])}),
            None,
        )
        .await
        .unwrap()
        .id;
    LinkFixture {
        store,
        authz,
        alice,
        bob,
        projects: projects.id,
        parent: parent.id,
        hidden: vec![a1, a2, a3],
        shown: vec![b1, b2],
        secret,
        rec,
    }
}

/// ⑫-2（B-P1-1）リンク列の値に、閲覧者に見えない参照先（行述語で不可視・権限の無い表）の id と件数が
/// 出ない。REST（get / query）・SQL・履歴・SSE の全面で同じ。
#[tokio::test]
async fn link_values_never_expose_invisible_targets() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let f = link_fixture(pool, authz).await;
    let shown = ids(&f.shown);
    let got = f
        .store
        .get_record(&f.bob, f.parent, f.rec, None)
        .await
        .unwrap();
    assert_eq!(got.data["kids"], shown, "見える参照先だけ（並び順は保つ）");
    assert_eq!(got.data["lead"], json!([]), "単一値でも見えなければ空");
    assert_eq!(got.data["other"], json!([]), "権限の無い表の行は出ない");
    // 参照先の表示名（タイトル列）も見える参照先の分だけ。
    let labels: Vec<(String, String)> = got.link_labels.clone().into_iter().collect();
    let mut want: Vec<(String, String)> = vec![
        (f.shown[0].to_string(), "b1".to_string()),
        (f.shown[1].to_string(), "b2".to_string()),
    ];
    want.sort();
    assert_eq!(labels, want);
    let whole = serde_json::to_string(&got).unwrap();
    let q = f
        .store
        .run_query(&f.bob, f.parent, &DataQuery::default(), None)
        .await
        .unwrap()
        .items
        .unwrap()
        .remove(0);
    assert_eq!(q.data["kids"], shown);
    assert_eq!(q.data["other"], json!([]));
    let rows = sql_rows(&f.store, &f.bob, "SELECT kids, other FROM parent").await;
    assert_eq!(rows, vec![vec![shown.clone(), json!([])]]);
    let revs = f
        .store
        .list_revisions(&f.bob, f.parent, f.rec, None, 50, None)
        .await
        .unwrap();
    let all = serde_json::to_string(&revs).unwrap();
    // 作成者（alice）には全部見える。
    let mine = f
        .store
        .get_record(&f.alice, f.parent, f.rec, None)
        .await
        .unwrap();
    assert_eq!(mine.data["kids"].as_array().unwrap().len(), 5);
    // SSE: bob の購読に届く更新にも見えない参照先は載らない。
    let mut stream = f
        .store
        .subscribe_records(&f.bob, f.parent, None, None)
        .await
        .unwrap();
    f.store
        .update_record(&f.alice, f.parent, f.rec, json!({"title": "更新"}), 1, None)
        .await
        .unwrap();
    let mut seen = Vec::new();
    let deadline = std::time::Instant::now() + SSE_WAIT;
    let mut got_up = false;
    while std::time::Instant::now() < deadline && !got_up {
        if let Ok(Some(m)) =
            tokio::time::timeout(std::time::Duration::from_millis(500), stream.next()).await
        {
            if let data::StreamEvent::Upserted(u) = &m.event {
                assert_eq!(u.record.data["kids"], shown);
                got_up = true;
            }
            seen.push(format!("{}", m.event.data_json()));
        }
    }
    assert!(got_up, "{seen:?}");
    let all = format!("{all}\n{whole}\n{}", seen.join("\n"));
    for leak in f.hidden.iter().chain([&f.secret]) {
        assert!(!all.contains(&leak.to_string()), "{leak} が漏れた: {all}");
    }
}

/// ⑫-3（B-P0-2・B-P1-2）書き手に見えない既存の参照先は、見える分だけの更新で消えない。関係ない列は
/// 見えない参照先を持つ行でも編集できる。検証するのは今回足した参照先だけで、見えない既存の id を
/// 送っても「見つからない」と同じ応答（オラクルなし）。単一値の見えない参照先は付け替えられない。
#[tokio::test]
async fn link_updates_keep_targets_the_writer_cannot_see() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let f = link_fixture(pool, authz).await;
    let (b1, b2) = (f.shown[0], f.shown[1]);
    let (a1, a2, a3) = (f.hidden[0], f.hidden[1], f.hidden[2]);
    // 関係ない列だけの更新（見えない参照先を持つ行でも 200）。
    let r = f
        .store
        .update_record(&f.bob, f.parent, f.rec, json!({"title": "bob"}), 1, None)
        .await
        .unwrap();
    assert_eq!(
        r.data["kids"],
        ids(&f.shown),
        "応答にも見えない参照先は出ない"
    );
    // 見えている配列をそのまま送り返す（UI の典型）→ 変化なし。
    let r = f
        .store
        .update_record(
            &f.bob,
            f.parent,
            f.rec,
            json!({"kids": ids(&f.shown)}),
            2,
            None,
        )
        .await
        .unwrap();
    assert_eq!(r.rev, 2, "見えない分を残すので差分なし");
    // 見える分から 1 件外す → 見えない 3 件は元の位置に残る。
    let r = f
        .store
        .update_record(
            &f.bob,
            f.parent,
            f.rec,
            json!({"kids": ids(&[b1])}),
            2,
            None,
        )
        .await
        .unwrap();
    assert_eq!(r.data["kids"], ids(&[b1]));
    let a = f
        .store
        .get_record(&f.alice, f.parent, f.rec, None)
        .await
        .unwrap();
    assert_eq!(a.data["kids"], ids(&[a1, b1, a2, a3]));
    assert_eq!(a.data["lead"], ids(&[a1]));
    assert_eq!(a.data["other"], ids(&[f.secret]));
    // 見えない既存の id を送る・存在しない id を送る → 同じ 400。
    let hidden_err = f
        .store
        .update_record(
            &f.bob,
            f.parent,
            f.rec,
            json!({"kids": ids(&[b1, a1])}),
            3,
            None,
        )
        .await
        .unwrap_err();
    let missing_err = f
        .store
        .update_record(
            &f.bob,
            f.parent,
            f.rec,
            json!({"kids": ids(&[b1, Uuid::new_v4()])}),
            3,
            None,
        )
        .await
        .unwrap_err();
    assert!(hidden_err.is_invalid(), "{hidden_err:?}");
    assert_eq!(format!("{hidden_err:?}"), format!("{missing_err:?}"));
    // 単一値の見えない参照先は付け替えられない（黙って外させない）。除去は見えない参照先を残すだけ。
    let r = f
        .store
        .update_record(
            &f.bob,
            f.parent,
            f.rec,
            json!({"lead": ids(&[b2])}),
            3,
            None,
        )
        .await;
    assert!(matches!(r, Err(DataError::Forbidden)), "{r:?}");
    let r = f
        .store
        .update_record(&f.bob, f.parent, f.rec, json!({"lead": null}), 3, None)
        .await
        .unwrap();
    assert_eq!(r.rev, 3, "見えない参照先は残る＝差分なし");
    // 一括 upsert の更新行も同じ入口（見えない参照先を残す）。
    let imp = f
        .store
        .start_bulk_upsert(
            &f.bob,
            f.parent,
            vec![data::BulkUpsertRow {
                key: None,
                record_id: Some(f.rec),
                expected_rev: None,
                data: json!({"kids": ids(&[b2])}),
            }],
            data::BulkImportOptions::default(),
            None,
        )
        .await
        .unwrap();
    f.store
        .run_bulk_import(&f.bob.tenant_id, f.parent, imp)
        .await
        .unwrap();
    let a = f
        .store
        .get_record(&f.alice, f.parent, f.rec, None)
        .await
        .unwrap();
    assert_eq!(a.data["kids"], ids(&[a1, b2, a2, a3]));
    // 参照先の行が見えるようになれば、その id は bob にも出る（絞り込みは読取時・保存値は保たれる）。
    f.store
        .update_record(&f.alice, f.projects, a1, json!({"person": "bob"}), 1, None)
        .await
        .unwrap();
    let b = f
        .store
        .get_record(&f.bob, f.parent, f.rec, None)
        .await
        .unwrap();
    assert_eq!(b.data["kids"], ids(&[a1, b2]));
    let _ = &f.authz;
}

/// ⑫-4（B-P2-4・B-P3-1）読めない列に依存する派生列（数式・参照先の読めない列の lookup）は、その閲覧者には
/// 読めない列と同じ扱い（応答に出ない・検索に使えない・閲覧者文脈で隠す列に入る）。履歴は読めない列の
/// 変更記録を返さず、読めない列だけが変わった版は版ごと返さない。
#[tokio::test]
async fn derived_columns_over_masked_fields_are_masked_and_revisions_hide_masked_changes() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let store = DataStore::new(pool, Arc::clone(&authz), Arc::new(AllowRefs));
    join_role(&authz, &alice, "keiri").await;
    let mut bonus = field("bonus", FieldType::Formula);
    bonus.formula =
        Some(serde_json::from_value(json!({"mul": [{"field": "salary"}, {"lit": 2}]})).unwrap());
    let mut label = field("label", FieldType::Formula);
    label.formula = Some(serde_json::from_value(json!({"upper": {"field": "name"}})).unwrap());
    let mut es = plain(vec![
        field("name", FieldType::Text),
        field("salary", FieldType::Number),
        bonus,
        label,
    ]);
    es.aggregate_min_rows = Some(3);
    es.field_policy = vec![FieldPolicy {
        field: "salary".into(),
        readable_by: PolicyExpr::HasRole {
            role: "keiri".into(),
            subtree: false,
        },
    }];
    let emp = store
        .create_table(
            &alice,
            NewDataTable {
                name: "emp".into(),
                schema: es,
            },
            None,
        )
        .await
        .unwrap();
    let mut link = items_link(emp.id);
    link.key = "who".into();
    link.display_name = "who".into();
    let mut sal = field("sal", FieldType::Lookup);
    sal.lookup = Some(LookupDef {
        via_field: "who".into(),
        target_field: "salary".into(),
    });
    let mut names = field("names", FieldType::Lookup);
    names.lookup = Some(LookupDef {
        via_field: "who".into(),
        target_field: "name".into(),
    });
    // 社員表の K（3）を継承するロールアップ（見える社員が 1 人なのでスモールセル抑制に掛かる・PIT-17）。
    let mut cnt = field("cnt", FieldType::Rollup);
    cnt.rollup = Some(RollupDef {
        link: "who".into(),
        target: None,
        op: RollupOp::Count,
    });
    let teams = store
        .create_table(
            &alice,
            NewDataTable {
                name: "teams".into(),
                schema: plain(vec![link, sal, names, cnt]),
            },
            None,
        )
        .await
        .unwrap();
    for t in [emp.id, teams.id] {
        grant(&authz, &bob, Relation::Viewer, t).await;
    }
    let emp_rec = store
        .create_record(&alice, emp.id, json!({"name": "e", "salary": 100}), None)
        .await
        .unwrap();
    let tm = store
        .create_record(&alice, teams.id, json!({"who": ids(&[emp_rec.id])}), None)
        .await
        .unwrap();
    let alice_emp = store
        .get_record(&alice, emp.id, emp_rec.id, None)
        .await
        .unwrap();
    assert_eq!(alice_emp.data["bonus"], json!(200));
    let bob_emp = store
        .get_record(&bob, emp.id, emp_rec.id, None)
        .await
        .unwrap();
    let keys: Vec<&String> = bob_emp.data.as_object().unwrap().keys().collect();
    assert!(bob_emp.data.get("salary").is_none(), "{keys:?}");
    assert!(
        bob_emp.data.get("bonus").is_none(),
        "読めない列の数式は列ごと出ない: {keys:?}"
    );
    assert_eq!(bob_emp.data["label"], json!("E"), "関係ない数式は出る");
    let bt = store.get_record(&bob, teams.id, tm.id, None).await.unwrap();
    assert!(
        bt.data.get("sal").is_none(),
        "参照先の読めない列の lookup も出ない"
    );
    assert_eq!(bt.data["names"], json!(["e"]));
    // 抑制した値は null のまま「抑制した」ことだけを示す（画面は空欄でなく「件数が少ないため非表示」）。
    assert_eq!(bt.data["cnt"], serde_json::Value::Null);
    assert_eq!(bt.suppressed_fields, vec!["cnt".to_string()]);
    let at = store
        .get_record(&alice, teams.id, tm.id, None)
        .await
        .unwrap();
    assert_eq!(
        at.suppressed_fields,
        vec!["cnt".to_string()],
        "全件見える人にも同じ規則"
    );
    assert!(
        serde_json::to_value(&alice_emp)
            .unwrap()
            .get("suppressed_fields")
            .is_none(),
        "抑制が無ければ応答に出ない"
    );
    // 読めない列に依存する派生列（bonus）は bob のテーブル定義に出ない。閲覧者文脈はスキーマ管理者以外に
    // マスク列名を返さない（検証 2-B-P2-1）。
    let vc = store
        .table_viewer_context(&bob, emp.id, None)
        .await
        .unwrap();
    assert!(vc.masked_fields.is_empty(), "{vc:?}");
    let seen = store
        .get_table_for_viewer(&bob, emp.id, None)
        .await
        .unwrap();
    assert!(seen.schema.field("bonus").is_none(), "{:?}", seen.schema);
    let by_bonus = DataQuery {
        sort: vec![SortKey {
            field: "bonus".into(),
            descending: true,
        }],
        ..DataQuery::default()
    };
    let r = store.run_query(&bob, emp.id, &by_bonus, None).await;
    assert!(matches!(r, Err(DataError::FieldUnavailable)), "{r:?}");
    // 履歴: 読めない列だけの更新（rev 2）は bob に返らない。混ざった更新（rev 3）は読める列だけ。
    store
        .update_record(&alice, emp.id, emp_rec.id, json!({"salary": 300}), 1, None)
        .await
        .unwrap();
    store
        .update_record(
            &alice,
            emp.id,
            emp_rec.id,
            json!({"salary": 400, "name": "f"}),
            2,
            None,
        )
        .await
        .unwrap();
    let revs = store
        .list_revisions(&bob, emp.id, emp_rec.id, None, 50, None)
        .await
        .unwrap();
    let got: Vec<(i64, Vec<String>)> = revs
        .iter()
        .map(|r| (r.rev, r.patch.iter().map(|p| p.field.clone()).collect()))
        .collect();
    assert_eq!(
        got,
        vec![(3, vec!["name".to_string()]), (1, vec!["name".to_string()])]
    );
    // ページの大きさ 1 でも間引いた版で途切れない（続きを引いて埋める）。
    let page = store
        .list_revisions(&bob, emp.id, emp_rec.id, Some(3), 1, None)
        .await
        .unwrap();
    assert_eq!(page.iter().map(|r| r.rev).collect::<Vec<_>>(), vec![1]);
    let alice_revs = store
        .list_revisions(&alice, emp.id, emp_rec.id, None, 50, None)
        .await
        .unwrap();
    assert_eq!(alice_revs.len(), 3);
}

/// ⑫-5（B-P3-3・B-P3-4・B-P3-6・B-P2-2）個別共有の宛先はディレクトリに居るユーザーだけ。取込の状態・
/// 行エラーは作成者・テーブル owner だけ（他の editor には「見つからない」）。組織管理者でもテーブルの
/// viewer でなければオプティマイザの報告は「見つからない」。レコードの書込の監査は `data_record` で残る。
#[tokio::test]
async fn share_targets_import_observers_optimizer_admin_and_audit_vocabulary() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let carol = ctx(&tenant, "carol");
    let store = DataStore::new(
        pool.clone(),
        Arc::clone(&authz),
        Arc::new(DirectoryRefs(vec!["alice", "bob", "carol"])),
    );
    let t = store
        .create_table(
            &alice,
            NewDataTable {
                name: "notes".into(),
                schema: plain(vec![field("name", FieldType::Text)]),
            },
            None,
        )
        .await
        .unwrap();
    let rec = store
        .create_record(&alice, t.id, json!({"name": "n"}), None)
        .await
        .unwrap();
    // 個別共有: 居ないユーザー・他テナント形の id は 400、居るユーザーは通る。
    for bad in ["ghost", "x|y"] {
        let r = store
            .share_record(
                &alice,
                t.id,
                rec.id,
                &storage::ShareTarget::User { id: bad.into() },
                data::RecordShareRole::Viewer,
                None,
            )
            .await;
        assert!(matches!(&r, Err(e) if e.is_invalid()), "{bad}: {r:?}");
    }
    store
        .share_record(
            &alice,
            t.id,
            rec.id,
            &storage::ShareTarget::User { id: "bob".into() },
            data::RecordShareRole::Viewer,
            None,
        )
        .await
        .unwrap();
    // 取込の観測: bob の取込は bob と owner（alice）だけが見られる。carol（editor）には存在ごと見えない。
    grant(&authz, &bob, Relation::Editor, t.id).await;
    grant(&authz, &carol, Relation::Editor, t.id).await;
    let imp = store
        .start_bulk_import(
            &bob,
            t.id,
            vec![data::BulkImportRow {
                key: None,
                data: json!({"name": 1}),
            }],
            data::BulkImportOptions::default(),
            None,
        )
        .await
        .unwrap();
    store.run_bulk_import(&tenant, t.id, imp).await.unwrap();
    for who in [&bob, &alice] {
        store.bulk_import_status(who, t.id, imp).await.unwrap();
        store
            .bulk_import_errors(who, t.id, imp, 0, 10)
            .await
            .unwrap();
    }
    let st = store.bulk_import_status(&carol, t.id, imp).await;
    assert!(matches!(st, Err(DataError::NotFound)), "{st:?}");
    let er = store.bulk_import_errors(&carol, t.id, imp, 0, 10).await;
    assert!(matches!(er, Err(DataError::NotFound)), "{er:?}");
    // オプティマイザの管理面: 組織管理者（dave）でもテーブルの viewer でなければ 404。
    let dave = ctx(&tenant, "dave");
    authz
        .write_tuple(
            &dave.subject(),
            Relation::Admin,
            &dave.ns().organization(&dave.org),
        )
        .await
        .unwrap();
    let r = store.table_optimizer_report(&dave, t.id).await;
    assert!(matches!(r, Err(DataError::NotFound)), "{r:?}");
    let r = store.set_table_fixed_plan(&dave, t.id, true, None).await;
    assert!(matches!(r, Err(DataError::NotFound)), "{r:?}");
    grant(&authz, &dave, Relation::Viewer, t.id).await;
    store.table_optimizer_report(&dave, t.id).await.unwrap();
    // 組織管理者はテーブルの editor であれば他人の取込も見られる。
    grant(&authz, &dave, Relation::Editor, t.id).await;
    store.bulk_import_status(&dave, t.id, imp).await.unwrap();
    // 監査の語彙: レコードの作成・更新・削除は data_record（object_id はレコードの外部 ID）。
    let r2 = store
        .update_record(&alice, t.id, rec.id, json!({"name": "m"}), 1, None)
        .await
        .unwrap();
    store
        .delete_record(&alice, t.id, rec.id, r2.rev, None)
        .await
        .unwrap();
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT action, object_type, object_id FROM audit_log \
         WHERE tenant_id = $1 AND action IN ('data.record.create', 'data.record.update', \
           'data.record.delete') AND decision = 'allow' ORDER BY id",
    )
    .bind(&tenant)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 3, "{rows:?}");
    for (action, ty, id) in rows {
        assert_eq!(ty, "data_record", "{action}");
        assert_eq!(id, rec.id.to_string(), "{action}");
    }
}

/// ⑫-6（B-P2-1）参照先テーブルに行述語を後付けすると、参照元テーブルの購読に `stream.reset` が届く
/// （開いている画面が古いルックアップ値・リンク値のまま残らない）。
#[tokio::test]
async fn reference_policy_change_resets_referrer_subscriptions() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let f = link_fixture(pool, authz).await;
    let mut stream = f
        .store
        .subscribe_records(&f.bob, f.parent, None, None)
        .await
        .unwrap();
    let projects = f.store.get_table(&f.alice, f.projects, None).await.unwrap();
    let mut s = projects.schema.clone();
    s.row_policy = None;
    f.store
        .update_table_schema(&f.alice, f.projects, s, None, None)
        .await
        .unwrap();
    let mut seen = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    let mut reset = false;
    while std::time::Instant::now() < deadline && !reset {
        match tokio::time::timeout(std::time::Duration::from_millis(500), stream.next()).await {
            Ok(Some(m)) => {
                if let data::StreamEvent::Reset(r) = &m.event {
                    assert_eq!(r.reason, data::StreamResetReason::Refresh);
                    reset = true;
                }
                seen.push(format!("{} {}", m.event.name(), m.event.data_json()));
            }
            Ok(None) => break,
            Err(_) => {}
        }
    }
    assert!(reset, "参照元の購読に stream.reset が届く: {seen:?}");
}

/// ⑬（C-P2-4・C-P2-5・C P3）構造化した 400 の文言・テーブル定義の応答・投影の拒否が、読めない（マスクされた）
/// 列の名前・ポリシーの定義（ロール名）を漏らさない。
/// - 必須の読めない列の欠落は `required_missing` だが、列名（`field`）も文言も出さない
/// - スキーマ管理者（owner）以外の `get_table_for_viewer` / `list_tables_for_viewer` は読める列だけを返し、
///   `field_policy`・`row_policy` を返さない（owner には全体）
/// - 直接の要求の投影（`select`）に読めない列を入れると 403（黙って落とさない・SQL 面と揃える）
#[tokio::test]
async fn structured_errors_and_table_definitions_never_reveal_masked_columns() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let carol = ctx(&tenant, "carol");
    let store = DataStore::new(pool, Arc::clone(&authz), Arc::new(AllowRefs));
    join_role(&authz, &alice, "bmgr").await;
    join_role(&authz, &carol, "bmgr").await;
    let mut schema = staff_schema();
    let mut code = field("secret_code", FieldType::Text);
    code.required = true;
    schema.fields.push(code);
    schema.field_policy.push(FieldPolicy {
        field: "secret_code".into(),
        readable_by: PolicyExpr::HasRole {
            role: "bmgr".into(),
            subtree: false,
        },
    });
    // 行述語が参照する列は indexed 必須。
    schema.fields[0].indexed = true;
    schema.row_policy = Some(RowPolicy {
        read: PolicyExpr::Any(vec![
            PolicyExpr::HasRole {
                role: "bmgr".into(),
                subtree: false,
            },
            PolicyExpr::FieldCmp {
                field: "name".into(),
                op: CmpOp::Ne,
                value: PolicyOperand::Lit(json!("hidden-row")),
            },
        ]),
        write: None,
    });
    let t = store
        .create_table(
            &alice,
            NewDataTable {
                name: "staff".into(),
                schema,
            },
            None,
        )
        .await
        .unwrap();
    grant(&authz, &bob, Relation::Editor, t.id).await;
    grant(&authz, &carol, Relation::Editor, t.id).await;

    // 必須の読めない列の欠落: コードは出すが、列名は出さない。
    let err = store
        .create_record(&bob, t.id, json!({"name": "b"}), None)
        .await
        .unwrap_err();
    let DataError::InvalidInput(ie) = &err else {
        panic!("{err:?}");
    };
    assert_eq!(ie.code(), data::InvalidCode::RequiredMissing, "{err:?}");
    assert_eq!(ie.field_key(), None, "読めない列の名前を返さない: {err:?}");
    assert!(!ie.message().contains("secret_code"), "{err:?}");
    // 読める必須列の欠落なら列名を返す（書き手は読める）。
    let err = store
        .create_record(&carol, t.id, json!({"name": "c"}), None)
        .await
        .unwrap_err();
    assert_eq!(err.invalid_code(), Some(data::InvalidCode::RequiredMissing));
    let DataError::InvalidInput(ie) = &err else {
        panic!("{err:?}");
    };
    assert_eq!(ie.field_key(), Some("secret_code"));

    // テーブル定義: bob には読める列だけ・ポリシーの定義なし。
    let seen = store.get_table_for_viewer(&bob, t.id, None).await.unwrap();
    let keys: Vec<&str> = seen.schema.fields.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(keys, vec!["name"], "{keys:?}");
    assert!(seen.schema.field_policy.is_empty());
    assert!(seen.schema.row_policy.is_none());
    let text = serde_json::to_string(&seen).unwrap();
    for leak in ["emp_no", "secret_code", "bmgr", "hidden-row"] {
        assert!(!text.contains(leak), "{leak}: {text}");
    }
    let listed = store.list_tables_for_viewer(&bob, 50).await.unwrap();
    let text = serde_json::to_string(&listed).unwrap();
    for leak in ["emp_no", "secret_code", "bmgr", "hidden-row"] {
        assert!(!text.contains(leak), "一覧 {leak}: {text}");
    }
    // carol（bmgr・owner ではない）は列は見えるが、ポリシーの定義は見えない。
    let seen = store
        .get_table_for_viewer(&carol, t.id, None)
        .await
        .unwrap();
    assert_eq!(seen.schema.fields.len(), 3);
    assert!(seen.schema.field_policy.is_empty() && seen.schema.row_policy.is_none());
    // owner（スキーマ管理者）には全体。
    let full = store
        .get_table_for_viewer(&alice, t.id, None)
        .await
        .unwrap();
    assert_eq!(full.schema.field_policy.len(), 2);
    assert!(full.schema.row_policy.is_some());

    // 投影に読めない列を入れると 403（黙って落とさない）。読める列だけなら通る。
    let q = |sel: &[&str]| DataQuery {
        select: Some(sel.iter().map(|s| (*s).to_string()).collect()),
        ..DataQuery::default()
    };
    let r = store
        .run_query(&bob, t.id, &q(&["name", "emp_no"]), None)
        .await;
    assert!(matches!(r, Err(DataError::FieldUnavailable)), "{r:?}");
    store
        .run_query(&bob, t.id, &q(&["name"]), None)
        .await
        .unwrap();
    store
        .run_query(&carol, t.id, &q(&["name", "emp_no"]), None)
        .await
        .unwrap();
}

/// ⑭（検証 2-B-P2-1 / 2-C-P2-2）列の存在オラクル: 存在しない列と読めない（マスクされた）列は、
/// 絞り込み・並べ替え・投影・検索・集計・作成・更新・取込（JSON・CSV の参照名と表示名）・索引の昇格・
/// カーソル位置の通知の**すべてで同一の応答**（同じ 403 `field_unavailable`・列名なし）。閲覧者文脈は
/// マスク列名を返さず、テーブル定義の表示順は詰めて返す。マスク列への書込の拒否は監査に deny で残る。
#[tokio::test]
async fn unknown_and_masked_columns_are_indistinguishable_everywhere() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let store = DataStore::new(pool.clone(), Arc::clone(&authz), Arc::new(AllowRefs));
    join_role(&authz, &alice, "bmgr").await;
    let mut schema = staff_schema();
    let mut dept = field("dept", FieldType::Text);
    dept.indexed = true;
    schema.fields.push(dept);
    for f in &mut schema.fields {
        if f.key == "emp_no" {
            f.display_name = "社員番号".into();
            f.searchable = true;
        }
    }
    let t = store
        .create_table(
            &alice,
            NewDataTable {
                name: "staff-oracle".into(),
                schema,
            },
            None,
        )
        .await
        .unwrap();
    grant(&authz, &bob, Relation::Editor, t.id).await;
    let rec = store
        .create_record(&alice, t.id, json!({"name": "a", "emp_no": "E1"}), None)
        .await
        .unwrap();

    // 各経路で「マスク列」と「存在しない列」を指定し、応答の形が同じであることを確かめる。
    let same = |what: &str, masked: String, unknown: String| {
        assert!(masked.contains("FieldUnavailable"), "{what}: {masked}");
        assert_eq!(
            masked, unknown,
            "{what}: マスク列と存在しない列で応答が違う"
        );
    };
    for (what, q) in [
        (
            "filter",
            json!({"filter": {"cmp": {"field": "$K", "op": "eq", "value": "E1"}}}),
        ),
        ("sort", json!({"sort": [{"field": "$K"}]})),
        ("select", json!({"select": ["name", "$K"]})),
        ("search", json!({"search": "E", "search_fields": ["$K"]})),
        (
            "contains",
            json!({"filter": {"cmp": {"field": "$K", "op": "contains", "value": "E"}}}),
        ),
        (
            "group_by",
            json!({"group_by": ["$K"], "aggregate": [{"metric": "count"}]}),
        ),
    ] {
        let run = |k: &str| {
            let q: DataQuery = serde_json::from_value(
                serde_json::from_str(&q.to_string().replace("$K", k)).unwrap(),
            )
            .unwrap();
            let store = &store;
            let bob = &bob;
            async move { format!("{:?}", store.run_query(bob, t.id, &q, None).await.err()) }
        };
        same(what, run("emp_no").await, run("zzz_nope").await);
    }
    let create = |k: &str| {
        let store = &store;
        let bob = &bob;
        let k = k.to_string();
        async move {
            format!(
                "{:?}",
                store
                    .create_record(bob, t.id, json!({"name": "b", k: "E9"}), None)
                    .await
                    .err()
            )
        }
    };
    same("create", create("emp_no").await, create("zzz_nope").await);
    let update = |k: &str| {
        let store = &store;
        let bob = &bob;
        let k = k.to_string();
        async move {
            format!(
                "{:?}",
                store
                    .update_record(bob, t.id, rec.id, json!({ k: "E9" }), 1, None)
                    .await
                    .err()
            )
        }
    };
    same("update", update("emp_no").await, update("zzz_nope").await);
    let import = |k: &str| {
        let store = &store;
        let bob = &bob;
        let k = k.to_string();
        async move {
            format!(
                "{:?}",
                store
                    .start_bulk_import(
                        bob,
                        t.id,
                        vec![data::BulkImportRow {
                            key: None,
                            data: json!({"name": "b", k: "E9"}),
                        }],
                        data::BulkImportOptions::default(),
                        None,
                    )
                    .await
                    .err()
            )
        }
    };
    same("import", import("emp_no").await, import("zzz_nope").await);
    let csv = |h: &str| {
        let store = &store;
        let bob = &bob;
        let body = format!("name,{h}\nb,E9\n");
        async move {
            format!(
                "{:?}",
                store
                    .start_bulk_import_csv(
                        bob,
                        t.id,
                        body.as_bytes(),
                        data::BulkImportOptions::default(),
                        None,
                    )
                    .await
                    .err()
            )
        }
    };
    same("csv key", csv("emp_no").await, csv("zzz_nope").await);
    same(
        "csv display",
        csv("社員番号").await,
        csv("存在しない列").await,
    );
    let promote = |k: &str| {
        let store = &store;
        let bob = &bob;
        let k = k.to_string();
        async move {
            format!(
                "{:?}",
                store.promote_index(bob, t.id, &k, false, None).await.err()
            )
        }
    };
    same(
        "promote",
        promote("emp_no").await,
        promote("zzz_nope").await,
    );
    let aware = |k: &str| {
        let store = &store;
        let bob = &bob;
        let k = k.to_string();
        async move {
            format!(
                "{:?}",
                store
                    .post_awareness(bob, t.id, Some(rec.id), Some(k), None)
                    .await
                    .err()
            )
        }
    };
    same("awareness", aware("emp_no").await, aware("zzz_nope").await);

    // 閲覧者文脈はマスク列名を返さない（スキーマ管理者には返す）。
    let vc = store.table_viewer_context(&bob, t.id, None).await.unwrap();
    assert!(vc.masked_fields.is_empty(), "{vc:?}");
    // テーブル定義の表示順は詰める（読めない列の位置に欠番を作らない）。
    let seen = store.get_table_for_viewer(&bob, t.id, None).await.unwrap();
    let orders: Vec<(&str, i32)> = seen
        .schema
        .fields
        .iter()
        .map(|f| (f.key.as_str(), f.order))
        .collect();
    assert_eq!(orders, vec![("name", 0), ("dept", 1)], "{orders:?}");

    // マスク列への書込の拒否は監査に deny で残る（列名は残さない・検証 2-B-P3-4）。
    let denies: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT metadata FROM audit_log WHERE tenant_id = $1 AND decision = 'deny' \
         AND metadata ->> 'reason' = 'field_unavailable'",
    )
    .bind(&tenant)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(denies.len() >= 4, "{denies:?}");
    let text = serde_json::to_string(&denies).unwrap();
    assert!(
        !text.contains("emp_no") && !text.contains("zzz_nope"),
        "{text}"
    );
}

/// ⑮（2 回目の検証・web の依頼）対称列（逆リンク）にも参照元の表示名（`link_labels`）を返す。
/// 返すのは閲覧者に見える参照元だけ（順方向のリンクと同じ規則・権限の無い表の行は値にも名前にも出ない）。
/// 対称列は作成時に表示名を付けられる。
#[tokio::test]
async fn reverse_link_labels_only_for_visible_referrers() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let store = DataStore::new(pool, Arc::clone(&authz), Arc::new(AllowRefs));
    let projects = store
        .create_table(
            &alice,
            NewDataTable {
                name: "projects-rev".into(),
                schema: plain(vec![field("name", FieldType::Text)]),
            },
            None,
        )
        .await
        .unwrap();
    let parent = |name: &'static str, sym: &'static str, label: Option<&'static str>| {
        let mut kids = items_link(projects.id);
        kids.key = "kids".into();
        if let Some(l) = kids.link.as_mut() {
            l.symmetric_key = Some(sym.into());
            l.symmetric_display_name = label.map(str::to_string);
        }
        let store = &store;
        let alice = &alice;
        async move {
            store
                .create_table(
                    alice,
                    NewDataTable {
                        name: name.into(),
                        schema: plain(vec![field("title", FieldType::Text), kids]),
                    },
                    None,
                )
                .await
                .unwrap()
        }
    };
    let open = parent("open-rev", "open_parents", Some("公開の親（kids）")).await;
    let closed = parent("closed-rev", "closed_parents", None).await;
    grant(&authz, &bob, Relation::Viewer, projects.id).await;
    grant(&authz, &bob, Relation::Viewer, open.id).await;
    let p1 = store
        .create_record(&alice, projects.id, json!({"name": "p1"}), None)
        .await
        .unwrap()
        .id;
    let shown = store
        .create_record(
            &alice,
            open.id,
            json!({"title": "公開", "kids": ids(&[p1])}),
            None,
        )
        .await
        .unwrap()
        .id;
    store
        .create_record(
            &alice,
            closed.id,
            json!({"title": "秘密", "kids": ids(&[p1])}),
            None,
        )
        .await
        .unwrap();
    // 対称列の表示名は作成時に付けたもの（省略は参照名）。
    let ps = store.get_table(&alice, projects.id, None).await.unwrap();
    assert_eq!(
        ps.schema.field("open_parents").unwrap().display_name,
        "公開の親（kids）"
    );
    assert_eq!(
        ps.schema.field("closed_parents").unwrap().display_name,
        "closed_parents"
    );
    // bob: 見える参照元の値と表示名だけ。
    let got = store.get_record(&bob, projects.id, p1, None).await.unwrap();
    assert_eq!(got.data["open_parents"], ids(&[shown]));
    assert_eq!(got.data["closed_parents"], json!([]));
    assert_eq!(
        got.link_labels.get(&shown.to_string()).map(String::as_str),
        Some("公開")
    );
    let text = serde_json::to_string(&got).unwrap();
    assert!(!text.contains("秘密"), "{text}");
    // alice（両方見える）には両方の表示名。
    let mine = store
        .get_record(&alice, projects.id, p1, None)
        .await
        .unwrap();
    let names: Vec<&str> = mine.link_labels.values().map(String::as_str).collect();
    assert!(
        names.contains(&"公開") && names.contains(&"秘密"),
        "{names:?}"
    );
}

// ---------------------------------------------------------------------------
// ⑮ 独立レビュー（データ読取）の確定指摘の回帰
// ---------------------------------------------------------------------------

/// ⑮-1（R1-A・R1-B・PIT-17）結合を残した集計の小セル抑制は、組の数ではなく **FROM の表のレコードの個数**で
/// 判定し（多値リンクで 1 レコードを K 件以上の組に増やしても素通りしない）、結合先の列を集計するなら
/// **結合先の表の `aggregate_min_rows`** も結合先のレコードの個数で判定する（ロールアップと同じ継承）。
#[tokio::test]
async fn joined_aggregate_small_cells_count_records_and_inherit_join_k() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let store = DataStore::new(pool, Arc::clone(&authz), Arc::new(AllowRefs));
    let mut amount = field("amount", FieldType::Number);
    amount.indexed = true;
    let mut lines_schema = plain(vec![field("name", FieldType::Text), amount]);
    lines_schema.aggregate_min_rows = Some(3);
    let lines = store
        .create_table(
            &alice,
            NewDataTable {
                name: "klines".into(),
                schema: lines_schema,
            },
            None,
        )
        .await
        .unwrap();
    let mut grp = field("grp", FieldType::Select);
    grp.options = vec!["a".into(), "b".into(), "c".into()];
    grp.indexed = true;
    let mut orders_schema = plain(vec![items_link(lines.id), grp]);
    orders_schema.aggregate_min_rows = Some(2);
    let orders = store
        .create_table(
            &alice,
            NewDataTable {
                name: "korders".into(),
                schema: orders_schema,
            },
            None,
        )
        .await
        .unwrap();
    let mut l = Vec::new();
    for n in ["l1", "l2", "l3"] {
        l.push(
            store
                .create_record(&alice, lines.id, json!({"name": n, "amount": 10}), None)
                .await
                .unwrap()
                .id,
        );
    }
    // a: 1 レコード × 3 明細（組は 3 件・レコードは 1 件 < 2）。
    // b: 2 レコード × 同じ 2 明細（組は 4 件・レコード 2 件・明細 2 件 < 3）。
    // c: 2 レコード × 3 明細（組は 6 件・レコード 2 件・明細 3 件）。
    for (g, items) in [
        ("a", vec![l[0], l[1], l[2]]),
        ("b", vec![l[0], l[1]]),
        ("b", vec![l[0], l[1]]),
        ("c", vec![l[0], l[1], l[2]]),
        ("c", vec![l[0], l[1], l[2]]),
    ] {
        store
            .create_record(
                &alice,
                orders.id,
                json!({"grp": g, "items": ids(&items)}),
                None,
            )
            .await
            .unwrap();
    }
    let run = |q: serde_json::Value| {
        let store = &store;
        let alice = &alice;
        async move {
            let q: DataQuery = serde_json::from_value(q).unwrap();
            let r = store.run_query(alice, orders.id, &q, None).await.unwrap();
            let mut out: Vec<(String, Vec<serde_json::Value>)> = r
                .groups
                .unwrap()
                .into_iter()
                .map(|g| (g.key["grp"].as_str().unwrap().to_string(), g.values))
                .collect();
            out.sort_by(|x, y| x.0.cmp(&y.0));
            (out, r.suppressed)
        }
    };
    let null2 = vec![serde_json::Value::Null, serde_json::Value::Null];
    // 結合先の列を集計する: a は FROM のレコード不足、b は結合先のレコード不足で抑制。
    let (got, suppressed) = run(json!({
        "joins": [{"alias": "l", "field": "items"}],
        "group_by": ["grp"],
        "aggregate": [{"metric": "count"}, {"metric": "sum", "field": "l.amount"}],
    }))
    .await;
    assert!(suppressed);
    assert_eq!(
        got,
        vec![
            ("a".to_string(), null2.clone()),
            ("b".to_string(), null2.clone()),
            ("c".to_string(), vec![json!(6), json!(60.0)]),
        ],
        "組の数ではなくレコードの個数で抑制し、結合先の K も効くこと"
    );
    // 結合先の列を集計しない（件数だけ）: FROM の表の K だけ（b は 2 レコードなので出る）。
    let (got, _) = run(json!({
        "joins": [{"alias": "l", "field": "items"}],
        "group_by": ["grp"],
        "aggregate": [{"metric": "count"}],
    }))
    .await;
    assert_eq!(
        got,
        vec![
            ("a".to_string(), vec![serde_json::Value::Null]),
            ("b".to_string(), vec![json!(4)]),
            ("c".to_string(), vec![json!(6)]),
        ]
    );
    // SQL 前段面も同じ経路（a の 1 レコードは組が 3 件でも抑制）。
    let rows = sql_rows(
        &store,
        &alice,
        "SELECT o.grp, COUNT(*) FROM korders o JOIN klines l ON o.items = l._id \
         WHERE o.grp = 'a' GROUP BY o.grp",
    )
    .await;
    assert_eq!(rows, vec![vec![json!("a"), serde_json::Value::Null]]);
}

/// ⑮-2（R1-C）マスクされたリンク列を半結合・結合・入れ子の半結合で名指ししても、参照先を開かずに
/// 存在しない列と**同じ** 403 `field_unavailable`（参照先が読めないときの本文なし 403 と区別できない
/// ＝マスク列の存在オラクルにならない・data-platform §16.1）。
#[tokio::test]
async fn masked_link_columns_are_indistinguishable_from_unknown_in_links_and_joins() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let store = DataStore::new(pool, Arc::clone(&authz), Arc::new(AllowRefs));
    join_role(&authz, &alice, "keiri").await;
    let keiri_only = |key: &str| FieldPolicy {
        field: key.into(),
        readable_by: PolicyExpr::HasRole {
            role: "keiri".into(),
            subtree: false,
        },
    };
    // bob が viewer でない参照先（ここを開こうとすると 403 になる）。
    let secret = store
        .create_table(
            &alice,
            NewDataTable {
                name: "secret".into(),
                schema: plain(vec![field("name", FieldType::Text)]),
            },
            None,
        )
        .await
        .unwrap();
    let mut sub = field("sub", FieldType::RecordRef);
    sub.ref_table = Some(secret.id);
    sub.link = items_link(secret.id).link;
    let mut mid_schema = plain(vec![field("name", FieldType::Text), sub]);
    mid_schema.field_policy = vec![keiri_only("sub")];
    let mid = store
        .create_table(
            &alice,
            NewDataTable {
                name: "mid".into(),
                schema: mid_schema,
            },
            None,
        )
        .await
        .unwrap();
    let mut hidden = field("hidden", FieldType::RecordRef);
    hidden.ref_table = Some(secret.id);
    hidden.link = items_link(secret.id).link;
    let mut base_schema = plain(vec![
        field("name", FieldType::Text),
        items_link(mid.id),
        hidden,
    ]);
    base_schema.field_policy = vec![keiri_only("hidden")];
    let base = store
        .create_table(
            &alice,
            NewDataTable {
                name: "base".into(),
                schema: base_schema,
            },
            None,
        )
        .await
        .unwrap();
    grant(&authz, &bob, Relation::Viewer, base.id).await;
    grant(&authz, &bob, Relation::Viewer, mid.id).await;
    store
        .create_record(&alice, base.id, json!({"name": "x"}), None)
        .await
        .unwrap();
    for (what, q) in [
        ("link", json!({"filter": {"link": {"field": "$K"}}})),
        (
            "join",
            json!({"joins": [{"alias": "s", "field": "$K"}], "select": ["name"]}),
        ),
        (
            "nested link",
            json!({"filter": {"link": {"field": "items", "filter": {"link": {"field": "$S"}}}}}),
        ),
        (
            "nested join",
            json!({"joins": [{"alias": "m", "field": "items"},
                             {"alias": "s", "from": "m", "field": "$S"}],
                   "select": ["name"]}),
        ),
    ] {
        let run = |k: &str, s: &str| {
            let q: DataQuery = serde_json::from_value(
                serde_json::from_str(&q.to_string().replace("$K", k).replace("$S", s)).unwrap(),
            )
            .unwrap();
            let store = &store;
            let bob = &bob;
            async move { format!("{:?}", store.run_query(bob, base.id, &q, None).await.err()) }
        };
        let masked = run("hidden", "sub").await;
        let unknown = run("zzz_nope", "zzz_nope").await;
        assert!(masked.contains("FieldUnavailable"), "{what}: {masked}");
        assert_eq!(
            masked, unknown,
            "{what}: マスク列と存在しない列で応答が違う"
        );
    }
}

/// ⑮-3（R1-D）SQL のテーブル名の解決は、見えない同名異表記（`sales` と `Sales`）の有無で結果が変わらない
/// （候補すべての viewer を先に確かめ、見える候補の中で完全一致 → 一意の順に選ぶ）。
#[tokio::test]
async fn sql_table_names_ignore_invisible_case_variants() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let store = DataStore::new(pool.clone(), Arc::clone(&authz), Arc::new(AllowRefs));
    let shown = store
        .create_table(
            &alice,
            NewDataTable {
                name: "Sales".into(),
                schema: plain(vec![field("name", FieldType::Text)]),
            },
            None,
        )
        .await
        .unwrap();
    grant(&authz, &bob, Relation::Viewer, shown.id).await;
    store
        .create_record(&alice, shown.id, json!({"name": "visible"}), None)
        .await
        .unwrap();
    let before = sales_answers(&store, &bob).await;
    assert!(before[0].starts_with("ok"), "{before:?}");
    assert!(before[4].starts_with("err"), "{before:?}");
    // bob に見えない同名異表記を作っても、bob の解決結果は 1 文字も変わらない。
    let invisible = store
        .create_table(
            &alice,
            NewDataTable {
                name: "sales".into(),
                schema: plain(vec![field("name", FieldType::Text)]),
            },
            None,
        )
        .await
        .unwrap();
    store
        .create_record(&alice, invisible.id, json!({"name": "secret"}), None)
        .await
        .unwrap();
    let after = sales_answers(&store, &bob).await;
    assert_eq!(before, after, "見えない同名異表記で解決結果が変わった");
    // alice（両方見える）は完全一致で選ぶ。
    assert_eq!(
        sql_rows(&store, &alice, "SELECT name FROM sales").await,
        vec![vec![json!("secret")]]
    );
    assert_eq!(
        sql_rows(&store, &alice, "SELECT name FROM Sales").await,
        vec![vec![json!("visible")]]
    );
    // どちらも見えない carol: 応答は存在しない表と同じだが、見えない表を名指しした試行は監査に拒否で残る。
    let carol = ctx(&tenant, "carol");
    assert!(store
        .run_sql(&carol, &sql_req("SELECT name FROM sales"), None)
        .await
        .is_err());
    let denied: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND decision = 'deny' \
         AND action = 'data.query.sql' AND object_id = $2",
    )
    .bind(&tenant)
    .bind(invisible.id.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(denied >= 1, "見えない表の名指しが監査に残らない");
}

/// ⑮-4（R1-E）材料化した計算列は本体に値が無いので、スナップショット読取（本体の SQL で評価）で絞り込むと
/// 黙って 0 件になる。誤った結果を返さず 400 `snapshot_unsupported`。
#[tokio::test]
async fn snapshot_read_rejects_filters_on_materialized_derived_columns() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let m = materialized_fixture(pool, authz).await;
    let filter = json!({"cmp": {"field": "total", "op": "gte", "value": 0}});
    let live: DataQuery = serde_json::from_value(json!({"filter": filter})).unwrap();
    let r = m
        .store
        .run_query(&m.alice, m.orders, &live, None)
        .await
        .unwrap();
    assert_eq!(
        r.items.unwrap().len(),
        1,
        "前提: ライブ読取では 1 件に当たる"
    );
    let snap: DataQuery =
        serde_json::from_value(json!({"filter": filter, "snapshot": true})).unwrap();
    let r = m.store.run_query(&m.alice, m.orders, &snap, None).await;
    assert_eq!(
        r.as_ref().err().and_then(DataError::invalid_code),
        Some(data::InvalidCode::SnapshotUnsupported),
        "{r:?}"
    );
}

/// ⑮-3 の読取（bob の目に見える解決結果を文字列で並べる）。
async fn sales_answers(store: &DataStore, bob: &AuthContext) -> Vec<String> {
    let mut out = Vec::new();
    for sql in [
        "SELECT name FROM sales",
        "SELECT name FROM SALES",
        "SELECT name FROM Sales",
        "SELECT name FROM \"Sales\"",
        "SELECT name FROM \"sales\"",
    ] {
        let r = store.run_sql(bob, &sql_req(sql), None).await;
        out.push(match r {
            Ok(r) => format!("ok {:?}", r.rows),
            Err(e) => format!("err {e:?}"),
        });
    }
    out
}

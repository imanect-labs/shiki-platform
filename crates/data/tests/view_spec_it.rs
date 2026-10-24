//! 型付き ViewSpec の結合テスト（Task 15.5・data-platform §8.2）。実 Postgres ＋ 実 OpenFGA。
//!
//! 受け入れ条件:
//! - 列の表示名変更・並べ替え・幅変更・非表示がビューに保存され、他ユーザーに影響しない
//! - マスク列や `per_viewer` 列を不正に指定した ViewSpec が保存時に拒否される
//! - ルックアップ・ロールアップ列が 13 本以上のビューが保存時に拒否される（テナント設定で変えられる）
//! - 保存ビューを別ユーザーが開くと、そのユーザーの権限で行が絞られる（作成者の権限を引き継がない）

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
use authz::{AuthContext, AuthzClient, Principal, Relation};
use data::{
    CmpOp, ColumnConfig, DataError, DataQuery, DataStore, DataViewBody, DataViewPage,
    DataViewStore, FieldDef, FieldPolicy, FieldType, GridView, GroupConfig, LinkCardinality,
    LinkDef, LookupDef, NewDataTable, PolicyExpr, PolicyOperand, RefResolver, RollupDef, RollupOp,
    RowPolicy, SortKey, TableSchema, ViewSpec,
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

struct Env {
    pool: PgPool,
    authz: Arc<dyn AuthzClient>,
    store: DataStore,
    views: DataViewStore,
}

async fn setup() -> Option<Env> {
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
        .expect("pg connect");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    let model: serde_json::Value =
        serde_json::from_str(include_str!("../../authz/model/authorization-model.json")).unwrap();
    let fga = OpenFgaClient::connect(
        reqwest::Client::new(),
        &OpenFgaConfig {
            base_url,
            store_name: format!("shiki-data-viewspec-test-{}", Uuid::new_v4()),
        },
        &model,
    )
    .await
    .expect("fga connect");
    let authz: Arc<dyn AuthzClient> = Arc::new(fga);
    let store = DataStore::new(pool.clone(), Arc::clone(&authz), Arc::new(AllowRefs));
    let artifacts = Arc::new(artifact::ArtifactStore::new(
        pool.clone(),
        Arc::clone(&authz),
    ));
    let views = DataViewStore::new(artifacts, store.clone());
    Some(Env {
        pool,
        authz,
        store,
        views,
    })
}

fn ctx(tenant: &str, user: &str, roles: &[&str]) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: user.into(),
            email: None,
            groups: vec![],
            roles: roles.iter().map(|r| (*r).to_string()).collect(),
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        tenant.into(),
    )
}

fn field(key: &str, ty: FieldType) -> FieldDef {
    FieldDef::new(key, ty)
}

fn base_schema(fields: Vec<FieldDef>) -> TableSchema {
    TableSchema {
        fields,
        status_field: None,
        row_policy: None,
        field_policy: vec![],
        aggregate_min_rows: None,
        fsm_ref: None,
        rag: None,
    }
}

fn col(key: &str) -> ColumnConfig {
    ColumnConfig {
        field: key.into(),
        label: None,
        width: None,
        hidden: false,
        frozen: false,
        sort_by: None,
    }
}

fn grid(columns: Vec<ColumnConfig>) -> ViewSpec {
    ViewSpec::Grid(GridView {
        columns,
        ..GridView::default()
    })
}

fn body(table: Uuid, display: ViewSpec) -> DataViewBody {
    DataViewBody {
        table_id: table,
        query: DataQuery::default(),
        display,
    }
}

async fn share_table(
    env: &Env,
    owner: &AuthContext,
    who: &AuthContext,
    table: Uuid,
    rel: Relation,
) {
    env.authz
        .write_tuple(
            &who.subject(),
            rel,
            &owner.ns().data_table(&table.to_string()),
        )
        .await
        .expect("share table");
}

/// 担当者（owner_ref）本人だけが読める行・経理ロールだけが読める salary・per_viewer のロールアップを持つ表。
/// 作成者（alice）は経理ロール（OpenFGA の role member）。
async fn people_table(env: &Env, alice: &AuthContext) -> Uuid {
    env.authz
        .write_tuple(
            &alice.subject(),
            Relation::Member,
            &alice.ns().role("keiri"),
        )
        .await
        .expect("keiri member");
    let teams = env
        .store
        .create_table(
            alice,
            NewDataTable {
                name: "teams".into(),
                schema: base_schema(vec![field("title", FieldType::Text)]),
            },
            None,
        )
        .await
        .expect("teams");
    let mut team = field("team", FieldType::RecordRef);
    team.ref_table = Some(teams.id);
    team.link = Some(LinkDef {
        cardinality: LinkCardinality::ManyToMany,
        symmetric_field: None,
        symmetric_key: None,
        reverse: false,
    });
    let mut team_count = field("team_count", FieldType::Rollup);
    team_count.rollup = Some(RollupDef {
        link: "team".into(),
        target: None,
        op: RollupOp::Count,
    });
    let mut salary = field("salary", FieldType::Number);
    salary.indexed = true;
    let schema = TableSchema {
        status_field: None,
        aggregate_min_rows: None,
        fsm_ref: None,
        rag: None,
        fields: vec![
            FieldDef {
                indexed: true,
                ..field("owner_ref", FieldType::UserRef)
            },
            field("name", FieldType::Text),
            FieldDef {
                indexed: true,
                ..field("kana", FieldType::Text)
            },
            salary,
            team,
            team_count,
        ],
        row_policy: Some(RowPolicy {
            read: PolicyExpr::FieldCmp {
                field: "owner_ref".into(),
                op: CmpOp::Eq,
                value: PolicyOperand::UserId,
            },
            write: None,
        }),
        field_policy: vec![FieldPolicy {
            field: "salary".into(),
            readable_by: PolicyExpr::HasRole {
                role: "keiri".into(),
                subtree: true,
            },
        }],
    };
    let t = env
        .store
        .create_table(
            alice,
            NewDataTable {
                name: "people".into(),
                schema,
            },
            None,
        )
        .await
        .expect("people");
    for (who, name) in [("alice", "あ"), ("bob", "い"), ("bob", "う")] {
        env.store
            .create_record(
                alice,
                t.id,
                json!({"owner_ref": who, "name": name, "kana": name, "salary": 100}),
                None,
            )
            .await
            .expect("row");
    }
    t.id
}

/// 受け入れ条件: マスク列・per_viewer 列・存在しない列を不正に指定した ViewSpec は保存時に拒否される。
#[tokio::test]
async fn invalid_view_specs_are_rejected_on_save() {
    let Some(env) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let keiri = ctx(&tenant, "alice", &["keiri"]);
    let bob = ctx(&tenant, "bob", &[]);
    let table = people_table(&env, &keiri).await;
    share_table(&env, &keiri, &bob, table, Relation::Viewer).await;

    // 経理（salary を読める）は salary を表示できる。
    env.views
        .create(
            &keiri,
            "経理用",
            &body(table, grid(vec![col("salary")])),
            None,
        )
        .await
        .expect("keiri can show salary");
    // bob（salary はマスク）が salary を表示・代替並べ替え・グループ化に使う設定は 403。
    let r = env
        .views
        .create(&bob, "b1", &body(table, grid(vec![col("salary")])), None)
        .await;
    assert!(matches!(r, Err(DataError::FieldUnavailable)), "{r:?}");
    let mut alt = col("name");
    alt.sort_by = Some("salary".into());
    let r = env
        .views
        .create(&bob, "b2", &body(table, grid(vec![alt])), None)
        .await;
    assert!(matches!(r, Err(DataError::FieldUnavailable)), "{r:?}");
    // 非表示にしたマスク列の設定は何も見せないので保存できる（他人の設定を写しても止めない）。
    let mut hidden = col("salary");
    hidden.hidden = true;
    env.views
        .create(&bob, "b3", &body(table, grid(vec![hidden])), None)
        .await
        .expect("hidden masked column is fine");

    // per_viewer のロールアップで並べ替え・グループ化は拒否（クエリの sort も同じ）。
    let mut alt = col("name");
    alt.sort_by = Some("team_count".into());
    let r = env
        .views
        .create(&keiri, "p1", &body(table, grid(vec![alt])), None)
        .await;
    assert!(
        matches!(r, Err(DataError::Rejected(ref r)) if r.message.contains("閲覧者ごと")),
        "{r:?}"
    );
    let spec = ViewSpec::Grid(GridView {
        group_by: vec![GroupConfig {
            field: "team_count".into(),
            descending: false,
            collapsed: false,
        }],
        ..GridView::default()
    });
    let r = env
        .views
        .create(&keiri, "p2", &body(table, spec), None)
        .await;
    assert!(matches!(r, Err(DataError::Rejected(_))), "{r:?}");
    let mut b = body(table, ViewSpec::default());
    b.query.sort = vec![SortKey {
        field: "team_count".into(),
        descending: true,
    }];
    let r = env.views.create(&keiri, "p3", &b, None).await;
    assert!(
        matches!(r, Err(DataError::Rejected(ref r)) if r.message.contains("閲覧者ごと")),
        "{r:?}"
    );
    // 表示するだけなら per_viewer 列も出せる。
    env.views
        .create(
            &keiri,
            "p4",
            &body(table, grid(vec![col("team_count")])),
            None,
        )
        .await
        .expect("per_viewer column can be displayed");

    // 存在しない列は拒否（読めない列と同じ 403 `field_unavailable`・列の存在オラクルを作らない）。
    // 更新（新しい版）でも同じ検証が掛かる。
    let r = env
        .views
        .create(&keiri, "x1", &body(table, grid(vec![col("nope")])), None)
        .await;
    assert!(matches!(r, Err(DataError::FieldUnavailable)), "{r:?}");
    let id = env
        .views
        .create(&keiri, "x2", &body(table, ViewSpec::default()), None)
        .await
        .unwrap();
    let r = env
        .views
        .update(
            &keiri,
            id,
            &body(table, grid(vec![col("nope")])),
            Some(1),
            None,
        )
        .await;
    assert!(matches!(r, Err(DataError::FieldUnavailable)), "{r:?}");
}

/// 受け入れ条件: ルックアップ・ロールアップ列が 13 本以上のビューは保存時に拒否される（既定 12）。
/// 非表示の列は数えない。テナント設定で上限を変えられる。
#[tokio::test]
async fn lookup_and_rollup_columns_are_capped_per_view() {
    let Some(env) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice", &[]);
    let customers = env
        .store
        .create_table(
            &alice,
            NewDataTable {
                name: "customers".into(),
                schema: base_schema(vec![
                    field("name", FieldType::Text),
                    field("city", FieldType::Text),
                ]),
            },
            None,
        )
        .await
        .unwrap();
    let mut cust = field("cust", FieldType::RecordRef);
    cust.ref_table = Some(customers.id);
    let mut fields = vec![field("title", FieldType::Text), cust];
    // ルックアップ 7 本 ＋ ロールアップ 6 本 = 13 本。
    for i in 0..7 {
        let mut lk = field(&format!("lk{i}"), FieldType::Lookup);
        lk.lookup = Some(LookupDef {
            via_field: "cust".into(),
            target_field: if i % 2 == 0 { "name" } else { "city" }.into(),
        });
        fields.push(lk);
    }
    for i in 0..6 {
        let mut ru = field(&format!("ru{i}"), FieldType::Rollup);
        ru.rollup = Some(RollupDef {
            link: "cust".into(),
            target: None,
            op: RollupOp::Count,
        });
        fields.push(ru);
    }
    let orders = env
        .store
        .create_table(
            &alice,
            NewDataTable {
                name: "orders".into(),
                schema: base_schema(fields),
            },
            None,
        )
        .await
        .unwrap();

    // 既定の表（全列表示）は 13 本 → 拒否。
    let r = env
        .views
        .create(&alice, "all", &body(orders.id, ViewSpec::default()), None)
        .await;
    match &r {
        Err(DataError::Rejected(rej)) => {
            assert_eq!(rej.code, data::RejectCode::LimitExceeded);
            assert!(rej.message.contains("12 本まで"), "{rej:?}");
            let limit = rej.limit.as_ref().unwrap();
            assert_eq!(
                (limit.name.as_str(), limit.max),
                ("max_view_lookup_columns", 12)
            );
        }
        other => panic!("13 本は 422: {other:?}"),
    }
    // 1 本を非表示にすれば 12 本で保存できる。
    let mut hidden = col("ru5");
    hidden.hidden = true;
    let id = env
        .views
        .create(&alice, "12", &body(orders.id, grid(vec![hidden])), None)
        .await
        .expect("12 lookup columns are fine");
    // 非表示の列は実行時にも解決しない（投影に入らない）。
    env.store
        .create_record(&alice, orders.id, json!({"title": "o"}), None)
        .await
        .unwrap();
    let run = env.views.run(&alice, id, None, None).await.unwrap();
    let rec = &run.items.unwrap()[0];
    assert!(rec.data.get("ru5").is_none(), "{:?}", rec.data);
    assert_eq!(rec.data["title"], "o");
    // 投影を明示しても数える。
    let mut b = body(orders.id, ViewSpec::default());
    b.query.select = Some(
        (0..7)
            .map(|i| format!("lk{i}"))
            .chain((0..6).map(|i| format!("ru{i}")))
            .collect(),
    );
    let r = env.views.create(&alice, "sel", &b, None).await;
    assert!(matches!(r, Err(DataError::Rejected(_))), "{r:?}");
    // テナント設定で上限を 13 にすれば保存できる（範囲外は拒否）。
    assert!(env
        .store
        .update_tenant_view_lookup_cap(&alice, Some(0), None)
        .await
        .is_err());
    let s = env
        .store
        .update_tenant_view_lookup_cap(&alice, Some(13), None)
        .await
        .unwrap();
    assert_eq!(s.effective_max_view_lookup_columns, 13);
    env.views
        .create(&alice, "all13", &body(orders.id, ViewSpec::default()), None)
        .await
        .expect("cap raised");
    let s = env
        .store
        .update_tenant_view_lookup_cap(&alice, None, None)
        .await
        .unwrap();
    assert_eq!(s.max_view_lookup_columns, None);
    assert_eq!(s.effective_max_view_lookup_columns, 12);
    // 変更は監査に残る。
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE tenant_id = $1 \
           AND action = 'data.tenant_settings.update' AND metadata ? 'max_view_lookup_columns'",
    )
    .bind(&tenant)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(audited, 2);
}

/// 受け入れ条件: 列の表示名変更・並べ替え・幅・非表示はビューに保存され、他ユーザーのビューに影響しない。
/// 保存ビューを別ユーザーが開くと、そのユーザーの権限で行・列が絞られる。
#[tokio::test]
async fn views_are_per_user_and_reevaluated_for_each_viewer() {
    let Some(env) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice", &["keiri"]);
    let bob = ctx(&tenant, "bob", &[]);
    let table = people_table(&env, &alice).await;
    share_table(&env, &alice, &bob, table, Relation::Viewer).await;

    // bob の個人ビュー。
    let mut bob_name = col("name");
    bob_name.width = Some(300);
    let bob_view = env
        .views
        .create(
            &bob,
            "bob の",
            &body(table, grid(vec![bob_name.clone()])),
            None,
        )
        .await
        .unwrap();
    // alice のビュー: 表示名変更・並べ替え（kana を先頭）・幅・非表示・読み仮名で並べ替え。
    let mut name = col("name");
    name.label = Some("氏名".into());
    name.width = Some(180);
    name.sort_by = Some("kana".into());
    let mut kana = col("kana");
    kana.hidden = true;
    let spec = grid(vec![col("salary"), name, kana]);
    let alice_view = env
        .views
        .create(&alice, "alice の", &body(table, spec.clone()), None)
        .await
        .unwrap();
    let (v, got) = env.views.get(&alice, alice_view, None, None).await.unwrap();
    assert_eq!(v, 1);
    assert_eq!(got.display, spec);
    // alice が自分のビューを更新しても bob のビューは変わらない（別の artifact・不変の版）。
    let mut spec2 = spec.clone();
    if let ViewSpec::Grid(g) = &mut spec2 {
        g.columns.reverse();
        g.columns[0].width = Some(90);
    }
    let v2 = env
        .views
        .update(
            &alice,
            alice_view,
            &body(table, spec2.clone()),
            Some(1),
            None,
        )
        .await
        .unwrap();
    assert_eq!(v2, 2);
    let (_, b) = env.views.get(&bob, bob_view, None, None).await.unwrap();
    assert_eq!(b.display, grid(vec![bob_name]));
    // bob は alice のビューを（共有されるまで）読めず、書けない。
    assert!(env.views.get(&bob, alice_view, None, None).await.is_err());

    // ビューの一覧: 本人のものだけ。共有されると載る。
    let list = env.views.list_for_table(&bob, table, None).await.unwrap();
    assert_eq!(
        list.iter().map(|v| v.id).collect::<Vec<_>>(),
        vec![bob_view]
    );
    env.authz
        .write_tuple(
            &bob.subject(),
            Relation::Viewer,
            &alice.ns().artifact(&alice_view.to_string()),
        )
        .await
        .unwrap();
    let list = env.views.list_for_table(&bob, table, None).await.unwrap();
    assert_eq!(list.len(), 2);
    assert!(list.iter().any(|v| v.id == alice_view && v.version == 2));
    // 共有されても bob は更新できない（viewer）。
    let r = env
        .views
        .update(&bob, alice_view, &body(table, spec.clone()), Some(2), None)
        .await;
    assert!(r.is_err(), "{r:?}");

    // alice が開くと alice の行だけ・salary が見える。
    let a = env
        .views
        .run_pinned(&alice, alice_view, 2, &DataViewPage::default(), None)
        .await
        .unwrap();
    let a_items = a.result.items.unwrap();
    assert_eq!(a_items.len(), 1);
    assert_eq!(a_items[0].data["owner_ref"], "alice");
    assert_eq!(a_items[0].data["salary"], json!(100));
    assert_eq!(a.display, spec2);
    // bob が同じビューを開くと bob の行だけ・salary はマスクされる（作成者の権限を引き継がない）。
    let b = env
        .views
        .run_pinned(&bob, alice_view, 2, &DataViewPage::default(), None)
        .await
        .unwrap();
    // 表示設定にも bob に読めない列の名前を残さない（検証 2-B-P2-1）。
    let shown = serde_json::to_string(&b.display).unwrap();
    assert!(!shown.contains("salary"), "{shown}");
    let b_items = b.result.items.unwrap();
    assert_eq!(b_items.len(), 2);
    assert!(b_items.iter().all(|r| r.data["owner_ref"] == "bob"));
    assert!(b_items.iter().all(|r| r.data.get("salary").is_none()));

    // テーブルの viewer でない人には一覧も出さない。
    let carol = ctx(&tenant, "carol", &[]);
    assert!(matches!(
        env.views.list_for_table(&carol, table, None).await,
        Err(DataError::NotFound)
    ));
}

/// 閲覧者の文脈（読めない列・編集/スキーマ変更の可否）は閲覧者本人の権限で決まる。
#[tokio::test]
async fn viewer_context_reflects_the_viewer() {
    let Some(env) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice", &["keiri"]);
    let bob = ctx(&tenant, "bob", &[]);
    let table = people_table(&env, &alice).await;
    let a = env
        .store
        .table_viewer_context(&alice, table, None)
        .await
        .unwrap();
    assert!(a.masked_fields.is_empty());
    assert!(a.can_edit && a.can_manage);
    assert_eq!(a.max_view_lookup_columns, 12);
    assert!(matches!(
        env.store.table_viewer_context(&bob, table, None).await,
        Err(DataError::NotFound)
    ));
    share_table(&env, &alice, &bob, table, Relation::Editor).await;
    let b = env
        .store
        .table_viewer_context(&bob, table, None)
        .await
        .unwrap();
    // スキーマ管理者以外にはマスク列名を返さない（列の存在オラクルを作らない・検証 2-B-P2-1）。
    assert!(b.masked_fields.is_empty(), "{b:?}");
    assert!(b.can_edit);
    assert!(!b.can_manage);
}

/// v1 の保存ビュー（display がサーバ非解釈の JSON）は既定の表として読め、実行できる（上位互換）。
#[tokio::test]
async fn legacy_saved_views_still_run() {
    let Some(env) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice", &["keiri"]);
    let table = people_table(&env, &alice).await;
    let artifacts = artifact::ArtifactStore::new(env.pool.clone(), Arc::clone(&env.authz));
    let legacy = artifacts
        .create(
            &alice,
            artifact::NewArtifact {
                kind: artifact::ArtifactKind::DataView,
                name: "v1".into(),
                body: json!({
                    "table_id": table,
                    "query": {"sort": {"field": "kana", "descending": true}},
                    "display": {"kind": "list", "columns": ["name"]},
                }),
            },
            None,
        )
        .await
        .unwrap();
    let (_, b) = env.views.get(&alice, legacy.id, None, None).await.unwrap();
    assert_eq!(b.display, ViewSpec::default());
    let run = env.views.run(&alice, legacy.id, None, None).await.unwrap();
    assert_eq!(run.items.unwrap().len(), 1);
}

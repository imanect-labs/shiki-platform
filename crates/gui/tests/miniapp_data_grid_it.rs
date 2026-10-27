//! ミニアプリの画面の表（```data_grid）のデータ束縛の結合テスト（Task 15.7 受け入れ条件）。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）＋実 OpenFGA（`OPENFGA_TEST_URL`）で次を確かめる。
//!
//! - 表はデータ束縛（[`gui::ActionBinding::DataView`]）で描かれ、**閲覧者本人の権限で毎回評価**される:
//!   行述語・フィールドマスクの違う 2 人が同じ表から違う行・列を受け取り、権限を剥奪された閲覧者は
//!   何も受け取らない（同じ dispatcher・同じ版のまま、剥奪の直後の呼び出しから）。
//! - クライアントが送れるのは `action_id + params`（ページ位置・件数）だけ: 条件を送ると拒否、
//!   画面に無い表・束ねていないビューは 404。
//! - 保存時（`MiniAppStore::create`）に**保存者の権限**でビューのピンを解決する: 読めないビュー・
//!   束ねていない別名は拒否。ビューを改訂しても画面はピンした版のまま。

#![allow(
    clippy::pedantic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

mod common;

use std::sync::Arc;

use artifact::{ArtifactRole, ArtifactStore};
use async_trait::async_trait;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal, Relation};
use data::{
    CmpOp, CondOp, Condition, DataQuery, DataStore, DataViewBody, DataViewStore, FieldDef,
    FieldPolicy, FieldType, NewDataTable, PolicyExpr, PolicyOperand, RefResolver, RowPolicy,
    SortKey, TableSchema,
};
use gui::{
    ActionDispatcher, ActionError, GuiError, MiniAppActionRequest, MiniAppStore, SkillStore,
};
use serde_json::{json, Map, Value};
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
    let fga = OpenFgaClient::connect(
        reqwest::Client::new(),
        &OpenFgaConfig {
            base_url,
            store_name: format!("shiki-data-grid-test-{}", Uuid::new_v4()),
        },
        &authz::model::default_model(),
    )
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

/// 申請者本人か経理（keiri）だけが行を読め、`internal_memo` は経理だけが読める経費テーブル。
fn expense_schema() -> TableSchema {
    let mut applicant = FieldDef::new("applicant", FieldType::UserRef);
    applicant.indexed = true;
    let mut amount = FieldDef::new("amount", FieldType::Number);
    amount.indexed = true;
    let mut status = FieldDef::new("status", FieldType::Select);
    status.options = vec!["承認待ち".into(), "承認済み".into()];
    status.indexed = true;
    TableSchema {
        fields: vec![
            applicant,
            amount,
            status,
            FieldDef::new("title", FieldType::Text),
            FieldDef::new("internal_memo", FieldType::Text),
        ],
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
            write: Some(PolicyExpr::Public),
        }),
        field_policy: vec![FieldPolicy {
            field: "internal_memo".into(),
            readable_by: PolicyExpr::HasRole {
                role: "keiri".into(),
                subtree: true,
            },
        }],
        aggregate_min_rows: None,
        fsm_ref: None,
        rag: None,
    }
}

fn req(action_id: &str, params: Value) -> MiniAppActionRequest {
    let params: Map<String, Value> = match params {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    MiniAppActionRequest {
        version: 1,
        action_id: action_id.into(),
        params,
        idempotency_key: None,
    }
}

/// 表の 1 ページ（`result.result.items` の applicant/amount と、`internal_memo` を受け取ったか）。
fn rows(out: &Value) -> Vec<(String, i64, bool)> {
    assert_eq!(out["kind"], "data_view", "{out}");
    out["result"]["result"]["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|r| {
            (
                r["data"]["applicant"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                r["data"]["amount"].as_f64().unwrap_or_default() as i64,
                r["data"].get("internal_memo").is_some(),
            )
        })
        .collect()
}

#[tokio::test]
async fn data_grid_is_evaluated_with_each_viewers_own_permissions() {
    let Some((pool, fga)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice"); // 作成者（テーブル・ビュー・ミニアプリの owner）
    let bob = ctx(&tenant, "bob"); // 一般社員: 自分の行だけ・内部メモは見えない
    let carol = ctx(&tenant, "carol"); // 経理: 全行・内部メモも見える
    let dave = ctx(&tenant, "dave"); // 経理だったが、テーブルの閲覧権限を剥奪される

    let data = DataStore::new(pool.clone(), Arc::clone(&fga), Arc::new(AllowRefs));
    let artifacts = Arc::new(ArtifactStore::new(pool.clone(), Arc::clone(&fga)));
    let views = DataViewStore::new(Arc::clone(&artifacts), data.clone());
    let mini_apps = MiniAppStore::new(Arc::clone(&artifacts), pool.clone());
    let mut dispatcher = ActionDispatcher::new(storage::audit::AuditRecorder::new(pool.clone()));
    dispatcher.set_data_views(views.clone());

    let table = data
        .create_table(
            &alice,
            NewDataTable {
                name: "経費".into(),
                schema: expense_schema(),
            },
            None,
        )
        .await
        .expect("table");
    let ns = alice.ns();
    for who in [&bob, &carol, &dave] {
        fga.write_tuple(
            &who.subject(),
            Relation::Viewer,
            &ns.data_table(&table.id.to_string()),
        )
        .await
        .expect("table viewer");
    }
    // 内部メモ（経理だけが読める列）へ書けるのは読める人だけ（マスク列への書込は 403）。種を入れる
    // 作成者の alice も経理にする（行の見え方の検査は bob / carol / dave で行う）。
    for who in [&alice, &carol, &dave] {
        fga.write_tuple(&who.subject(), Relation::Member, &ns.role("keiri"))
            .await
            .expect("keiri");
    }
    for (applicant, amount, status) in [
        ("alice", 100, "承認待ち"),
        ("bob", 300, "承認待ち"),
        ("bob", 50, "承認済み"),
        ("carol", 200, "承認待ち"),
    ] {
        data.create_record(
            &alice,
            table.id,
            json!({
                "applicant": applicant, "amount": amount, "status": status,
                "title": format!("{applicant} の申請"), "internal_memo": "社外秘"
            }),
            None,
        )
        .await
        .expect("seed");
    }

    // alice が「承認待ちを金額の大きい順」の保存ビューを作る（v1）。
    let pending = DataQuery {
        filter: Some(Condition::cmp(
            "status",
            CondOp::Eq,
            Some(json!("承認待ち")),
        )),
        sort: vec![SortKey {
            field: "amount".into(),
            descending: true,
        }],
        ..Default::default()
    };
    let view_id = views
        .create(
            &alice,
            "承認待ちの経費",
            &DataViewBody {
                table_id: table.id,
                query: pending,
                display: data::ViewSpec::default(),
            },
            None,
        )
        .await
        .expect("view");

    // --- 保存時のピン解決（保存者の権限）---
    let screen = "# 経費\n\n```data_grid\n承認待ち\n```\n";
    let body = json!({
        "description": "経費の確認",
        "screen": screen,
        "data_views": [{ "alias": "承認待ち", "artifact_id": view_id, "version": 1 }],
    });
    // bob はビューを読めない → 保存者の権限で解決できず拒否（見えないビューは束ねられない）。
    let err = mini_apps
        .create(&bob, "bob-app", &body, None)
        .await
        .expect_err("読めないビューは束ねられない");
    let codes: Vec<(String, Option<String>)> = err
        .validation_errors()
        .unwrap()
        .iter()
        .map(|e| (e.code.clone(), e.path.clone()))
        .collect();
    assert_eq!(
        codes,
        vec![(
            "miniapp.pin_unresolved".to_string(),
            Some("data_views[0]".to_string())
        )]
    );
    // 文言は利用者の画面に出るので、ビューを別名と版で指し、id を出さない（BUG-80）。
    let message = &err.validation_errors().unwrap()[0].message;
    assert_eq!(
        message,
        "保存ビュー「承認待ち」（v1）が見つからないか、あなたには読めません"
    );
    assert!(!message.contains(&view_id.to_string()));
    // 画面の表が束ねていない別名を参照していたら拒否。
    let unbundled = json!({
        "description": "x",
        "screen": "```data_grid\n全件\n```",
        "data_views": [{ "alias": "承認待ち", "artifact_id": view_id, "version": 1 }],
    });
    let err = mini_apps
        .create(&alice, "bad-app", &unbundled, None)
        .await
        .expect_err("束ねていない別名");
    assert!(
        matches!(&err, GuiError::Validation(v) if v[0].code == "miniapp.data_view_not_bundled")
    );

    let (app_id, _) = mini_apps
        .create(&alice, "経費アプリ", &body, None)
        .await
        .expect("mini app");
    // ミニアプリ**本体だけ**を共有する（ビューは個別共有しない＝バンドル権限で定義を読む）。
    for who in ["bob", "carol", "dave"] {
        artifacts
            .share(
                &alice,
                app_id,
                &storage::ShareTarget::User { id: who.into() },
                ArtifactRole::Viewer,
                None,
            )
            .await
            .expect("share app");
    }
    // ビューを改訂しても（承認済みへ）、画面はピンした v1 のまま。
    views
        .update(
            &alice,
            view_id,
            &DataViewBody {
                table_id: table.id,
                query: DataQuery {
                    filter: Some(Condition::cmp(
                        "status",
                        CondOp::Eq,
                        Some(json!("承認済み")),
                    )),
                    ..Default::default()
                },
                display: data::ViewSpec::default(),
            },
            Some(1),
            None,
        )
        .await
        .expect("view v2");

    let grid = gui::data_grid_action_id("承認待ち");
    let invoke = |who: AuthContext, params: Value| {
        let mini_apps = mini_apps.clone();
        let dispatcher = &dispatcher;
        let grid = grid.clone();
        async move {
            mini_apps
                .invoke_yui_action(&who, dispatcher, app_id, req(&grid, params), None)
                .await
        }
    };

    // --- 閲覧者ごとに違う行・列（同じ表・同じ版） ---
    let bob_rows = rows(&invoke(bob.clone(), json!({})).await.expect("bob"));
    assert_eq!(
        bob_rows,
        vec![("bob".to_string(), 300, false)],
        "bob は自分の承認待ちだけ・内部メモは受け取らない"
    );
    let carol_out = invoke(carol.clone(), json!({})).await.expect("carol");
    assert_eq!(
        rows(&carol_out),
        vec![
            ("bob".to_string(), 300, true),
            ("carol".to_string(), 200, true),
            ("alice".to_string(), 100, true),
        ],
        "経理は全員の承認待ちを金額の大きい順・内部メモも受け取る（ピンした v1 の条件）"
    );
    assert_eq!(carol_out["result"]["view_id"], json!(view_id));
    assert_eq!(carol_out["result"]["version"], 1);
    assert_eq!(carol_out["result"]["table_id"], json!(table.id));

    // ページ送り（送れるのはページ位置と件数だけ）。
    let first = invoke(carol.clone(), json!({ "limit": 1 }))
        .await
        .expect("page 1");
    assert_eq!(rows(&first), vec![("bob".to_string(), 300, true)]);
    let cursor = first["result"]["result"]["next_cursor"]
        .as_str()
        .expect("next_cursor")
        .to_string();
    let second = invoke(carol.clone(), json!({ "limit": 1, "cursor": cursor }))
        .await
        .expect("page 2");
    assert_eq!(rows(&second), vec![("carol".to_string(), 200, true)]);

    // --- 剥奪: 直後の呼び出しから何も受け取らない（毎回評価・キャッシュしない）---
    assert_eq!(
        rows(&invoke(dave.clone(), json!({})).await.expect("dave before")).len(),
        3,
        "剥奪前の dave（経理）は全件"
    );
    fga.delete_tuple(
        &dave.subject(),
        Relation::Viewer,
        &ns.data_table(&table.id.to_string()),
    )
    .await
    .expect("revoke");
    let revoked = invoke(dave.clone(), json!({})).await;
    assert!(
        matches!(
            revoked,
            Err(ActionError::Forbidden) | Err(ActionError::NotFound)
        ),
        "剥奪後は行を返さない: {revoked:?}"
    );

    // --- クライアントは束縛・条件を送れない ---
    let with_filter = invoke(
        carol.clone(),
        json!({ "filter": { "cmp": { "field": "status", "op": "eq", "value": "承認済み" } } }),
    )
    .await;
    assert!(
        matches!(with_filter, Err(ActionError::Invalid(_))),
        "条件は params に載せられない: {with_filter:?}"
    );
    let undeclared = mini_apps
        .invoke_yui_action(
            &carol,
            &dispatcher,
            app_id,
            req(&gui::data_grid_action_id("全件"), json!({})),
            None,
        )
        .await;
    assert!(matches!(undeclared, Err(ActionError::NotFound)));

    // 描画は監査に残る（ui_action.invoke・束縛種は data_view）。
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND action = 'ui_action.invoke' \
           AND metadata->>'binding' = 'data_view'",
    )
    .bind(&tenant)
    .fetch_one(&pool)
    .await
    .expect("audit");
    assert!(audited >= 5, "表の描画が監査に残る: {audited}");
}

/// 表の描画・ページ送りは、ミニアプリの解決（`miniapp.resolve`・skill のバンドル読み）をやり直さない（BUG-24）。
/// 1 ページごとに残るのは操作（`ui_action.invoke`）とビューの定義のバンドル読みだけ。本体の viewer は毎ページ
/// 確かめ直すので、共有を外した直後のページ送りから読めない。束ねた skill を消しても表は描ける。
#[tokio::test]
async fn paging_does_not_resolve_the_app_again() {
    let Some((pool, fga)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let data = DataStore::new(pool.clone(), Arc::clone(&fga), Arc::new(AllowRefs));
    let artifacts = Arc::new(ArtifactStore::new(pool.clone(), Arc::clone(&fga)));
    let views = DataViewStore::new(Arc::clone(&artifacts), data.clone());
    let skills = SkillStore::new(Arc::clone(&artifacts));
    let mini_apps = MiniAppStore::new(Arc::clone(&artifacts), pool.clone());
    let mut dispatcher = ActionDispatcher::new(storage::audit::AuditRecorder::new(pool.clone()));
    dispatcher.set_data_views(views.clone());

    let table = data
        .create_table(
            &alice,
            NewDataTable {
                name: "備品".into(),
                schema: TableSchema {
                    fields: vec![FieldDef::new("title", FieldType::Text)],
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
        .expect("table");
    fga.write_tuple(
        &bob.subject(),
        Relation::Viewer,
        &alice.ns().data_table(&table.id.to_string()),
    )
    .await
    .expect("table viewer");
    for title in ["机", "椅子", "棚"] {
        data.create_record(&alice, table.id, json!({ "title": title }), None)
            .await
            .expect("seed");
    }
    let view_id = views
        .create(
            &alice,
            "備品の一覧",
            &DataViewBody {
                table_id: table.id,
                query: DataQuery::default(),
                display: data::ViewSpec::default(),
            },
            None,
        )
        .await
        .expect("view");
    let (skill_id, _) = skills
        .create(
            &alice,
            "備品の案内",
            &json!({ "description": "備品の案内", "instructions": "備品の質問に答える。" }),
            None,
        )
        .await
        .expect("skill");
    let (app_id, _) = mini_apps
        .create(
            &alice,
            "備品アプリ",
            &json!({
                "description": "備品",
                "screen": "```data_grid\n一覧\n```\n",
                "skill": { "artifact_id": skill_id, "version": 1 },
                "data_views": [{ "alias": "一覧", "artifact_id": view_id, "version": 1 }],
            }),
            None,
        )
        .await
        .expect("mini app");
    artifacts
        .share(
            &alice,
            app_id,
            &storage::ShareTarget::User { id: "bob".into() },
            ArtifactRole::Viewer,
            None,
        )
        .await
        .expect("share app");

    let grid = gui::data_grid_action_id("一覧");
    let page = |who: AuthContext, params: Value| {
        let mini_apps = mini_apps.clone();
        let dispatcher = &dispatcher;
        let grid = grid.clone();
        async move {
            mini_apps
                .invoke_yui_action(&who, dispatcher, app_id, req(&grid, params), None)
                .await
        }
    };
    // 1 件ずつ 3 ページ送る。
    let mut cursor: Option<String> = None;
    let mut seen = Vec::new();
    for _ in 0..3 {
        let params = match &cursor {
            Some(c) => json!({ "limit": 1, "cursor": c }),
            None => json!({ "limit": 1 }),
        };
        let out = page(bob.clone(), params).await.expect("page");
        let items = out["result"]["result"]["items"].as_array().expect("items");
        assert_eq!(items.len(), 1, "{out}");
        seen.push(
            items[0]["data"]["title"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        );
        cursor = out["result"]["result"]["next_cursor"]
            .as_str()
            .map(str::to_string);
    }
    seen.sort();
    let mut all: Vec<String> = ["机", "椅子", "棚"].map(String::from).to_vec();
    all.sort();
    assert_eq!(seen, all, "3 ページで全行を 1 回ずつ");

    // bob の 3 ページで残った監査: 解決（miniapp.resolve）と skill のバンドル読みは無い。
    let count = |action: &'static str, object: Option<Uuid>| {
        let pool = pool.clone();
        let tenant = tenant.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND actor = 'bob' \
                   AND action = $2 AND ($3::text IS NULL OR object_id = $3)",
            )
            .bind(&tenant)
            .bind(action)
            .bind(object.map(|id| id.to_string()))
            .fetch_one(&pool)
            .await
            .expect("audit")
        }
    };
    assert_eq!(
        count("miniapp.resolve", None).await,
        0,
        "ページ送りで解決し直さない"
    );
    assert_eq!(
        count("artifact.read_via_bundle", Some(skill_id)).await,
        0,
        "表は skill を読まない"
    );
    assert_eq!(
        count("artifact.read_via_bundle", Some(view_id)).await,
        3,
        "ビューの定義のバンドル読みは 1 ページに 1 回"
    );
    assert_eq!(
        count("ui_action.invoke", None).await,
        3,
        "操作は 1 ページに 1 回"
    );

    // 束ねた skill を消しても表は描ける（表は skill を使わない）。
    artifacts
        .delete(&alice, skill_id, None)
        .await
        .expect("skill を消す");
    assert_eq!(
        page(alice.clone(), json!({ "limit": 1 }))
            .await
            .expect("alice")["result"]["result"]["items"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );

    // 本体の viewer はページごとに確かめ直す: 共有を外した直後のページ送りから読めない。
    artifacts
        .unshare(
            &alice,
            app_id,
            &storage::ShareTarget::User { id: "bob".into() },
            ArtifactRole::Viewer,
            None,
        )
        .await
        .expect("unshare");
    let revoked = page(bob.clone(), json!({ "limit": 1 })).await;
    assert!(
        matches!(
            revoked,
            Err(ActionError::Forbidden) | Err(ActionError::NotFound)
        ),
        "共有を外したら次のページから読めない: {revoked:?}"
    );

    common::purge_tenant_rows(&pool, &tenant).await;
}

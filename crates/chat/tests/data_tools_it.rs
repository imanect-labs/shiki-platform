//! 構造化データのツール（data_query / data_schema / data_record_write）の結合テスト（Task 15.7）。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）＋実 OpenFGA（`OPENFGA_TEST_URL`）で次を確かめる。
//!
//! - 「経費テーブルの承認待ちを金額順に見せて」に対して LLM が出す `data_query` の入力（`data::DataQuery`
//!   そのもの）を、行述語・フィールドマスクの違う 2 人で実行し、**見えない行・列が返らない**。
//! - 同じ入力を stub LLM（`calltool:`）からチャットの run として流し、ツールの提示（見えるテーブルが
//!   無い人には提示しない）→ 実行 → 観測までの実経路でも同じになる。
//! - description には発話ユーザーが viewer のテーブルだけが載る。`data_schema` は読めない列を載せない。
//!   `data_record_write` は承認必須で、読めない行は更新できない。

#![allow(
    clippy::pedantic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;
use std::time::Duration;

use agent_core::Tool;
use async_trait::async_trait;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal, Relation};
use chat::data_tool::{DataQueryTool, DataRecordWriteTool, DataSchemaTool, DataTableCatalog};
use chat::{ChatStore, ChatWorker, StreamEventKind, WorkerConfig};
use data::{
    CmpOp, DataStore, FieldDef, FieldPolicy, FieldType, NewDataTable, PolicyExpr, PolicyOperand,
    RefResolver, RowPolicy, TableSchema,
};
use futures::stream::StreamExt;
use llm_gateway::{
    GatewayConfig, LlmGateway, ModelCatalog, ModelEntry, ProviderConfig, ProviderKind,
};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
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
        .max_connections(8)
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
            store_name: format!("shiki-chat-data-test-{}", Uuid::new_v4()),
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
    amount.display_name = "金額".into();
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

/// 「経費テーブルの承認待ちを金額順に見せて」に対して LLM が出す入力（data::DataQuery そのもの）。
fn llm_input() -> Value {
    json!({
        "table": "経費",
        "query": {
            "filter": { "cmp": { "field": "status", "op": "eq", "value": "承認待ち" } },
            "sort": [{ "field": "amount", "descending": true }]
        }
    })
}

struct Fixture {
    fga: Arc<dyn AuthzClient>,
    data: DataStore,
    tenant: String,
    table_id: Uuid,
    carol_row: Uuid,
}

/// alice（経理）が経費テーブルを作り、bob（一般）・carol（経理）を viewer にする。dave は何も持たない。
async fn fixture(pool: PgPool, fga: Arc<dyn AuthzClient>) -> Fixture {
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let data = DataStore::new(pool, Arc::clone(&fga), Arc::new(AllowRefs));
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
    // 見えるかどうかの対照に、bob には見えない別テーブルも作る。
    data.create_table(
        &alice,
        NewDataTable {
            name: "役員報酬".into(),
            schema: expense_schema(),
        },
        None,
    )
    .await
    .expect("hidden table");
    let ns = alice.ns();
    for who in ["bob", "carol"] {
        fga.write_tuple(
            &ctx(&tenant, who).subject(),
            Relation::Viewer,
            &ns.data_table(&table.id.to_string()),
        )
        .await
        .expect("viewer");
    }
    // 読めない列へは書けない（PIT-19）ので、社外秘メモを入れる alice も keiri にする。
    for who in ["carol", "alice"] {
        fga.write_tuple(
            &ctx(&tenant, who).subject(),
            Relation::Member,
            &ns.role("keiri"),
        )
        .await
        .expect("keiri");
    }
    let mut carol_row = Uuid::nil();
    for (applicant, amount, status) in [
        ("alice", 100, "承認待ち"),
        ("bob", 300, "承認待ち"),
        ("bob", 50, "承認済み"),
        ("carol", 200, "承認待ち"),
    ] {
        let r = data
            .create_record(
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
        if applicant == "carol" {
            carol_row = r.id;
        }
    }
    Fixture {
        fga,
        data,
        tenant,
        table_id: table.id,
        carol_row,
    }
}

/// 観測の行（1 行 1 レコードの JSON）を (applicant, amount, internal_memo を受け取ったか) に。
fn observed_rows(content: &str) -> Vec<(String, i64, bool)> {
    content
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v.get("id").is_some())
        .map(|v| {
            (
                v["applicant"].as_str().unwrap_or_default().to_string(),
                v["amount"].as_f64().unwrap_or_default() as i64,
                v.get("internal_memo").is_some(),
            )
        })
        .collect()
}

async fn tools_for(f: &Fixture, c: &AuthContext) -> Arc<DataTableCatalog> {
    Arc::new(DataTableCatalog::load(&f.data, c).await.expect("catalog"))
}

#[tokio::test]
async fn data_query_returns_only_rows_and_columns_the_speaker_can_read() {
    let Some((pool, fga)) = setup().await else {
        return;
    };
    let f = fixture(pool, fga).await;
    let bob = ctx(&f.tenant, "bob");
    let carol = ctx(&f.tenant, "carol");
    let dave = ctx(&f.tenant, "dave");

    // description には発話ユーザーが viewer のテーブルだけ（bob に「役員報酬」は載らない）。
    let bob_catalog = tools_for(&f, &bob).await;
    let bob_query = DataQueryTool::new(f.data.clone(), Arc::clone(&bob_catalog));
    assert!(bob_query
        .description()
        .contains(&format!("- 経費 / {}", f.table_id)));
    assert!(!bob_query.description().contains("役員報酬"));
    assert!(bob_query.is_read_only());
    // 見えるテーブルが無い dave には提示しない（toolset は空の一覧で提示を見送る）。
    assert!(tools_for(&f, &dave).await.is_empty());

    // bob: 自分の承認待ちだけ・内部メモは受け取らない。
    let out = bob_query.call(&bob, llm_input(), None).await.expect("bob");
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(
        observed_rows(&out.content),
        vec![("bob".to_string(), 300, false)],
        "{}",
        out.content
    );
    assert!(!out.content.contains("社外秘"));

    // carol（経理）: 全員の承認待ちを金額の大きい順・内部メモも受け取る。
    let carol_query = DataQueryTool::new(f.data.clone(), tools_for(&f, &carol).await);
    let out = carol_query
        .call(&carol, llm_input(), None)
        .await
        .expect("carol");
    assert_eq!(
        observed_rows(&out.content),
        vec![
            ("bob".to_string(), 300, true),
            ("carol".to_string(), 200, true),
            ("alice".to_string(), 100, true),
        ],
        "{}",
        out.content
    );

    // 見えないテーブルを id で直接指しても、存在を明かさない観測エラー。
    let mut by_id = llm_input();
    by_id["table"] = json!(f.table_id.to_string());
    let out = DataQueryTool::new(f.data.clone(), Arc::new(DataTableCatalog::default()))
        .call(&dave, by_id, None)
        .await
        .expect("dave");
    assert!(out.is_error);
    assert!(
        out.content.contains("見つからないか、権限がありません"),
        "{}",
        out.content
    );

    // 読めない列で絞り込むと拒否（マスク列を検索に使わせない・PIT-19）。
    let out = bob_query
        .call(
            &bob,
            json!({ "table": "経費", "query": {
                "filter": { "cmp": { "field": "internal_memo", "op": "eq", "value": "社外秘" } }
            }}),
            None,
        )
        .await
        .expect("masked filter");
    assert!(out.is_error, "{}", out.content);

    // 一覧に無い名前は候補つきの観測エラー（ハルシネーション境界）。
    let out = bob_query
        .call(&bob, json!({ "table": "役員報酬" }), None)
        .await
        .expect("unknown name");
    assert!(
        out.is_error && out.content.contains("経費"),
        "{}",
        out.content
    );

    // data_schema: bob には internal_memo が載らない・carol には載る。
    let schema = DataSchemaTool::new(f.data.clone(), Arc::clone(&bob_catalog));
    let out = schema
        .call(&bob, json!({ "table": "経費" }), None)
        .await
        .expect("schema bob");
    assert!(
        out.content.contains("- amount: number — 金額"),
        "{}",
        out.content
    );
    assert!(out.content.contains("承認待ち, 承認済み"));
    assert!(!out.content.contains("internal_memo"), "{}", out.content);
    let out = DataSchemaTool::new(f.data.clone(), tools_for(&f, &carol).await)
        .call(&carol, json!({ "table": "経費" }), None)
        .await
        .expect("schema carol");
    assert!(out.content.contains("internal_memo"), "{}", out.content);

    // data_record_write: 承認必須。bob（editor にする）は作成できるが、読めない carol の行は更新できない。
    let write = DataRecordWriteTool::new(f.data.clone(), Arc::clone(&bob_catalog));
    assert!(write.requires_confirmation());
    f.fga
        .write_tuple(
            &bob.subject(),
            Relation::Editor,
            &bob.ns().data_table(&f.table_id.to_string()),
        )
        .await
        .expect("bob editor");
    let out = write
        .call(
            &bob,
            json!({ "table": "経費", "op": "create", "data": {
                "applicant": "bob", "amount": 80, "status": "承認待ち", "title": "交通費"
            }}),
            None,
        )
        .await
        .expect("create");
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("作成しました"));
    let out = write
        .call(
            &bob,
            json!({ "table": "経費", "op": "update", "record_id": f.carol_row, "data": { "amount": 1 } }),
            None,
        )
        .await
        .expect("update hidden");
    assert!(out.is_error, "読めない行は更新できない: {}", out.content);
    let kept = f
        .data
        .get_record(&carol, f.table_id, f.carol_row, None)
        .await
        .expect("carol reads own row");
    assert_eq!(
        kept.data["amount"].as_f64(),
        Some(200.0),
        "carol の行は書き換わらない"
    );
}

fn stub_gateway(pool: PgPool) -> LlmGateway {
    let config = GatewayConfig {
        provider: ProviderConfig {
            kind: ProviderKind::Stub,
            base_url: None,
            api_key: None,
            user_agent: None,
            session_header: None,
            ..Default::default()
        },
        catalog: ModelCatalog {
            default_model: "m".into(),
            models: vec![ModelEntry {
                id: "m".into(),
                real_id: None,
                prompt_price_micros_per_mtok: 0,
                completion_price_micros_per_mtok: 0,
                context_window_tokens: None,
                ..Default::default()
            }],
        },
        langfuse: None,
        ..Default::default()
    };
    LlmGateway::build(pool, reqwest::Client::new(), config).expect("gateway")
}

/// 発話 → done まで流し、観測イベントを返す。
async fn run_to_done(store: &ChatStore, c: &AuthContext, text: &str) -> Vec<StreamEventKind> {
    let thread = store.create_thread(c, "t", true, None, None).await.unwrap();
    let res = store
        .post_message(c, thread.id, text, &[], None, Some(true), false, &[], None)
        .await
        .unwrap();
    let mut rx = store.event_stream(&c.tenant_id, res.run_id, 0);
    let mut events = Vec::new();
    for _ in 0..500 {
        let next = tokio::time::timeout(Duration::from_secs(180), rx.next())
            .await
            .expect("イベント待ちがタイムアウト");
        let Some(ev) = next else { break };
        let is_done = matches!(ev.event, StreamEventKind::Done { .. });
        if let StreamEventKind::Error { message } = &ev.event {
            panic!("生成失敗: {message}");
        }
        events.push(ev.event);
        if is_done {
            break;
        }
    }
    events
}

fn data_query_result(events: &[StreamEventKind]) -> Option<(bool, String)> {
    let call_id = events.iter().find_map(|e| match e {
        StreamEventKind::ToolCall { id, name, .. } if name == "data_query" => Some(id.clone()),
        _ => None,
    })?;
    events.iter().find_map(|e| match e {
        StreamEventKind::ToolResult {
            tool_call_id,
            ok,
            content,
        } if *tool_call_id == call_id => Some((*ok, content.clone())),
        _ => None,
    })
}

/// stub LLM が LLM と同じ入力で `data_query` を呼ぶ chat run（提示 → 実行 → 観測の実経路）。
///
/// 同一プロセスの jobq キューを複数 worker が奪い合わないよう、1 テストに直列化する。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_run_answers_pending_expenses_by_amount_within_speaker_permissions() {
    let Some((pool, fga)) = setup().await else {
        return;
    };
    let f = fixture(pool.clone(), Arc::clone(&fga)).await;
    let store = ChatStore::connect(pool.clone(), Arc::clone(&fga), None)
        .await
        .unwrap();
    let worker = ChatWorker::new(
        pool.clone(),
        store.clone(),
        chat::WorkerDeps {
            gateway: stub_gateway(pool.clone()),
            search: None,
            sandbox: None,
            artifacts: None,
            web_search: None,
            parser: None,
            storage: None,
            workflow_resolver: None,
            skill_artifacts: None,
            skill_catalog: None,
            workflow_store: None,
            workflow_catalog: None,
            collab: None,
            tabular: None,
            office: None,
            office_live: None,
            office_creator: None,
            office_engine: None,
        },
        WorkerConfig {
            system_prompt: "あなたはアシスタントです。".into(),
            model: Some("m".into()),
            lease_secs: 120,
            max_steps: 4,
            ..Default::default()
        },
    )
    .with_data(f.data.clone());
    worker.spawn(1);

    let text = format!("calltool:data_query {}", llm_input());
    let bob = ctx(&f.tenant, "bob");
    let (ok, content) =
        data_query_result(&run_to_done(&store, &bob, &text).await).expect("bob の data_query");
    assert!(ok, "{content}");
    assert_eq!(
        observed_rows(&content),
        vec![("bob".to_string(), 300, false)]
    );

    let carol = ctx(&f.tenant, "carol");
    let (ok, content) =
        data_query_result(&run_to_done(&store, &carol, &text).await).expect("carol の data_query");
    assert!(ok, "{content}");
    assert_eq!(
        observed_rows(&content),
        vec![
            ("bob".to_string(), 300, true),
            ("carol".to_string(), 200, true),
            ("alice".to_string(), 100, true),
        ]
    );

    // 見えるテーブルが無い dave にはツールが提示されない（呼ばれず、本文の応答になる）。
    let dave = ctx(&f.tenant, "dave");
    let events = run_to_done(&store, &dave, &text).await;
    assert!(
        data_query_result(&events).is_none(),
        "見えるテーブルが無い人に data_query は提示されない"
    );
}

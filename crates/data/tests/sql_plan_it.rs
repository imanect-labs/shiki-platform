//! SQL 前段面の不変条件とプランキャッシュ（Task 15.10・data-platform §16.1・§16.3）。
//!
//! - **呼び出し側の SQL テキストが 1 文字もデータベースへ届かない**: DataStore と Postgres の間に TCP の
//!   中継を挟み、クライアントが送った Parse / Query メッセージ（＝データベースへ渡った SQL テキスト）を
//!   すべて記録して、呼び出し側の SQL の断片（別名・コメント・リテラル）が 1 つも無いことを確かめる。
//! - **同一形状は同一の SQL テキスト**: 別の principal（材料の違う利用者）・別の値で同じ形状を流し、
//!   実行した文のテキストがバイト単位で一致し、違いがバインドだけであること。プランキャッシュに
//!   材料（principal・ロール・共有 id）が載っていないこと。
//! - **prepared statement の再利用の計測**: 1 接続のプールで `pg_prepared_statements` を数え、
//!   2 人目・別の値の実行で新しい prepared statement が増えず、実行回数だけが増えること。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）＋実 OpenFGA（`OPENFGA_TEST_URL`・材料の違う principal）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::too_many_lines
)]

mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Relation};
use data::{
    CmpOp, DataQuery, DataStore, FieldType, ForcedStrategy, PolicyExpr, PolicyOperand, QueryTuning,
    RecordShareRole, RefResolver, RowPolicy, SqlRequest,
};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;

use common::{create_table, ctx, field, schema, AllowAll};

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
        Ok(false)
    }
}

/// クライアント → サーバのバイト列（接続ごと）。
type Captured = Arc<Mutex<Vec<Arc<Mutex<Vec<u8>>>>>>;

/// Postgres の前に挟む TCP 中継（クライアントが送ったバイトを接続ごとに記録する）。
async fn proxy(upstream: String) -> (u16, Captured) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let captured: Captured = Arc::default();
    let cap = Arc::clone(&captured);
    tokio::spawn(async move {
        loop {
            let Ok((mut client, _)) = listener.accept().await else {
                return;
            };
            let Ok(mut server) = TcpStream::connect(&upstream).await else {
                return;
            };
            let log = Arc::new(Mutex::new(Vec::new()));
            cap.lock().unwrap().push(Arc::clone(&log));
            tokio::spawn(async move {
                let (mut cr, mut cw) = client.split();
                let (mut sr, mut sw) = server.split();
                let up = async {
                    let mut buf = vec![0u8; 64 * 1024];
                    loop {
                        let n = match cr.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => n,
                        };
                        log.lock().unwrap().extend_from_slice(&buf[..n]);
                        if sw.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                    let _ = sw.shutdown().await;
                };
                let down = async {
                    let _ = tokio::io::copy(&mut sr, &mut cw).await;
                    let _ = cw.shutdown().await;
                };
                tokio::join!(up, down);
            });
        }
    });
    (port, captured)
}

/// クライアントが送った SQL テキスト（Parse の問い合わせ文と単純問い合わせ）を取り出す。
fn sent_sql(bytes: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    // 型バイトの無い開始メッセージ（SSLRequest / StartupMessage）を読み飛ばす。
    while i + 8 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[i..i + 4].try_into().unwrap()) as usize;
        let code = u32::from_be_bytes(bytes[i + 4..i + 8].try_into().unwrap());
        if code == 80_877_103 || code == 196_608 {
            i += len;
        } else {
            break;
        }
    }
    while i + 5 <= bytes.len() {
        let ty = bytes[i];
        let len = u32::from_be_bytes(bytes[i + 1..i + 5].try_into().unwrap()) as usize;
        let end = (i + 1 + len).min(bytes.len());
        let body = &bytes[i + 5..end];
        match ty {
            // Parse: 文の名前\0 問い合わせ文\0 …
            b'P' => {
                let mut parts = body.splitn(3, |b| *b == 0);
                let _name = parts.next();
                if let Some(q) = parts.next() {
                    out.push(String::from_utf8_lossy(q).to_string());
                }
            }
            // 単純問い合わせ: 問い合わせ文\0
            b'Q' => out.push(
                String::from_utf8_lossy(body.split(|b| *b == 0).next().unwrap_or(&[])).to_string(),
            ),
            _ => {}
        }
        i = end;
    }
    out
}

fn db_url() -> Option<String> {
    std::env::var("STORAGE_TEST_DATABASE_URL").ok()
}

async fn migrate(url: &str) -> PgPool {
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(url)
        .await
        .expect("Postgres へ接続できること");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("マイグレーション適用");
    pool
}

#[tokio::test]
async fn caller_sql_text_never_reaches_the_database() {
    let Some(url) = db_url() else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return;
    };
    migrate(&url).await;
    // postgres://user:pass@host:port/db の host:port を中継へ差し替える。
    let (prefix, rest) = url.split_once('@').unwrap();
    let (hostport, db) = rest.split_once('/').unwrap();
    let (port, captured) = proxy(hostport.to_string()).await;
    let proxied = format!("{prefix}@127.0.0.1:{port}/{db}");
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&format!("{proxied}?sslmode=disable"))
        .await
        .unwrap();
    let store = DataStore::new(pool, Arc::new(AllowAll), Arc::new(common::FixedResolver));
    let c = ctx(&common::unique_tenant(), "alice");
    let mut amount = field("amount", FieldType::Number);
    amount.indexed = true;
    let mut title = field("title", FieldType::Text);
    title.indexed = true;
    let t = create_table(&store, &c, "orders", schema(vec![title, amount])).await;
    for i in 0..5 {
        store
            .create_record(
                &c,
                t.id,
                json!({"title": format!("t{i}"), "amount": i}),
                None,
            )
            .await
            .unwrap();
    }
    // ここから先にデータベースへ送られた SQL だけを見る。
    for log in captured.lock().unwrap().iter() {
        log.lock().unwrap().clear();
    }
    let sql = "SELECT title AS zzqalias, zzqtbl.amount FROM orders zzqtbl \
               /* zzqcomment */ WHERE zzqtbl.amount >= 1 AND zzqtbl.title <> 'zzqvalue' \
               ORDER BY zzqtbl.amount DESC LIMIT 3";
    let r = store
        .run_sql(
            &c,
            &SqlRequest {
                sql: sql.into(),
                ..SqlRequest::default()
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(r.columns, vec!["zzqalias", "amount"]);
    assert_eq!(r.rows.len(), 3);
    let sent: Vec<String> = captured
        .lock()
        .unwrap()
        .iter()
        .flat_map(|l| sent_sql(&l.lock().unwrap()))
        .collect();
    assert!(
        sent.iter()
            .any(|s| s.contains("data_index_num") || s.contains("FROM data_record r")),
        "中継が問い合わせを記録できていること: {sent:?}"
    );
    for s in &sent {
        for marker in [
            "zzq",
            "zzqalias",
            "zzqtbl",
            "zzqcomment",
            "zzqvalue",
            "ORDER BY zzqtbl",
        ] {
            assert!(
                !s.contains(marker),
                "呼び出し側の SQL がデータベースへ渡った: {s}"
            );
        }
        assert!(!s.contains(sql), "{s}");
    }
    // IR 直投入で同じ問い合わせを流した文は、SQL 経由で送った文に含まれる（IR が合流点）。
    // 調整は SQL 経路（ストアの既定＝環境変数の戦略固定を含む）と揃える。
    let compiled = store.compile_sql(&c, sql).await.unwrap();
    let (_, debug) = store
        .run_query_debug(
            &c,
            compiled.table_id,
            &compiled.query,
            &QueryTuning::from_env(),
        )
        .await
        .unwrap();
    for st in debug.statements {
        assert!(
            sent.contains(&st.sql),
            "IR 経路の文が SQL 経路で送られていない: {}",
            st.sql
        );
    }
}

async fn fga() -> Option<Arc<dyn AuthzClient>> {
    let Ok(base_url) = std::env::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return None;
    };
    let model: serde_json::Value =
        serde_json::from_str(include_str!("../../authz/model/authorization-model.json")).unwrap();
    let config = OpenFgaConfig {
        base_url,
        store_name: format!("shiki-data-plan-test-{}", Uuid::new_v4()),
    };
    Some(Arc::new(
        OpenFgaClient::connect(reqwest::Client::new(), &config, &model)
            .await
            .unwrap(),
    ))
}

/// 1 接続の上の prepared statement の数と実行回数。
async fn prepared(pool: &PgPool) -> (i64, i64) {
    sqlx::query_as(
        "SELECT count(*)::int8, coalesce(sum(generic_plans + custom_plans), 0)::int8 \
         FROM pg_prepared_statements",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn same_shape_is_byte_identical_across_principals_and_prepared_statements_are_reused() {
    let Some(url) = db_url() else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return;
    };
    let Some(authz) = fga().await else { return };
    migrate(&url).await;
    // 1 接続のプール（pg_prepared_statements は接続ごと）。
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let store = DataStore::new(pool.clone(), Arc::clone(&authz), Arc::new(AllowRefs));
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let charlie = ctx(&tenant, "charlie");
    let mut applicant = field("applicant", FieldType::UserRef);
    applicant.indexed = true;
    let mut dept = field("dept", FieldType::RoleRef);
    dept.indexed = true;
    let mut amount = field("amount", FieldType::Number);
    amount.indexed = true;
    let mut status = field("status", FieldType::Select);
    status.indexed = true;
    status.options = vec!["draft".into(), "submitted".into()];
    let mut s = schema(vec![applicant, dept, amount, status]);
    // 申請者本人・所属部署・経理ロール（subtree）の行だけ読める（材料が principal ごとに違う）。
    s.row_policy = Some(RowPolicy {
        read: PolicyExpr::Any(vec![
            PolicyExpr::FieldCmp {
                field: "applicant".into(),
                op: CmpOp::Eq,
                value: PolicyOperand::UserId,
            },
            PolicyExpr::FieldCmp {
                field: "dept".into(),
                op: CmpOp::In,
                value: PolicyOperand::UserRoles { subtree: true },
            },
            PolicyExpr::HasRole {
                role: "keiri".into(),
                subtree: true,
            },
        ]),
        write: None,
    });
    let table = create_table(&store, &alice, "expense", s).await;
    for who in [&bob, &charlie] {
        authz
            .write_tuple(
                &who.subject(),
                Relation::Editor,
                &alice.ns().data_table(&table.id.to_string()),
            )
            .await
            .unwrap();
    }
    authz
        .write_tuple(
            &charlie.subject(),
            Relation::Member,
            &alice.ns().role("keiri"),
        )
        .await
        .unwrap();
    authz
        .write_tuple(&bob.subject(), Relation::Member, &alice.ns().role("sales"))
        .await
        .unwrap();
    let mut ids = Vec::new();
    for (i, who) in [&alice, &bob, &alice, &bob, &alice].iter().enumerate() {
        let r = store
            .create_record(
                who,
                table.id,
                json!({"applicant": who.principal.id, "dept": if i % 2 == 0 { "sales" } else { "dev" },
                       "amount": i * 100, "status": if i < 3 { "draft" } else { "submitted" }}),
                None,
            )
            .await
            .unwrap();
        ids.push(r.id);
    }
    // alice の行を bob へ個別共有（bob の材料にだけ共有 id が入る）。
    store
        .share_record(
            &alice,
            table.id,
            ids[0],
            &storage::ShareTarget::User { id: "bob".into() },
            RecordShareRole::Viewer,
            None,
        )
        .await
        .unwrap();

    let q = |min: i64, st: &str| -> DataQuery {
        serde_json::from_value(json!({
            "filter": {"all": [
                {"cmp": {"field": "amount", "op": "gte", "value": min}},
                {"cmp": {"field": "status", "op": "neq", "value": st}}
            ]},
            "sort": [{"field": "amount", "descending": true}],
            "limit": 2,
            "with_count": true
        }))
        .unwrap()
    };
    for force in [
        ForcedStrategy::Sort,
        ForcedStrategy::Filter,
        ForcedStrategy::Policy,
        ForcedStrategy::Batch,
    ] {
        let tuning = QueryTuning {
            force: Some(force),
            ..QueryTuning::default()
        };
        let mut texts: Vec<Vec<String>> = Vec::new();
        let mut params = Vec::new();
        for (who, min, st) in [
            (&alice, 0, "submitted"),
            (&bob, 100, "draft"),
            (&charlie, 250, "x"),
        ] {
            let (_, d) = store
                .run_query_debug(who, table.id, &q(min, st), &tuning)
                .await
                .unwrap();
            texts.push(d.statements.iter().map(|s| s.sql.clone()).collect());
            params.push(
                d.statements
                    .iter()
                    .map(|s| s.params.clone())
                    .collect::<Vec<_>>(),
            );
        }
        // 同一形状 → 同一テキスト（principal・材料・値に依らない）。違いはバインドだけ。
        assert_eq!(
            texts[0], texts[1],
            "{force:?}: alice と bob で SQL テキストが違う"
        );
        assert_ne!(params[0], params[1], "{force:?}: 材料・値はバインドで渡る");
        if force == ForcedStrategy::Policy {
            // 認可駆動は行述語の選言から入口を作るので、`HasRole` の畳み込み結果（charlie は経理＝全行可視で
            // 認可駆動が成立しない）で計画が変わる。data-platform §16.3 の「畳み込み結果は形状に入れる」に
            // 当たる違いで、材料そのものはテキストに入らない（下のプランキャッシュの検査）。
            continue;
        }
        assert_eq!(
            texts[0], texts[2],
            "{force:?}: alice と charlie で SQL テキストが違う"
        );
        assert_ne!(params[1], params[2]);
    }
    // プランキャッシュに材料（principal・ロール・共有 id）も値も載っていない。
    let cached = store.plan_cache_texts();
    assert!(!cached.is_empty());
    for (_, t) in &cached {
        for banned in [
            "alice",
            "bob",
            "charlie",
            "keiri",
            "sales",
            "submitted",
            "draft",
        ] {
            assert!(
                !t.contains(banned),
                "プランキャッシュに材料・値: {banned} in {t}"
            );
        }
        for id in &ids {
            assert!(!t.contains(&id.to_string()), "{t}");
        }
    }
    assert_eq!(store.plan_cache_stats().overflows, 0);

    // prepared statement の再利用（1 接続）: 同じ戦略の下で、別の値・別の principal（材料が違う）でも
    // 新しい prepared statement は増えず、既存の文の実行回数だけが増える（形状ごとのテキストが固定）。
    let _ = prepared(&pool).await;
    for force in [ForcedStrategy::Sort, ForcedStrategy::Filter] {
        let tuning = QueryTuning {
            force: Some(force),
            ..QueryTuning::default()
        };
        store
            .run_query_debug(&alice, table.id, &q(0, "x"), &tuning)
            .await
            .unwrap();
        let (n1, e1) = prepared(&pool).await;
        let before = store.plan_cache_stats();
        for (who, min, st) in [
            (&alice, 300, "draft"),
            (&bob, 100, "y"),
            (&charlie, 50, "z"),
        ] {
            store
                .run_query_debug(who, table.id, &q(min, st), &tuning)
                .await
                .unwrap();
        }
        let (n2, e2) = prepared(&pool).await;
        let after = store.plan_cache_stats();
        eprintln!(
            "{force:?}: prepared statements {n1} → {n2}（実行回数 {e1} → {e2}）・プランキャッシュ hits {} → {}・misses {} → {}",
            before.hits, after.hits, before.misses, after.misses
        );
        assert_eq!(
            n1, n2,
            "{force:?}: 同一形状で prepared statement が増えない"
        );
        assert!(
            e2 > e1,
            "{force:?}: 既存の prepared statement が再実行される"
        );
        assert_eq!(
            after.misses, before.misses,
            "{force:?}: 全てプランキャッシュに当たる"
        );
        assert!(after.hits > before.hits);
    }
}

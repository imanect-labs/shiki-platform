//! 会話の分岐（発話の編集・枝の切り替え・新しいチャットに分岐・#1166）の結合テスト。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行。authz は全許可のモックに置き換え、
//! DB 上の木（parent_id）と表示中の枝（thread.active_message_id）の動きを検証する。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use chat::{ChatError, ChatStore};
use sqlx::{postgres::PgPoolOptions, PgPool};

/// 全許可のモック authz（`deny_all` で全拒否に切り替えられる）。
#[derive(Default)]
struct AllowAll {
    tuples: std::sync::Mutex<Vec<ReadTupleKey>>,
    /// 立てると以後の check を全て拒否する（権限の無い操作の再現）。
    deny_all: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl AuthzClient for AllowAll {
    async fn check(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: &FgaObject,
        _c: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(!self.deny_all.load(std::sync::atomic::Ordering::SeqCst))
    }
    async fn write_tuple(
        &self,
        s: &Subject,
        r: Relation,
        o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        self.tuples.lock().unwrap().push(ReadTupleKey {
            user: s.as_str().to_string(),
            relation: r.as_str().to_string(),
            object: o.as_str().to_string(),
        });
        Ok(true)
    }
    async fn delete_tuple(
        &self,
        s: &Subject,
        r: Relation,
        o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        let mut tuples = self.tuples.lock().unwrap();
        let before = tuples.len();
        tuples.retain(|t| {
            !(t.user == s.as_str() && t.relation == r.as_str() && t.object == o.as_str())
        });
        Ok(tuples.len() != before)
    }
    async fn read_tuples(
        &self,
        o: &FgaObject,
        r: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        Ok(self
            .tuples
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.object == o.as_str() && r.is_none_or(|r| t.relation == r.as_str()))
            .map(|t| ReadTupleKey {
                user: t.user.clone(),
                relation: t.relation.clone(),
                object: t.object.clone(),
            })
            .collect())
    }
    async fn list_objects(
        &self,
        _s: &Subject,
        _r: Relation,
        _t: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
    async fn delete_object_tuples(&self, _o: &FgaObject) -> Result<u32, AuthzError> {
        Ok(0)
    }
    async fn read_subject_objects(
        &self,
        _s: &Subject,
        _t: ObjectType,
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

fn ctx(tenant: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        authz::TenantId::parse(tenant).unwrap(),
    )
}

async fn store(pool: &PgPool) -> ChatStore {
    store_with(pool, Arc::new(AllowAll::default())).await
}

async fn store_with(pool: &PgPool, authz: Arc<AllowAll>) -> ChatStore {
    ChatStore::connect(pool.clone(), authz, None).await.unwrap()
}

/// 発話を送り、生成済みの回答を書き込む（ワーカーを回さずに本文を入れる）。(user, assistant)。
async fn turn(
    pool: &PgPool,
    store: &ChatStore,
    c: &AuthContext,
    thread_id: uuid::Uuid,
    text: &str,
    edit_of: Option<uuid::Uuid>,
) -> (uuid::Uuid, uuid::Uuid) {
    let r = store
        .post_message_on_branch(
            c,
            thread_id,
            text,
            &[],
            None,
            Some(false),
            false,
            &[],
            None,
            &[],
            chat::BranchTarget {
                edit_of,
                after: None,
            },
            None,
        )
        .await
        .unwrap();
    // 生成を終えた状態にする（未完了の run がある間は編集・切り替えを受けない）。
    sqlx::query("UPDATE generation_run SET status = 'done' WHERE run_id = $1")
        .bind(r.run_id)
        .execute(pool)
        .await
        .unwrap();
    let answer = serde_json::json!([{ "type": "text", "text": format!("{text}への回答") }]);
    sqlx::query("UPDATE message SET content = $1 WHERE id = $2")
        .bind(answer)
        .bind(r.assistant_message_id)
        .execute(pool)
        .await
        .unwrap();
    // 同じ秒に並ばないよう時刻を進める（作成順で「最も新しい」を決めるため）。
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    (r.user_message_id, r.assistant_message_id)
}

fn texts(msgs: &[chat::Message]) -> Vec<String> {
    msgs.iter()
        .map(|m| {
            m.content
                .iter()
                .filter_map(|b| match b {
                    chat::ContentBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect::<String>()
        })
        .collect()
}

/// 編集で枝ができ、表示は新しい枝・切り替えで元の枝（と続き）へ戻れる。続けて送ると表示中の枝に続く。
#[tokio::test]
async fn edit_creates_branch_and_switch_restores_it() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool).await;
    let c = ctx(&tenant);
    let thread = store
        .create_thread(&c, "会話", false, None, None)
        .await
        .unwrap();
    let (_u1, _a1) = turn(&pool, &store, &c, thread.id, "Q1", None).await;
    let (u2, _a2) = turn(&pool, &store, &c, thread.id, "Q2", None).await;
    let (_u3, _a3) = turn(&pool, &store, &c, thread.id, "Q3", None).await;

    // Q2 を直して送る → Q2 から後ろは別の枝になり、表示は新しい枝。
    let (u2b, a2b) = turn(&pool, &store, &c, thread.id, "Q2'", Some(u2)).await;
    let msgs = store.get_messages(&c, thread.id, None).await.unwrap();
    assert_eq!(texts(&msgs), ["Q1", "Q1への回答", "Q2'", "Q2'への回答"]);
    let edited = msgs.iter().find(|m| m.id == u2b).unwrap();
    assert_eq!(edited.sibling_ids, vec![u2, u2b], "兄弟は古い順");
    // 枝の無いメッセージには載せない。
    assert!(msgs[0].sibling_ids.is_empty());

    // 生成ワーカーの履歴は生成対象の祖先だけ（元の枝の Q3 を混ぜない）。
    let path = store
        .get_message_path(&c, thread.id, a2b, None)
        .await
        .unwrap();
    assert_eq!(texts(&path), ["Q1", "Q1への回答", "Q2'", "Q2'への回答"]);

    // 元の版へ切り替えると、その続き（Q3）まで戻る。
    store.select_branch(&c, thread.id, u2, None).await.unwrap();
    let msgs = store.get_messages(&c, thread.id, None).await.unwrap();
    assert_eq!(
        texts(&msgs),
        ["Q1", "Q1への回答", "Q2", "Q2への回答", "Q3", "Q3への回答"]
    );

    // 元の枝のまま送ると、その枝の続きになる（新しい枝の側は変わらない）。
    turn(&pool, &store, &c, thread.id, "Q4", None).await;
    let msgs = store.get_messages(&c, thread.id, None).await.unwrap();
    assert_eq!(texts(&msgs).last().unwrap(), "Q4への回答");
    assert_eq!(texts(&msgs)[4], "Q3");
    store.select_branch(&c, thread.id, u2b, None).await.unwrap();
    let msgs = store.get_messages(&c, thread.id, None).await.unwrap();
    assert_eq!(texts(&msgs), ["Q1", "Q1への回答", "Q2'", "Q2'への回答"]);
}

/// 最初の発話も編集できる（根に兄弟ができる）。
#[tokio::test]
async fn first_message_can_be_edited() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool).await;
    let c = ctx(&tenant);
    let thread = store
        .create_thread(&c, "会話", false, None, None)
        .await
        .unwrap();
    let (u1, _) = turn(&pool, &store, &c, thread.id, "Q1", None).await;
    turn(&pool, &store, &c, thread.id, "Q2", None).await;
    let (u1b, _) = turn(&pool, &store, &c, thread.id, "Q1'", Some(u1)).await;
    let msgs = store.get_messages(&c, thread.id, None).await.unwrap();
    assert_eq!(texts(&msgs), ["Q1'", "Q1'への回答"]);
    assert_eq!(msgs[0].sibling_ids, vec![u1, u1b]);
}

/// 編集元は同じスレッドの user 発話だけ。回答・他スレッド・存在しない id は NotFound。
#[tokio::test]
async fn edit_of_must_be_user_message_of_thread() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool).await;
    let c = ctx(&tenant);
    let thread = store
        .create_thread(&c, "会話", false, None, None)
        .await
        .unwrap();
    let other = store
        .create_thread(&c, "別", false, None, None)
        .await
        .unwrap();
    let (_, a1) = turn(&pool, &store, &c, thread.id, "Q1", None).await;
    let (u_other, _) = turn(&pool, &store, &c, other.id, "X", None).await;
    for bad in [a1, u_other, uuid::Uuid::new_v4()] {
        let r = store
            .post_message_on_branch(
                &c,
                thread.id,
                "直し",
                &[],
                None,
                None,
                false,
                &[],
                None,
                &[],
                chat::BranchTarget {
                    edit_of: Some(bad),
                    after: None,
                },
                None,
            )
            .await;
        assert!(matches!(r, Err(ChatError::NotFound)), "{bad}: {r:?}");
    }
    // 他スレッドのメッセージへは切り替えられない。
    assert!(matches!(
        store.select_branch(&c, thread.id, u_other, None).await,
        Err(ChatError::NotFound)
    ));
}

/// 新しいチャットに分岐: その回答までを（他の枝・後の会話を含めず）新しいスレッドへ複製する。
#[tokio::test]
async fn fork_copies_path_until_message() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool).await;
    let c = ctx(&tenant);
    let thread = store
        .create_thread(&c, "予算の相談", true, None, None)
        .await
        .unwrap();
    let (u1, a1) = turn(&pool, &store, &c, thread.id, "Q1", None).await;
    let (u2, a2) = turn(&pool, &store, &c, thread.id, "Q2", None).await;
    turn(&pool, &store, &c, thread.id, "Q3", None).await;
    turn(&pool, &store, &c, thread.id, "Q2'", Some(u2)).await;
    // a2 は手数の上限で止まった回答（GAP-106）。分岐しても止まった理由の注記が残る。
    sqlx::query("UPDATE message SET stop_reason = 'step_limit' WHERE id = $1")
        .bind(a2)
        .execute(&pool)
        .await
        .unwrap();

    let forked = store.fork_thread(&c, thread.id, a2, None).await.unwrap();
    assert_ne!(forked.id, thread.id);
    assert_eq!(forked.title, "予算の相談（分岐）");
    assert!(forked.agent_mode, "エージェントモードは元を継ぐ");
    let msgs = store.get_messages(&c, forked.id, None).await.unwrap();
    assert_eq!(texts(&msgs), ["Q1", "Q1への回答", "Q2", "Q2への回答"]);
    // 新しいメッセージ（元の id は持ち越さない）で、親は直前のもの。
    assert!(msgs.iter().all(|m| ![u1, a1, u2, a2].contains(&m.id)));
    assert_eq!(msgs[1].parent_id, Some(msgs[0].id));
    assert_eq!(msgs[2].parent_id, Some(msgs[1].id));
    assert!(msgs.iter().all(|m| m.sibling_ids.is_empty()));
    let stops: Vec<_> = msgs.iter().map(|m| m.stop_reason).collect();
    assert_eq!(stops, [None, None, None, Some(chat::StopReason::StepLimit)]);
    // 元の会話は変わらない（表示中は編集した枝のまま）。
    let src = store.get_messages(&c, thread.id, None).await.unwrap();
    assert_eq!(texts(&src)[2], "Q2'");
    // 複製した題名は自動タイトルで上書きしない。
    assert!(store
        .title_seed(forked.id, &tenant)
        .await
        .unwrap()
        .is_none());
    // 分岐した会話でそのまま続けられる。
    turn(&pool, &store, &c, forked.id, "Q5", None).await;
    let msgs = store.get_messages(&c, forked.id, None).await.unwrap();
    assert_eq!(texts(&msgs).len(), 6);

    // 存在しないメッセージ・他スレッドのメッセージからは分岐できない（新しい会話を作らない）。
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM thread WHERE tenant_id = $1")
        .bind(&tenant)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(matches!(
        store.fork_thread(&c, thread.id, msgs[0].id, None).await,
        Err(ChatError::NotFound)
    ));
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM thread WHERE tenant_id = $1")
        .bind(&tenant)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(before, after);
}

/// 閲覧権限が無ければ切り替え・分岐できない（Forbidden）。
#[tokio::test]
async fn branch_ops_require_relation() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let authz = Arc::new(AllowAll::default());
    let store = store_with(&pool, authz.clone()).await;
    let c = ctx(&tenant);
    let thread = store
        .create_thread(&c, "会話", false, None, None)
        .await
        .unwrap();
    let (u1, a1) = turn(&pool, &store, &c, thread.id, "Q1", None).await;
    authz
        .deny_all
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(matches!(
        store.select_branch(&c, thread.id, u1, None).await,
        Err(ChatError::Forbidden)
    ));
    assert!(matches!(
        store.fork_thread(&c, thread.id, a1, None).await,
        Err(ChatError::Forbidden)
    ));
}

/// 続ける先（`after`）: 別のタブが枝を切り替えていても、送った人が見ていた枝に続ける。
/// 生成中は編集・切り替えを受けず、`after` は無視して表示中の枝の後ろへ積む。
#[tokio::test]
async fn after_continues_seen_branch_and_busy_blocks_branch_ops() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool).await;
    let c = ctx(&tenant);
    let thread = store
        .create_thread(&c, "会話", false, None, None)
        .await
        .unwrap();
    let (u1, a1) = turn(&pool, &store, &c, thread.id, "Q1", None).await;
    let (_u1b, a1b) = turn(&pool, &store, &c, thread.id, "Q1'", Some(u1)).await;
    // 表示中は Q1'。古いタブは Q1 の枝（a1）を見たまま送る → Q1 の枝の続きになる。
    let r = store
        .post_message_on_branch(
            &c,
            thread.id,
            "Q2",
            &[],
            None,
            Some(false),
            false,
            &[],
            None,
            &[],
            chat::BranchTarget {
                edit_of: None,
                after: Some(a1),
            },
            None,
        )
        .await
        .unwrap();
    let msgs = store.get_messages(&c, thread.id, None).await.unwrap();
    assert_eq!(msgs[0].id, u1);
    assert_eq!(msgs[2].id, r.user_message_id);
    assert_eq!(msgs[2].parent_id, Some(a1));

    // 生成中（この run は未完了）は編集・切り替えを受けない。
    assert!(matches!(
        store.select_branch(&c, thread.id, a1b, None).await,
        Err(ChatError::Invalid(_))
    ));
    let edit = store
        .post_message_on_branch(
            &c,
            thread.id,
            "Q1''",
            &[],
            None,
            None,
            false,
            &[],
            None,
            &[],
            chat::BranchTarget {
                edit_of: Some(u1),
                after: None,
            },
            None,
        )
        .await;
    assert!(matches!(edit, Err(ChatError::Invalid(_))), "{edit:?}");
    // 生成中の `after` は無視して表示中の枝（生成中の回答）の後ろへ積む。
    let queued = store
        .post_message_on_branch(
            &c,
            thread.id,
            "Q3",
            &[],
            None,
            None,
            false,
            &[],
            None,
            &[],
            chat::BranchTarget {
                edit_of: None,
                after: Some(a1b),
            },
            None,
        )
        .await
        .unwrap();
    let path = store
        .get_message_path(&c, thread.id, queued.user_message_id, None)
        .await
        .unwrap();
    assert_eq!(path[path.len() - 2].id, r.assistant_message_id);

    // 存在しない続ける先は 404（生成が終わってから）。
    sqlx::query("UPDATE generation_run SET status = 'done' WHERE thread_id = $1")
        .bind(thread.id)
        .execute(&pool)
        .await
        .unwrap();
    let missing = store
        .post_message_on_branch(
            &c,
            thread.id,
            "Q4",
            &[],
            None,
            None,
            false,
            &[],
            None,
            &[],
            chat::BranchTarget {
                edit_of: None,
                after: Some(uuid::Uuid::new_v4()),
            },
            None,
        )
        .await;
    assert!(matches!(missing, Err(ChatError::NotFound)), "{missing:?}");
    // 発話（回答でないもの）からは分岐できない。
    assert!(matches!(
        store.fork_thread(&c, thread.id, u1, None).await,
        Err(ChatError::NotFound)
    ));
}

/// 実行前フェーズの門の材料（回答の ```ui）は生成対象の祖先だけ。編集で分かれた別の枝の
/// 質問・計画を根拠にしない。編集は元の run の生成材料（自律など）を引ける。
#[tokio::test]
async fn gate_material_and_edit_context_follow_branch() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool).await;
    let c = ctx(&tenant);
    let thread = store
        .create_thread(&c, "会話", true, None, None)
        .await
        .unwrap();
    let r = store
        .post_message(&c, thread.id, "Q1", &[], None, None, true, &[], None)
        .await
        .unwrap();
    let ask = serde_json::json!([{ "type": "text", "text": "確認です\n```ui\nask\n```" }]);
    sqlx::query("UPDATE message SET content = $1 WHERE id = $2")
        .bind(ask)
        .bind(r.assistant_message_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE generation_run SET status = 'done' WHERE run_id = $1")
        .bind(r.run_id)
        .execute(&pool)
        .await
        .unwrap();
    let on_first = store
        .fenced_answer_texts(thread.id, &tenant, r.assistant_message_id)
        .await
        .unwrap();
    assert_eq!(on_first.len(), 1);

    // 編集は元の run の生成材料（自律）を引ける。
    let inherited = store
        .edited_turn_context(&c, thread.id, r.user_message_id, None)
        .await
        .unwrap()
        .expect("元の run がある");
    assert!(inherited.autonomous);

    let (_, a1b) = turn(&pool, &store, &c, thread.id, "Q1'", Some(r.user_message_id)).await;
    let on_edited = store
        .fenced_answer_texts(thread.id, &tenant, a1b)
        .await
        .unwrap();
    assert!(
        on_edited.is_empty(),
        "別の枝の ```ui を材料にしない: {on_edited:?}"
    );
}

/// 続ける先が発話を指していたら（空の応答が画面に出ていない等）、その応答の側へ続ける。
#[tokio::test]
async fn after_pointing_to_user_follows_its_reply() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool).await;
    let c = ctx(&tenant);
    let thread = store
        .create_thread(&c, "会話", false, None, None)
        .await
        .unwrap();
    // 応答が空のまま終わった（停止・失敗）往復。
    let r = store
        .post_message(&c, thread.id, "Q1", &[], None, None, false, &[], None)
        .await
        .unwrap();
    sqlx::query("UPDATE generation_run SET status = 'cancelled' WHERE run_id = $1")
        .bind(r.run_id)
        .execute(&pool)
        .await
        .unwrap();
    let next = store
        .post_message_on_branch(
            &c,
            thread.id,
            "Q2",
            &[],
            None,
            None,
            false,
            &[],
            None,
            &[],
            chat::BranchTarget {
                edit_of: None,
                after: Some(r.user_message_id),
            },
            None,
        )
        .await
        .unwrap();
    let path = store
        .get_message_path(&c, thread.id, next.user_message_id, None)
        .await
        .unwrap();
    let ids: Vec<uuid::Uuid> = path.iter().map(|m| m.id).collect();
    assert_eq!(
        ids,
        vec![
            r.user_message_id,
            r.assistant_message_id,
            next.user_message_id
        ]
    );
    assert!(
        path.iter().all(|m| m.sibling_ids.is_empty()),
        "版は増えない"
    );
}

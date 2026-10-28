//! サイドバー履歴の行操作（名前の変更・ピン留め・削除）と自動タイトルの保存の結合テスト。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行。authz は全許可のモックに置き換え、
//! DB 上の状態（title_pending・pinned_at・deleted_at・run の停止要求）と、削除済みスレッドが
//! 配下の操作から閉じることを検証する。

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
use chat::{ChatError, ChatStore, ThreadRole};
use sqlx::{postgres::PgPoolOptions, PgPool};
use storage::model::ShareTarget;

/// 全許可のモック authz。共有の剥奪を観測するため、タプルを手元に保持する。
#[derive(Default)]
struct AllowAll {
    tuples: std::sync::Mutex<Vec<ReadTupleKey>>,
}

impl AllowAll {
    fn relations_on(&self, object: &str) -> Vec<String> {
        self.tuples
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.object == object)
            .map(|t| format!("{}#{}", t.relation, t.user))
            .collect()
    }
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
        Ok(true)
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
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
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
        tenant.into(),
    )
}

async fn store(pool: &PgPool) -> ChatStore {
    store_with(pool, Arc::new(AllowAll::default())).await
}

async fn store_with(pool: &PgPool, authz: Arc<AllowAll>) -> ChatStore {
    ChatStore::connect(pool.clone(), authz, None).await.unwrap()
}

async fn post(store: &ChatStore, c: &AuthContext, thread_id: uuid::Uuid, text: &str) -> uuid::Uuid {
    store
        .post_message(c, thread_id, text, &[], None, Some(false), false, &[], None)
        .await
        .unwrap()
        .run_id
}

/// 改名すると以後は自動タイトルで上書きされない（生成中に改名されても改名が勝つ）。
#[tokio::test]
async fn rename_wins_over_generated_title() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool).await;
    let c = ctx(&tenant);
    let thread = store
        .create_thread(&c, "経費精算の締め日は…", false, None, None)
        .await
        .unwrap();
    // 発話が無いうちは材料が無い。
    assert!(store
        .title_seed(thread.id, &tenant)
        .await
        .unwrap()
        .is_none());
    post(&store, &c, thread.id, "  経費精算の締め日は？  ").await;
    assert_eq!(
        store
            .title_seed(thread.id, &tenant)
            .await
            .unwrap()
            .as_deref(),
        Some("経費精算の締め日は？")
    );
    // 別テナントからは材料を引けない。
    assert!(store
        .title_seed(thread.id, "other")
        .await
        .unwrap()
        .is_none());

    let before = store.get_thread(&c, thread.id, None).await.unwrap();
    store
        .rename_thread(&c, thread.id, " 経費の\n締め日 ", None)
        .await
        .unwrap();
    assert!(
        !store
            .set_generated_title(thread.id, &tenant, "生成タイトル")
            .await
            .unwrap(),
        "改名後は生成タイトルを書かない"
    );
    assert!(store
        .title_seed(thread.id, &tenant)
        .await
        .unwrap()
        .is_none());
    let got = store.get_thread(&c, thread.id, None).await.unwrap();
    assert_eq!(got.title, "経費の 締め日");
    assert_eq!(
        got.updated_at, before.updated_at,
        "改名で履歴の並びを動かさない"
    );

    // 空のタイトルは拒否する。
    assert!(matches!(
        store.rename_thread(&c, thread.id, " \n", None).await,
        Err(ChatError::Invalid(_))
    ));
}

/// 生成タイトルは仮のときに 1 度だけ書ける。
#[tokio::test]
async fn generated_title_is_written_once() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool).await;
    let c = ctx(&tenant);
    let thread = store
        .create_thread(&c, "", false, None, None)
        .await
        .unwrap();
    assert!(store
        .set_generated_title(thread.id, &tenant, "会議室の予約方法")
        .await
        .unwrap());
    assert!(!store
        .set_generated_title(thread.id, &tenant, "別のタイトル")
        .await
        .unwrap());
    let got = store.get_thread(&c, thread.id, None).await.unwrap();
    assert_eq!(got.title, "会議室の予約方法");
}

/// ピン留めは別一覧に新しい順で出て、外すと消える。付け直しで時刻が跳ねない。
#[tokio::test]
async fn pin_and_unpin_threads() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool).await;
    let c = ctx(&tenant);
    let a = store
        .create_thread(&c, "A", false, None, None)
        .await
        .unwrap();
    let b = store
        .create_thread(&c, "B", false, None, None)
        .await
        .unwrap();
    assert!(store.list_pinned_threads(&c).await.unwrap().is_empty());

    store.set_thread_pinned(&c, a.id, true, None).await.unwrap();
    store.set_thread_pinned(&c, b.id, true, None).await.unwrap();
    let pinned = store.list_pinned_threads(&c).await.unwrap();
    let ids: Vec<_> = pinned.iter().map(|t| t.id).collect();
    assert_eq!(ids, vec![b.id, a.id], "ピン留めの新しい順");
    let a_pinned_at = pinned[1].pinned_at.expect("pinned_at が付く");

    store.set_thread_pinned(&c, a.id, true, None).await.unwrap();
    let again = store.get_thread(&c, a.id, None).await.unwrap();
    assert_eq!(
        again.pinned_at,
        Some(a_pinned_at),
        "付け直しで時刻を変えない"
    );
    assert_eq!(
        again.updated_at, a.updated_at,
        "ピン留めで履歴の並びを動かさない"
    );

    store
        .set_thread_pinned(&c, a.id, false, None)
        .await
        .unwrap();
    let ids: Vec<_> = store
        .list_pinned_threads(&c)
        .await
        .unwrap()
        .iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(ids, vec![b.id]);
    assert!(store
        .get_thread(&c, a.id, None)
        .await
        .unwrap()
        .pinned_at
        .is_none());

    // 他人（同テナントの別ユーザー）のピン留め一覧には出ない（一覧は本人のものだけ）。
    let mut bob = ctx(&tenant);
    bob.principal.id = "bob".into();
    assert!(store.list_pinned_threads(&bob).await.unwrap().is_empty());
}

/// 削除すると一覧・ピン留めから消え、スレッド配下の操作が NotFound になる。進行中の run は停止要求。
#[tokio::test]
async fn delete_hides_thread_and_cancels_runs() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool).await;
    let c = ctx(&tenant);
    let thread = store
        .create_thread(&c, "消す会話", false, None, None)
        .await
        .unwrap();
    let keep = store
        .create_thread(&c, "残す会話", false, None, None)
        .await
        .unwrap();
    store
        .set_thread_pinned(&c, thread.id, true, None)
        .await
        .unwrap();
    let run_id = post(&store, &c, thread.id, "生成中に消す").await;

    store.delete_thread(&c, thread.id, None).await.unwrap();

    let listed: Vec<_> = store
        .list_threads(&c, None, None, None, None, 50)
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(listed, vec![keep.id], "削除した会話は一覧に出ない");
    assert!(store.list_pinned_threads(&c).await.unwrap().is_empty());

    let cancel: bool =
        sqlx::query_scalar("SELECT cancel_requested FROM generation_run WHERE run_id = $1")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(cancel, "進行中の run に停止を要求する");

    // 配下の操作はすべて閉じる（共有相手の relation が残っていても開けない）。
    assert!(matches!(
        store.get_thread(&c, thread.id, None).await,
        Err(ChatError::NotFound)
    ));
    assert!(matches!(
        store.get_messages(&c, thread.id, None).await,
        Err(ChatError::NotFound)
    ));
    assert!(matches!(
        store
            .post_message(&c, thread.id, "x", &[], None, Some(false), false, &[], None)
            .await,
        Err(ChatError::NotFound)
    ));
    assert!(matches!(
        store.rename_thread(&c, thread.id, "x", None).await,
        Err(ChatError::NotFound)
    ));
    assert!(matches!(
        store.delete_thread(&c, thread.id, None).await,
        Err(ChatError::NotFound)
    ));
    assert!(store
        .title_seed(thread.id, &tenant)
        .await
        .unwrap()
        .is_none());
    // 残した会話はそのまま開ける。
    assert!(store.get_thread(&c, keep.id, None).await.is_ok());
}

/// 削除は共有（viewer / editor）をすべて剥奪する（owner のタプルは残す）。
#[tokio::test]
async fn delete_revokes_all_shares() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let authz = Arc::new(AllowAll::default());
    let store = store_with(&pool, authz.clone()).await;
    let c = ctx(&tenant);
    let thread = store
        .create_thread(&c, "共有した会話", false, None, None)
        .await
        .unwrap();
    for (id, role) in [("bob", ThreadRole::Viewer), ("carol", ThreadRole::Editor)] {
        store
            .share_thread(
                &c,
                thread.id,
                &ShareTarget::User { id: id.into() },
                role,
                None,
            )
            .await
            .unwrap();
    }
    let obj = c.ns().thread(&thread.id.to_string());
    assert_eq!(
        authz.relations_on(obj.as_str()).len(),
        3,
        "owner＋共有 2 件"
    );

    store.delete_thread(&c, thread.id, None).await.unwrap();
    let left = authz.relations_on(obj.as_str());
    assert_eq!(left.len(), 1, "共有は剥奪され owner だけ残る: {left:?}");
    assert!(left[0].starts_with("owner#"), "{left:?}");
}

/// ピン留めは上限まで。上限に達したら新しく付けられず、付け直し・外すことはできる。
#[tokio::test]
async fn pin_limit_is_enforced() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool).await;
    let c = ctx(&tenant);
    let limit = usize::try_from(chat::store::MAX_PINNED).unwrap();
    let mut ids = Vec::new();
    for i in 0..=limit {
        let t = store
            .create_thread(&c, &format!("会話 {i}"), false, None, None)
            .await
            .unwrap();
        ids.push(t.id);
    }
    for id in &ids[..limit] {
        store.set_thread_pinned(&c, *id, true, None).await.unwrap();
    }
    assert!(matches!(
        store.set_thread_pinned(&c, ids[limit], true, None).await,
        Err(ChatError::Invalid(_))
    ));
    // 付け直しは上限に数えない。外せば次を付けられる。
    store
        .set_thread_pinned(&c, ids[0], true, None)
        .await
        .unwrap();
    store
        .set_thread_pinned(&c, ids[0], false, None)
        .await
        .unwrap();
    store
        .set_thread_pinned(&c, ids[limit], true, None)
        .await
        .unwrap();
    assert_eq!(store.list_pinned_threads(&c).await.unwrap().len(), limit);
}

/// 自動タイトルを試すのはユーザーの発話が 3 件までの間（失敗し続けても毎回は呼ばない）。
#[tokio::test]
async fn title_seed_stops_after_three_user_messages() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool).await;
    let c = ctx(&tenant);
    let thread = store
        .create_thread(&c, "仮", false, None, None)
        .await
        .unwrap();
    for i in 1..=3 {
        post(&store, &c, thread.id, &format!("質問 {i}")).await;
    }
    let seed = store.title_seed(thread.id, &tenant).await.unwrap();
    assert_eq!(seed.as_deref(), Some("質問 1\n\n質問 2\n\n質問 3"));
    post(&store, &c, thread.id, "質問 4").await;
    assert!(store
        .title_seed(thread.id, &tenant)
        .await
        .unwrap()
        .is_none());
}

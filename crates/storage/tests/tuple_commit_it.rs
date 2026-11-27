//! FGA のタプルと commit の区間を、呼ぶ側のフューチャを捨てても最後まで走らせる（BUG-134）の結合テスト。
//! 実 Postgres・RustFS・OpenFGA が必要。
//!
//! `STORAGE_TEST_DATABASE_URL` と `OPENFGA_TEST_URL` が設定されている時のみ実行する（未設定ならスキップ・
//! `SHIKI_REQUIRE_IT=1` なら失敗）。
//!
//! 本物の AuthzClient を包む [`Gated`] で、決めたタプルの書き込み（外し）の前か後で区間を止め、止まったのを確かめて
//! から呼ぶ側のフューチャを捨てる。時間で打ち切ると区間の前か後で切れて揺れる（BUG-164 の型）ので、時間は
//! 「別のタスクが終わるのを待つ上限」にだけ使う。その後に、行とタプル（共有なら監査の行とタプル）が両方あるか、
//! 両方無いことを確かめる。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::cognitive_complexity
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use authz::client::{CheckTuple, OpenFgaClient, OpenFgaConfig, ReadTupleKey};
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal, Relation,
    Subject,
};
use sqlx::{postgres::PgPoolOptions, PgPool};
use storage::{
    object_store::S3Config, GeneralAccessLevel, ObjectStore, OnNameTaken, S3ObjectStore, ShareRole,
    ShareTarget, StorageError, StorageService,
};
use tokio::sync::oneshot;
use uuid::Uuid;

const TENANT: &str = "default";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Write,
    Delete,
}

/// 本物へ渡す前に止めるか、渡した後に止めるか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum When {
    Before,
    After,
}

/// 止めた書き込み（外し）の続け方。
enum Release {
    /// そのまま進める（Before なら本物へ渡す・After なら本物の結果を返す）。
    Go,
    /// `Err` を返す（Before なら本物へ渡さない・After なら適用した後に `Err`）。
    Fail,
}

struct Gate {
    op: Op,
    relation: Relation,
    when: When,
    reached: oneshot::Sender<()>,
    release: oneshot::Receiver<Release>,
}

/// 本物の OpenFGA へ渡しつつ、決めた 1 つの書き込み（外し）を関所で止める・適用した後に `Err` を返す。
struct Gated {
    inner: Arc<dyn AuthzClient>,
    gate: Mutex<Option<Gate>>,
    /// この relation の書き込みは、適用した後に `Err` を返す（応答だけ失われた形）。
    fail_after_apply: Mutex<Option<Relation>>,
}

fn fake_err() -> AuthzError {
    AuthzError::Unexpected {
        status: 503,
        body: "gated".into(),
    }
}

impl Gated {
    fn arm(
        &self,
        op: Op,
        relation: Relation,
        when: When,
    ) -> (oneshot::Receiver<()>, oneshot::Sender<Release>) {
        let (reached, reached_rx) = oneshot::channel();
        let (release_tx, release) = oneshot::channel();
        *self.gate.lock().unwrap() = Some(Gate {
            op,
            relation,
            when,
            reached,
            release,
        });
        (reached_rx, release_tx)
    }

    fn take_gate(&self, op: Op, relation: Relation) -> Option<Gate> {
        let mut g = self.gate.lock().unwrap();
        if g.as_ref()
            .is_some_and(|g| g.op == op && g.relation == relation)
        {
            g.take()
        } else {
            None
        }
    }

    /// 関所を通す。`Err` を返すなら、その誤りで返す。
    async fn pass(gate: Gate) -> Result<(), AuthzError> {
        let _ = gate.reached.send(());
        match gate.release.await {
            Ok(Release::Go) => Ok(()),
            Ok(Release::Fail) | Err(_) => Err(fake_err()),
        }
    }

    async fn gated<F>(&self, op: Op, relation: Relation, call: F) -> Result<bool, AuthzError>
    where
        F: std::future::Future<Output = Result<bool, AuthzError>>,
    {
        let gate = self.take_gate(op, relation);
        let (before, after) = match gate {
            Some(g) if g.when == When::Before => (Some(g), None),
            Some(g) => (None, Some(g)),
            None => (None, None),
        };
        if let Some(g) = before {
            Self::pass(g).await?;
        }
        let r = call.await;
        if let Some(g) = after {
            Self::pass(g).await?;
        }
        if op == Op::Write && *self.fail_after_apply.lock().unwrap() == Some(relation) {
            r?;
            return Err(fake_err());
        }
        r
    }
}

#[async_trait]
impl AuthzClient for Gated {
    async fn check(
        &self,
        s: &Subject,
        r: Relation,
        o: &FgaObject,
        c: Consistency,
    ) -> Result<bool, AuthzError> {
        self.inner.check(s, r, o, c).await
    }
    async fn batch_check(
        &self,
        items: &[CheckTuple],
        c: Consistency,
    ) -> Result<Vec<bool>, AuthzError> {
        self.inner.batch_check(items, c).await
    }
    async fn write_tuple(
        &self,
        s: &Subject,
        r: Relation,
        o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        self.gated(Op::Write, r, self.inner.write_tuple(s, r, o))
            .await
    }
    async fn delete_tuple(
        &self,
        s: &Subject,
        r: Relation,
        o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        self.gated(Op::Delete, r, self.inner.delete_tuple(s, r, o))
            .await
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

struct Ctx {
    service: StorageService,
    pool: PgPool,
    /// 本物（関所を通らない・確かめと seed に使う）。
    fga: Arc<dyn AuthzClient>,
    gated: Arc<Gated>,
}

async fn setup() -> Option<Ctx> {
    let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let Ok(openfga_url) = shiki_testenv::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return None;
    };
    let s3_endpoint = std::env::var("STORAGE_TEST_S3_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:9000".into());
    let access_key =
        std::env::var("STORAGE_TEST_S3_ACCESS_KEY").unwrap_or_else(|_| "minioadmin".into());
    let secret_key =
        std::env::var("STORAGE_TEST_S3_SECRET_KEY").unwrap_or_else(|_| "minioadmin".into());

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
            base_url: openfga_url,
            store_name: format!("shiki-tuple-commit-it-{}", Uuid::new_v4().simple()),
        },
        &authz::model::default_model(),
    )
    .await
    .expect("OpenFGA へ接続できること");
    let fga: Arc<dyn AuthzClient> = Arc::new(fga);
    let gated = Arc::new(Gated {
        inner: fga.clone(),
        gate: Mutex::new(None),
        fail_after_apply: Mutex::new(None),
    });

    let s3 = S3Config {
        internal_endpoint: s3_endpoint.clone(),
        public_endpoint: s3_endpoint,
        bucket: "shiki-it-blobs".into(),
        access_key,
        secret_key,
        region: "us-east-1".into(),
        presign_get_ttl_secs: 300,
        presign_put_ttl_secs: 900,
        cors_allowed_origins: vec![],
    };
    let store: Arc<dyn ObjectStore> = Arc::new(S3ObjectStore::new(&s3));
    store.ensure_bucket().await.expect("バケット準備");

    let service = StorageService::new(
        pool.clone(),
        store,
        gated.clone(),
        Duration::from_secs(300),
        Duration::from_secs(900),
        5 * 1024 * 1024 * 1024,
    );
    Some(Ctx {
        service,
        pool,
        fga,
        gated,
    })
}

fn make_ctx(org: &str, uid: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: uid.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: None,
        },
        org.into(),
        authz::TenantId::parse(TENANT).unwrap(),
    )
}

/// org のメンバーを seed し、見えるまで待つ（作成の認可は member@org）。
async fn new_member(c: &Ctx, org: &str) -> AuthContext {
    let ctx = make_ctx(org, &format!("tcuser{}", Uuid::new_v4().simple()));
    let org_obj = ctx.ns().organization(org);
    c.fga
        .write_tuple(&ctx.subject(), Relation::Member, &org_obj)
        .await
        .expect("member tuple seed");
    for _ in 0..200 {
        if c.fga
            .check(
                &ctx.subject(),
                Relation::Member,
                &org_obj,
                Consistency::MinimizeLatency,
            )
            .await
            .expect("member check")
        {
            return ctx;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("seed した org member タプルが見えませんでした");
}

fn new_org() -> String {
    format!("tcorg{}", Uuid::new_v4().simple())
}

/// オブジェクトに直接張られたタプル（subject, relation）。
async fn tuples_of(fga: &Arc<dyn AuthzClient>, obj: &FgaObject) -> Vec<(String, String)> {
    fga.read_tuples(obj, None)
        .await
        .expect("read_tuples")
        .into_iter()
        .map(|t| (t.user, t.relation))
        .collect()
}

fn has(tuples: &[(String, String)], subject: &Subject, relation: Relation) -> bool {
    tuples
        .iter()
        .any(|(u, r)| u == subject.as_str() && r == relation.as_str())
}

/// 別のタスクが進むのを待つ（打ち切りの点は関所で決めるので、ここの時間は観測の上限だけ）。
async fn eventually<F, Fut>(what: &str, mut cond: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..400 {
        if cond().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("10 秒待っても {what} になりませんでした");
}

/// フォルダの中の生きているファイル（名前で引く）。
async fn file_in(pool: &PgPool, parent: Uuid, name: &str) -> Option<Uuid> {
    sqlx::query_scalar(
        "SELECT id FROM node WHERE tenant_id = $1 AND parent_id = $2 AND name = $3 \
         AND kind = 'file' AND deleted_at IS NULL",
    )
    .bind(TENANT)
    .bind(parent)
    .bind(name)
    .fetch_optional(pool)
    .await
    .unwrap()
}

async fn save(
    c: &Ctx,
    ctx: &AuthContext,
    parent: Uuid,
    name: &str,
    key: &str,
) -> Result<serde_json::Value, StorageError> {
    c.service
        .write_file_internal_idempotent(
            ctx,
            Some(parent),
            name,
            b"# tuple commit",
            "text/markdown",
            OnNameTaken::Fail,
            key,
            "digest-tuple-commit-it",
            None,
        )
        .await
}

/// parent の書き込みの前で止め、呼ぶ側のフューチャを捨てても、区間は commit まで走って行とタプルが両方残る。
/// 冪等の storage.write をやり直すと、dedup で同じノードが返る。
#[tokio::test]
async fn dropped_storage_write_runs_to_commit() {
    let Some(c) = setup().await else { return };
    let org = new_org();
    let ctx = new_member(&c, &org).await;
    let folder = c
        .service
        .create_folder(&ctx, None, "保存先", None)
        .await
        .unwrap();
    let key = format!("wf:tuple-commit:{}", Uuid::new_v4().simple());

    let (reached, release) = c.gated.arm(Op::Write, Relation::Parent, When::Before);
    let call = save(&c, &ctx, folder.id, "捨てた.md", &key);
    tokio::select! {
        r = call => panic!("parent の書き込みの前で止まるはず: {r:?}"),
        _ = reached => {}
    }
    // 呼ぶ側のフューチャは捨てた（owner を書いた後・parent の前）。区間を進める。
    let _ = release.send(Release::Go);

    let pool = c.pool.clone();
    eventually("ファイルの行が commit される", || {
        let pool = pool.clone();
        async move { file_in(&pool, folder.id, "捨てた.md").await.is_some() }
    })
    .await;
    let id = file_in(&c.pool, folder.id, "捨てた.md").await.unwrap();
    let obj = ctx.ns().file(&id.to_string());
    let tuples = tuples_of(&c.fga, &obj).await;
    assert!(
        has(&tuples, &ctx.subject(), Relation::Owner),
        "owner がある: {tuples:?}"
    );
    assert!(
        has(
            &tuples,
            &Subject::object(&ctx.ns().folder(&folder.id.to_string())),
            Relation::Parent
        ),
        "parent がある: {tuples:?}"
    );

    // やり直しは dedup で同じノードを返す（書き込みは 1 回のまま）。
    let again = save(&c, &ctx, folder.id, "捨てた.md", &key).await.unwrap();
    assert_eq!(again["id"], id.to_string(), "{again}");
    let read = c.service.read_file_internal(&ctx, id, None).await.unwrap();
    assert_eq!(read.1, b"# tuple commit");
}

/// parent の書き込みの前で止めて呼ぶ側を捨て、その書き込みが失敗すると、補償で owner も外れ、行も無い。
/// 冪等キーの予約も同じ Tx で巻き戻るので、やり直すと新しく書ける。
#[tokio::test]
async fn dropped_storage_write_compensates_on_parent_error() {
    let Some(c) = setup().await else { return };
    let org = new_org();
    let ctx = new_member(&c, &org).await;
    let folder = c
        .service
        .create_folder(&ctx, None, "保存先", None)
        .await
        .unwrap();
    let key = format!("wf:tuple-commit:{}", Uuid::new_v4().simple());

    // owner の subject で張られた file のタプルを、タスクが外すまで追う（file の id は commit 前なので見えない）。
    let owner_files = || {
        let (fga, subject) = (c.fga.clone(), ctx.subject());
        async move {
            fga.read_subject_objects(&subject, ObjectType::File)
                .await
                .expect("read_subject_objects")
        }
    };
    let (reached, release) = c.gated.arm(Op::Write, Relation::Parent, When::Before);
    let call = save(&c, &ctx, folder.id, "失敗.md", &key);
    tokio::select! {
        r = call => panic!("parent の書き込みの前で止まるはず: {r:?}"),
        _ = reached => {}
    }
    assert_eq!(owner_files().await.len(), 1, "owner は書いた後で止まった");
    let _ = release.send(Release::Fail);

    eventually("補償で owner が外れる", || {
        let files = owner_files();
        async move { files.await.is_empty() }
    })
    .await;
    assert_eq!(
        file_in(&c.pool, folder.id, "失敗.md").await,
        None,
        "行も無い"
    );

    // 予約も巻き戻ったので、やり直すと新しく書ける（行とタプルが両方ある）。
    let again = save(&c, &ctx, folder.id, "失敗.md", &key).await.unwrap();
    let id = file_in(&c.pool, folder.id, "失敗.md").await.unwrap();
    assert_eq!(again["id"], id.to_string());
    let tuples = tuples_of(&c.fga, &ctx.ns().file(&id.to_string())).await;
    assert!(has(&tuples, &ctx.subject(), Relation::Owner), "{tuples:?}");
}

/// 共有リンクの台帳の行（redeem した人）。
async fn grant_rows(pool: &PgPool, link_id: Uuid, user_id: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM node_share_link_grant \
         WHERE tenant_id = $1 AND link_id = $2 AND user_id = $3",
    )
    .bind(TENANT)
    .bind(link_id)
    .bind(user_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// redeem の via_link の書き込みが「適用した後に Err」でも、孤児の via_link タプルを残さない（台帳の行も無い）。
/// 呼ぶ側を書き込みの前で捨てても、区間は commit まで走って台帳の行とタプルが両方残る。
#[tokio::test]
async fn redeem_leaves_no_orphan_via_link_tuple() {
    let Some(c) = setup().await else { return };
    let org = new_org();
    let owner = new_member(&c, &org).await;
    let member = new_member(&c, &org).await;
    let file = c
        .service
        .write_file_internal(&owner, None, "鍵付き.txt", b"secret", "text/plain", None)
        .await
        .unwrap();
    let link = c
        .service
        .create_share_link(
            &owner,
            file.id,
            GeneralAccessLevel::Organization,
            ShareRole::Viewer,
            None,
            Some("pw-tuple-commit"),
            None,
            None,
        )
        .await
        .unwrap();
    let obj = owner.ns().file(&file.id.to_string());
    let member_subject = member.ns().user(&member.principal().id);

    // (a) 適用した後に Err → 補償で外す・台帳の行は巻き戻る。
    *c.gated.fail_after_apply.lock().unwrap() = Some(Relation::ViewerViaLink);
    let r = c
        .service
        .redeem_share_link(&member, &link.token, Some("pw-tuple-commit"), None)
        .await;
    *c.gated.fail_after_apply.lock().unwrap() = None;
    assert!(matches!(r, Err(StorageError::Authz(_))), "{r:?}");
    let tuples = tuples_of(&c.fga, &obj).await;
    assert!(
        !has(&tuples, &member_subject, Relation::ViewerViaLink),
        "孤児の via_link タプルが残った: {tuples:?}"
    );
    assert_eq!(
        grant_rows(&c.pool, link.link_id, &member.principal().id).await,
        0
    );

    // 書き込みの前で呼ぶ側を捨てる → 区間は commit まで走る。
    let (reached, release) = c
        .gated
        .arm(Op::Write, Relation::ViewerViaLink, When::Before);
    let call = c
        .service
        .redeem_share_link(&member, &link.token, Some("pw-tuple-commit"), None);
    tokio::select! {
        r = call => panic!("via_link の書き込みの前で止まるはず: {r:?}"),
        _ = reached => {}
    }
    let _ = release.send(Release::Go);
    let pool = c.pool.clone();
    let link_id = link.link_id;
    let uid = member.principal().id.clone();
    eventually("台帳の行が commit される", || {
        let (pool, uid) = (pool.clone(), uid.clone());
        async move { grant_rows(&pool, link_id, &uid).await == 1 }
    })
    .await;
    let tuples = tuples_of(&c.fga, &obj).await;
    assert!(
        has(&tuples, &member_subject, Relation::ViewerViaLink),
        "台帳の行があるのに via_link タプルが無い: {tuples:?}"
    );
}

async fn share_audits(pool: &PgPool, node_id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM audit_log \
         WHERE tenant_id = $1 AND action = 'node.share' AND object_id = $2 AND decision = 'allow'",
    )
    .bind(TENANT)
    .bind(node_id.to_string())
    .fetch_one(pool)
    .await
    .unwrap()
}

/// 共有のタプルを書いた後で呼ぶ側を捨てても、監査まで走って「タプルと監査の行が両方ある」で終わる（NFR-6）。
#[tokio::test]
async fn dropped_share_keeps_tuple_and_audit_together() {
    let Some(c) = setup().await else { return };
    let org = new_org();
    let owner = new_member(&c, &org).await;
    let file = c
        .service
        .write_file_internal(&owner, None, "共有.txt", b"share", "text/plain", None)
        .await
        .unwrap();
    let target_id = format!("tcpeer{}", Uuid::new_v4().simple());
    let target = ShareTarget::User {
        id: target_id.clone(),
    };

    let (reached, release) = c.gated.arm(Op::Write, Relation::Viewer, When::After);
    let call = c
        .service
        .share_node(&owner, file.id, &target, ShareRole::Viewer, None);
    tokio::select! {
        r = call => panic!("viewer の書き込みの後で止まるはず: {r:?}"),
        _ = reached => {}
    }
    assert_eq!(share_audits(&c.pool, file.id).await, 0, "監査はまだ");
    let _ = release.send(Release::Go);

    let pool = c.pool.clone();
    let node_id = file.id;
    eventually("共有の監査が残る", || {
        let pool = pool.clone();
        async move { share_audits(&pool, node_id).await == 1 }
    })
    .await;
    let tuples = tuples_of(&c.fga, &owner.ns().file(&file.id.to_string())).await;
    assert!(
        has(&tuples, &owner.ns().user(&target_id), Relation::Viewer),
        "監査があるのに共有のタプルが無い: {tuples:?}"
    );
}

/// move で旧親を外した後に呼ぶ側を捨てても、commit と新親の付与まで走る（親の行とタプルがそろう）。
#[tokio::test]
async fn dropped_move_runs_to_new_parent() {
    let Some(c) = setup().await else { return };
    let org = new_org();
    let ctx = new_member(&c, &org).await;
    let from = c
        .service
        .create_folder(&ctx, None, "前", None)
        .await
        .unwrap();
    let to = c
        .service
        .create_folder(&ctx, None, "後", None)
        .await
        .unwrap();
    let file = c
        .service
        .write_file_internal(&ctx, Some(from.id), "動く.txt", b"move", "text/plain", None)
        .await
        .unwrap();
    let obj = ctx.ns().file(&file.id.to_string());
    let from_subject = Subject::object(&ctx.ns().folder(&from.id.to_string()));
    let to_subject = Subject::object(&ctx.ns().folder(&to.id.to_string()));

    let (reached, release) = c.gated.arm(Op::Delete, Relation::Parent, When::After);
    let call = c.service.move_file(&ctx, file.id, Some(to.id), None);
    tokio::select! {
        r = call => panic!("旧親を外した後で止まるはず: {r:?}"),
        _ = reached => {}
    }
    let _ = release.send(Release::Go);

    eventually("新親の parent タプルが付く", || {
        let (fga, obj, to_subject) = (c.fga.clone(), obj.clone(), to_subject.clone());
        async move { has(&tuples_of(&fga, &obj).await, &to_subject, Relation::Parent) }
    })
    .await;
    assert_eq!(
        file_in(&c.pool, to.id, "動く.txt").await,
        Some(file.id),
        "行も新しい親の下にある"
    );
    assert!(
        !has(
            &tuples_of(&c.fga, &obj).await,
            &from_subject,
            Relation::Parent
        ),
        "旧親のタプルは外れている"
    );
}

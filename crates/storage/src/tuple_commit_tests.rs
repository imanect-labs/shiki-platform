//! [`super`] の単体テスト（DB も OpenFGA も要らない・偽の AuthzClient で区間を止めて確かめる）。

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use authz::client::ReadTupleKey;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal, Relation,
    Subject, TenantId,
};
use tokio::sync::oneshot;
use uuid::Uuid;

use super::{commit_with_tuples, node_creation_tuples, TupleCommitError, TupleKey};
use crate::error::StorageError;

type Key = (String, Relation, String);

fn key(t: &TupleKey) -> Key {
    (
        t.subject.as_str().to_string(),
        t.relation,
        t.object.as_str().to_string(),
    )
}

/// 関所で止めた書き込みの続け方。
enum Release {
    /// 書いて `Ok` を返す。
    Apply,
    /// 書かずに `Err` を返す。
    Fail,
}

/// その relation の書き込みが届いたら `reached` を送り、`release` を待つ。
struct Gate {
    relation: Relation,
    reached: oneshot::Sender<()>,
    release: oneshot::Receiver<Release>,
}

/// タプルを集合で持つ偽の AuthzClient。書き込みを関所で止める・適用した後に `Err` を返すことができる。
#[derive(Default)]
struct FakeAuthz {
    tuples: Mutex<HashSet<Key>>,
    gate: Mutex<Option<Gate>>,
    /// この relation の書き込みは、適用した後に `Err` を返す（応答だけ失われた形）。
    fail_after_apply: Mutex<Option<Relation>>,
    /// 外した順（補償の順を確かめる）。
    deleted: Mutex<Vec<Relation>>,
}

impl FakeAuthz {
    fn arm_gate(&self, relation: Relation) -> (oneshot::Receiver<()>, oneshot::Sender<Release>) {
        let (reached, reached_rx) = oneshot::channel();
        let (release_tx, release) = oneshot::channel();
        *self.gate.lock().unwrap() = Some(Gate {
            relation,
            reached,
            release,
        });
        (reached_rx, release_tx)
    }

    fn has(&self, t: &TupleKey) -> bool {
        self.tuples.lock().unwrap().contains(&key(t))
    }

    fn seed(&self, t: &TupleKey) {
        self.tuples.lock().unwrap().insert(key(t));
    }

    fn deleted(&self) -> Vec<Relation> {
        self.deleted.lock().unwrap().clone()
    }
}

fn fake_err() -> AuthzError {
    AuthzError::Unexpected {
        status: 503,
        body: "fake".into(),
    }
}

#[async_trait]
impl AuthzClient for FakeAuthz {
    async fn check(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: &FgaObject,
        _c: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(false)
    }

    async fn write_tuple(
        &self,
        s: &Subject,
        r: Relation,
        o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        let gate = {
            let mut g = self.gate.lock().unwrap();
            if g.as_ref().is_some_and(|g| g.relation == r) {
                g.take()
            } else {
                None
            }
        };
        if let Some(gate) = gate {
            let _ = gate.reached.send(());
            match gate.release.await {
                Ok(Release::Apply) => {}
                Ok(Release::Fail) | Err(_) => return Err(fake_err()),
            }
        }
        let applied =
            self.tuples
                .lock()
                .unwrap()
                .insert((s.as_str().to_string(), r, o.as_str().to_string()));
        if *self.fail_after_apply.lock().unwrap() == Some(r) {
            return Err(fake_err());
        }
        Ok(applied)
    }

    async fn delete_tuple(
        &self,
        s: &Subject,
        r: Relation,
        o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        self.deleted.lock().unwrap().push(r);
        Ok(self
            .tuples
            .lock()
            .unwrap()
            .remove(&(s.as_str().to_string(), r, o.as_str().to_string())))
    }

    async fn read_tuples(
        &self,
        _o: &FgaObject,
        _r: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        Ok(vec![])
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

fn ctx() -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: None,
        },
        "org1".into(),
        TenantId::parse("t1").unwrap(),
    )
}

/// owner と parent（作成の 2 つ）。
fn creation() -> (TupleKey, TupleKey) {
    let ctx = ctx();
    let file = ctx.ns().file(&Uuid::new_v4().to_string());
    let t = node_creation_tuples(&ctx, Some(Uuid::new_v4()), &file);
    assert_eq!(t.len(), 2);
    assert_eq!(t[0].relation, Relation::Owner);
    assert_eq!(t[1].relation, Relation::Parent);
    (t[0].clone(), t[1].clone())
}

/// 呼ぶ前には動かない finish。待たれたら `ran` に送る。
async fn finish_ok(ran: oneshot::Sender<()>) -> Result<(), &'static str> {
    let _ = ran.send(());
    Ok(())
}

/// 別のタスクが進むのを待つ（打ち切りの点は関所で決めるので、ここの時間は観測の上限だけ）。
async fn wait_until(mut cond: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !cond() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("5 秒以内に区間が終わること");
}

#[tokio::test]
async fn dropped_caller_still_runs_to_finish() {
    let fake = Arc::new(FakeAuthz::default());
    let (owner, parent) = creation();
    let (reached, release) = fake.arm_gate(Relation::Parent);
    let (ran_tx, ran) = oneshot::channel();
    let call = commit_with_tuples(
        fake.clone(),
        vec![owner.clone(), parent.clone()],
        finish_ok(ran_tx),
    );
    // owner を書いた後・parent の書き込みの途中で止まったのを確かめてから、呼ぶ側のフューチャを捨てる。
    tokio::select! {
        _ = call => panic!("関所で止まるはず"),
        _ = reached => {}
    }
    assert!(fake.has(&owner), "owner は書いた後");
    let _ = release.send(Release::Apply);
    ran.await.expect("呼ぶ側を捨てても finish まで走る");
    assert!(fake.has(&owner) && fake.has(&parent), "タプルは両方残る");
    assert!(fake.deleted().is_empty(), "補償は走らない");
}

#[tokio::test]
async fn dropped_caller_still_compensates_on_write_error() {
    let fake = Arc::new(FakeAuthz::default());
    let (owner, parent) = creation();
    let (reached, release) = fake.arm_gate(Relation::Parent);
    let (ran_tx, ran) = oneshot::channel();
    let call = commit_with_tuples(
        fake.clone(),
        vec![owner.clone(), parent.clone()],
        finish_ok(ran_tx),
    );
    tokio::select! {
        _ = call => panic!("関所で止まるはず"),
        _ = reached => {}
    }
    let _ = release.send(Release::Fail);
    // finish は一度も待たれずに捨てられる（送り手が送らずに drop される）。
    assert!(ran.await.is_err(), "書き込みの誤りでは finish を待たない");
    wait_until(|| fake.deleted().len() == 2).await;
    assert!(!fake.has(&owner) && !fake.has(&parent), "タプルは両方無い");
}

#[tokio::test]
async fn write_error_after_apply_removes_owner_and_parent() {
    let fake = Arc::new(FakeAuthz::default());
    let (owner, parent) = creation();
    *fake.fail_after_apply.lock().unwrap() = Some(Relation::Parent);
    let (ran_tx, ran) = oneshot::channel();
    let r = commit_with_tuples(
        fake.clone(),
        vec![owner.clone(), parent.clone()],
        finish_ok(ran_tx),
    )
    .await;
    assert!(matches!(r, Err(TupleCommitError::Write(_))), "{r:?}");
    assert!(ran.await.is_err(), "finish は待たない");
    assert!(
        !fake.has(&owner) && !fake.has(&parent),
        "適用された parent も外す"
    );
    assert_eq!(
        fake.deleted(),
        vec![Relation::Parent, Relation::Owner],
        "逆順に外す"
    );
}

#[tokio::test]
async fn write_error_keeps_preexisting_tuple() {
    let fake = Arc::new(FakeAuthz::default());
    let (owner, parent) = creation();
    fake.seed(&owner);
    *fake.fail_after_apply.lock().unwrap() = Some(Relation::Parent);
    let (ran_tx, _ran) = oneshot::channel();
    let r = commit_with_tuples(
        fake.clone(),
        vec![owner.clone(), parent.clone()],
        finish_ok(ran_tx),
    )
    .await;
    assert!(matches!(r, Err(TupleCommitError::Write(_))), "{r:?}");
    assert!(fake.has(&owner), "もともとあった（Ok(false)）タプルは残す");
    assert!(!fake.has(&parent), "失敗したタプルは外す");
}

#[tokio::test]
async fn finish_error_removes_only_fresh_tuples() {
    let fake = Arc::new(FakeAuthz::default());
    let (owner, parent) = creation();
    fake.seed(&parent);
    let r: Result<(), _> =
        commit_with_tuples(fake.clone(), vec![owner.clone(), parent.clone()], async {
            Err("commit")
        })
        .await;
    assert!(
        matches!(r, Err(TupleCommitError::Finish("commit"))),
        "{r:?}"
    );
    assert!(!fake.has(&owner), "新しく書いた（Ok(true)）owner は外す");
    assert!(
        fake.has(&parent),
        "もともとあった（Ok(false)）parent は残す"
    );
    assert_eq!(fake.deleted(), vec![Relation::Owner]);
}

#[tokio::test]
async fn finish_value_is_returned() {
    let fake = Arc::new(FakeAuthz::default());
    let (owner, _) = creation();
    let r: Result<u32, TupleCommitError<()>> =
        commit_with_tuples(fake.clone(), vec![owner.clone()], async { Ok(7) }).await;
    assert_eq!(r.unwrap(), 7);
    assert!(fake.has(&owner));
}

#[tokio::test]
async fn empty_tuples_just_awaits_finish() {
    let fake = Arc::new(FakeAuthz::default());
    let r: Result<(), TupleCommitError<&str>> =
        commit_with_tuples(fake.clone(), vec![], async { Err("commit") }).await;
    assert!(matches!(r, Err(TupleCommitError::Finish("commit"))));
    assert!(fake.deleted().is_empty());
}

#[tokio::test]
async fn panicking_task_is_aborted_and_maps_to_internal() {
    let fake = Arc::new(FakeAuthz::default());
    let (owner, _) = creation();
    let r: Result<(), TupleCommitError<sqlx::Error>> =
        commit_with_tuples(fake.clone(), vec![owner], async {
            panic!("finish の中の panic");
        })
        .await;
    assert!(matches!(r, Err(TupleCommitError::Aborted(_))), "{r:?}");
    let mapped: StorageError = r.unwrap_err().into();
    assert!(matches!(mapped, StorageError::Db(_)), "{mapped:?}");
}

#[test]
fn errors_map_to_storage_error() {
    let w: StorageError = TupleCommitError::<sqlx::Error>::Write(fake_err()).into();
    assert!(matches!(w, StorageError::Authz(_)));
    let f: StorageError = TupleCommitError::Finish(StorageError::Conflict).into();
    assert!(matches!(f, StorageError::Conflict));
}

#[test]
fn creation_tuples_without_parent_is_owner_only() {
    let ctx = ctx();
    let folder = ctx.ns().folder(&Uuid::new_v4().to_string());
    let t = node_creation_tuples(&ctx, None, &folder);
    assert_eq!(
        t,
        vec![TupleKey::new(ctx.subject(), Relation::Owner, folder)]
    );
}

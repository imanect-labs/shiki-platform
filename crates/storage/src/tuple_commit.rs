//! FGA のタプルを書いてから commit するまでの区間を、別のタスクで最後まで走らせる（BUG-134）。
//!
//! ノードの作成・共有リンクの解錠（redeem）は、DB の Tx の中身を確定させてから **commit の前に** FGA のタプル
//! （owner・parent・via_link）を書き、commit に失敗したらタプルを外す。「行があればタプルもある」を保つための順で、
//! 逆（commit してからタプル）にすると、見えない行と索引ができ、冪等のやり直しが見えないファイルを返す。
//!
//! この区間を呼ぶ側のフューチャの中で走らせると、フューチャが捨てられた（ワーカーの時間切れ・HTTP の切断・チャットの
//! 中止）ときに、タプルを書いた後・commit の前で止まる。Tx は drop で巻き戻るが、書いたタプルは外れずに残り、
//! **DB に無いノードの owner・parent のタプル**（redeem なら台帳から参照されない via_link のタプル）になる。
//! そこで区間を `tokio::spawn` した別のタスクで走らせ、呼ぶ側はその終わりを待つだけにする。呼ぶ側が捨てられても
//! タスクは最後まで走り、「commit まで済んでタプルが残る」か「補償でタプルが無い」のどちらかで終わる。
//! 残るのはプロセスが落ちたとき（kill・OOM・停止）だけで、その孤児のタプルは PIT-38 の復元後の整合検証
//! （孤児のタプルの検出）で拾う。
//!
//! # 補償の決まり
//!
//! [`commit_with_tuples`] はタプルを並べた順に書き、それぞれの結果（`Ok(true)` ＝この呼び出しで新しく書いた・
//! `Ok(false)` ＝もともとあった・`Err`）を覚えてから、終わりの段（`finish`・多くは Tx の commit）を待つ。
//!
//! - **(a) 書き込みが `Err`**: 適用されたかが分からない（応答だけ失われたことがある）。失敗したタプルと、それまでに
//!   `Ok(true)` だったタプルを逆順に外し（`delete_tuple` は冪等）、`finish` は待たずに捨てる（Tx なら巻き戻る）。
//!   `Ok(false)` のタプルは呼ぶ前からあったものなので外さない。補償は `finish` を捨てる**前**に行う（Tx が持つ
//!   ロック〔redeem の `lock_node`〕の下で外し、その間に別の経路が張り直さないようにする）。
//! - **(b) `finish` が `Err`**: `Ok(true)` だったタプルだけを逆順に外す。`Ok(false)` のタプルはほかの何かが
//!   頼っていることがあるので残す。
//!
//! 補償の `delete_tuple` の失敗はログに残して進む（best-effort）。外し損ねたタプルも PIT-38 の整合検証で拾う。
//!
//! # 時間切れとの関係
//!
//! 呼ぶ側を打ち切った後に、タスクが commit まで終えることがある。`storage.write` は冪等の予約（effect_journal）を
//! 同じ Tx に持つので、やり直しは相手の commit を待って結果を受け取り、書き込みは 1 回のまま（docs/workflow/engine.md §9.4）。

use std::{future::Future, sync::Arc};

use authz::{AuthContext, AuthzClient, AuthzError, FgaObject, Relation, Subject};
use sqlx::{Postgres, Transaction};
use tracing::Instrument as _;
use uuid::Uuid;

use crate::error::StorageError;

/// 書くタプル 1 つ（`object#relation@subject`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TupleKey {
    pub subject: Subject,
    pub relation: Relation,
    pub object: FgaObject,
}

impl TupleKey {
    pub fn new(subject: Subject, relation: Relation, object: FgaObject) -> Self {
        TupleKey {
            subject,
            relation,
            object,
        }
    }
}

/// [`commit_with_tuples`] の失敗。
#[derive(Debug, thiserror::Error)]
pub enum TupleCommitError<E> {
    /// タプルの書き込みに失敗した。決まり (a) で外し、`finish` は待たずに捨てた。
    #[error("タプルの書き込みに失敗しました: {0}")]
    Write(AuthzError),
    /// `finish` が失敗した。決まり (b) で、この呼び出しで新しく書いたタプルだけを外した。
    #[error("{0}")]
    Finish(E),
    /// 区間を走らせたタスクが終わらなかった（panic・ランタイムの停止）。補償が済んだかは分からない。
    #[error("タプルと commit の区間を走らせたタスクが終わりませんでした: {0}")]
    Aborted(String),
}

impl<E: Into<StorageError>> From<TupleCommitError<E>> for StorageError {
    fn from(err: TupleCommitError<E>) -> Self {
        match err {
            TupleCommitError::Write(e) => StorageError::Authz(e),
            TupleCommitError::Finish(e) => e.into(),
            TupleCommitError::Aborted(_) => aborted_error(),
        }
    }
}

/// タスクが終わらなかったときの StorageError（内部の誤り＝API は 500）。
///
/// StorageError に内部の誤りの種類が無いので、背後のワーカーが落ちたことを表す `sqlx::Error::WorkerCrashed` を
/// `Db` に入れる（API はどちらも Internal に写す）。理由は [`spawn_to_end`] がログに残す。
fn aborted_error() -> StorageError {
    StorageError::Db(sqlx::Error::WorkerCrashed)
}

/// タプルを書いてから `finish` を待つ区間を、別のタスクで最後まで走らせる（補償の決まりはモジュールの説明）。
///
/// `finish` は呼ぶ前には動かないフューチャ（`async move { tx.commit().await }` など）で渡す。決まり (a) では
/// 一度も待たずに捨てる。呼ぶ側のフューチャが捨てられても区間は最後まで走る。タプルが無ければ別のタスクに
/// せず、`finish` をそのまま待つ（捨てられて困る FGA の区間が無い）。
pub async fn commit_with_tuples<T, E, F>(
    authz: Arc<dyn AuthzClient>,
    tuples: Vec<TupleKey>,
    finish: F,
) -> Result<T, TupleCommitError<E>>
where
    F: Future<Output = Result<T, E>> + Send + 'static,
    T: Send + 'static,
    E: Send + 'static,
{
    if tuples.is_empty() {
        return finish.await.map_err(TupleCommitError::Finish);
    }
    spawn_to_end(write_then_finish(authz, tuples, finish))
        .await
        .unwrap_or_else(|reason| Err(TupleCommitError::Aborted(reason)))
}

/// Tx を commit する [`commit_with_tuples`]（作成の経路の形）。
///
/// `tx` は `PgPool::begin` が返す `'static` の Tx。決まり (a) では commit せずに捨てる（drop で巻き戻る）。
pub async fn commit_tx_with_tuples(
    authz: Arc<dyn AuthzClient>,
    tx: Transaction<'static, Postgres>,
    tuples: Vec<TupleKey>,
) -> Result<(), TupleCommitError<sqlx::Error>> {
    commit_with_tuples(authz, tuples, tx.commit()).await
}

/// ノードを作るときに書くタプル: 作った人の owner と、親フォルダがあれば parent（並べた順に書く）。
pub(crate) fn node_creation_tuples(
    ctx: &AuthContext,
    parent_folder: Option<Uuid>,
    object: &FgaObject,
) -> Vec<TupleKey> {
    let mut tuples = vec![TupleKey::new(
        ctx.subject(),
        Relation::Owner,
        object.clone(),
    )];
    if let Some(p) = parent_folder {
        tuples.push(TupleKey::new(
            Subject::object(&ctx.ns().folder(&p.to_string())),
            Relation::Parent,
            object.clone(),
        ));
    }
    tuples
}

/// FGA の区間（共有の付け外しと監査・move の親の付け替え）を別のタスクで最後まで走らせ、その結果を待つ。
///
/// 区間の補償の決まりは呼ぶ側のものをそのまま使い、捨てられない形にだけそろえる。タスクが終わらなかったら
/// 内部の誤りにする。
pub(crate) async fn run_to_end<T: Send + 'static>(
    fut: impl Future<Output = Result<T, StorageError>> + Send + 'static,
) -> Result<T, StorageError> {
    spawn_to_end(fut)
        .await
        .unwrap_or_else(|_| Err(aborted_error()))
}

/// `fut` を `tokio::spawn` で走らせて終わりを待つ。呼ぶ側が捨てられてもタスクは最後まで走る（JoinHandle を
/// 捨てても切り離されるだけ）。終わらなかったら（panic・ランタイムの停止）理由をログに残して `Err` で返す。
async fn spawn_to_end<R: Send + 'static>(
    fut: impl Future<Output = R> + Send + 'static,
) -> Result<R, String> {
    tokio::spawn(fut.in_current_span())
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "FGA の区間を走らせたタスクが終わりませんでした（タプルが残ったかもしれない）");
            e.to_string()
        })
}

/// 区間の本体（別のタスクの中で走る）。
async fn write_then_finish<T, E, F>(
    authz: Arc<dyn AuthzClient>,
    tuples: Vec<TupleKey>,
    finish: F,
) -> Result<T, TupleCommitError<E>>
where
    F: Future<Output = Result<T, E>>,
{
    // この呼び出しで新しく書いた（write_tuple が Ok(true) の）タプル。補償で外すのはこれだけ。
    let mut fresh: Vec<TupleKey> = Vec::with_capacity(tuples.len());
    for t in tuples {
        match authz.write_tuple(&t.subject, t.relation, &t.object).await {
            Ok(true) => fresh.push(t),
            Ok(false) => {}
            Err(e) => {
                // (a) 適用されたか分からないので、失敗したものも外す。finish は補償の後で捨てる。
                fresh.push(t);
                undo(authz.as_ref(), &fresh).await;
                drop(finish);
                return Err(TupleCommitError::Write(e));
            }
        }
    }
    match finish.await {
        Ok(v) => Ok(v),
        Err(e) => {
            // (b) 新しく書いたものだけを外す。
            undo(authz.as_ref(), &fresh).await;
            Err(TupleCommitError::Finish(e))
        }
    }
}

/// 逆順に外す（best-effort）。外し損ねたらログに残す（PIT-38 の整合検証で拾う）。
async fn undo(authz: &dyn AuthzClient, tuples: &[TupleKey]) {
    for t in tuples.iter().rev() {
        if let Err(e) = authz.delete_tuple(&t.subject, t.relation, &t.object).await {
            tracing::warn!(
                error = %e,
                object = t.object.as_str(),
                relation = t.relation.as_str(),
                "補償でタプルを外せませんでした（孤児のタプルが残る・PIT-38 の整合検証で拾う）"
            );
        }
    }
}

#[cfg(test)]
#[path = "tuple_commit_tests.rs"]
mod tests;

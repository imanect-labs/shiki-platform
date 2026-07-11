//! メモリ上のライブドキュメント（1 ノード = 1 [`LiveDoc`]・プロセス内共有）。
//!
//! Doc/Awareness の変更と永続化・ブロードキャストの順序は次で固定する:
//! 「適用 → 追記（DB） → ブロードキャスト」。DB 追記前に配信しないことで、
//! クラッシュ時に「他クライアントは見たが永続化されていない update」を作らない。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::RwLock;

use tokio::sync::broadcast;
use uuid::Uuid;
use yrs::sync::{Awareness, AwarenessUpdate};
use yrs::updates::decoder::Decode;
use yrs::{Doc, ReadTxn, StateVector, Transact, Update};

use crate::error::CollabError;
use crate::store::{DocStore, PersistedDoc, COMPACT_EVERY};

/// ブロードキャスト 1 フレーム。`from` は送信元接続 id（自己エコーの抑制に使う）。
#[derive(Clone, Debug)]
pub struct Frame {
    pub from: u64,
    pub data: Vec<u8>,
}

/// ブロードキャストチャネル容量。溢れたら遅い受信者は Lagged になり、
/// セッション側が接続を閉じて再同期させる（sync step1/2 で回復できるため安全）。
const BROADCAST_CAPACITY: usize = 256;

/// プロセス内で共有するライブドキュメント。
pub struct LiveDoc {
    pub node_id: Uuid,
    /// Doc を内包する Awareness。ロックは同期区間のみ（await を跨いで保持しない）。
    awareness: RwLock<Awareness>,
    tx: broadcast::Sender<Frame>,
    /// 接続数（0 になったら hub がアンロードする）。
    conns: AtomicUsize,
    /// 最後の圧縮以降に追記した update 件数（[`COMPACT_EVERY`] で圧縮発火）。
    appended_since_compact: AtomicUsize,
}

impl LiveDoc {
    /// 永続状態から復元する（snapshot → 残 update の順に適用）。
    pub fn restore(node_id: Uuid, persisted: &PersistedDoc) -> Result<Self, CollabError> {
        let doc = Doc::new();
        {
            let mut txn = doc.transact_mut();
            if let Some(snapshot) = &persisted.snapshot {
                apply_bytes(&mut txn, snapshot)?;
            }
            for update in &persisted.updates {
                apply_bytes(&mut txn, update)?;
            }
        }
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        Ok(LiveDoc {
            node_id,
            awareness: RwLock::new(Awareness::new(doc)),
            tx,
            conns: AtomicUsize::new(0),
            appended_since_compact: AtomicUsize::new(persisted.updates.len()),
        })
    }

    /// ブロードキャスト購読（接続ごと）。
    pub fn subscribe(&self) -> broadcast::Receiver<Frame> {
        self.tx.subscribe()
    }

    /// フレームを他接続へ配信する（受信者ゼロは正常＝単独編集）。
    pub fn broadcast(&self, from: u64, data: Vec<u8>) {
        let _ = self.tx.send(Frame { from, data });
    }

    pub fn conn_joined(&self) -> usize {
        self.conns.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn conn_left(&self) -> usize {
        self.conns.fetch_sub(1, Ordering::SeqCst) - 1
    }

    pub fn conn_count(&self) -> usize {
        self.conns.load(Ordering::SeqCst)
    }

    /// 受信 update を Doc に適用する（デコード失敗＝敵対的入力は拒否）。
    pub fn apply_update_bytes(&self, payload: &[u8]) -> Result<(), CollabError> {
        let awareness = self.read_awareness()?;
        let mut txn = awareness.doc().transact_mut();
        apply_bytes(&mut txn, payload)
    }

    /// サーバ側 state vector（sync step1 送信用）。
    pub fn state_vector(&self) -> Result<StateVector, CollabError> {
        let awareness = self.read_awareness()?;
        let txn = awareness.doc().transact();
        Ok(txn.state_vector())
    }

    /// クライアント state vector との差分（sync step2 応答用）。
    pub fn diff(&self, sv: &StateVector) -> Result<Vec<u8>, CollabError> {
        let awareness = self.read_awareness()?;
        let txn = awareness.doc().transact();
        Ok(txn.encode_state_as_update_v1(sv))
    }

    /// 全状態を 1 update に merge したもの（snapshot 圧縮用）。
    pub fn full_state(&self) -> Result<Vec<u8>, CollabError> {
        self.diff(&StateVector::default())
    }

    /// awareness update を適用する（プレゼンス・カーソル。viewer にも許可）。
    pub fn apply_awareness(&self, update: AwarenessUpdate) -> Result<(), CollabError> {
        let mut awareness = self.write_awareness()?;
        awareness
            .apply_update(update)
            .map_err(|e| CollabError::InvalidUpdate(format!("awareness: {e}")))
    }

    /// 現在の全 awareness 状態（新規接続への初期配信用）。状態が空なら None。
    pub fn awareness_full(&self) -> Result<Option<AwarenessUpdate>, CollabError> {
        let awareness = self.read_awareness()?;
        let clients: Vec<_> = awareness.iter().map(|(id, _)| id).collect();
        if clients.is_empty() {
            return Ok(None);
        }
        awareness
            .update_with_clients(clients)
            .map(Some)
            .map_err(|e| CollabError::InvalidUpdate(format!("awareness: {e}")))
    }

    /// 切断した接続が名乗っていた client 群の awareness を削除し、削除通知を返す。
    pub fn remove_awareness_clients(
        &self,
        client_ids: &[yrs::block::ClientID],
    ) -> Result<Option<AwarenessUpdate>, CollabError> {
        if client_ids.is_empty() {
            return Ok(None);
        }
        let mut awareness = self.write_awareness()?;
        for id in client_ids {
            awareness.remove_state(*id);
        }
        awareness
            .update_with_clients(client_ids.iter().copied())
            .map(Some)
            .map_err(|e| CollabError::InvalidUpdate(format!("awareness: {e}")))
    }

    /// update を「適用 → 追記 → （しきい値で）圧縮」まで済ませる。
    ///
    /// 圧縮は best-effort（失敗しても適用済み update は log にあり整合は崩れない）。
    pub async fn apply_and_persist(
        &self,
        store: &DocStore,
        payload: &[u8],
        author: &str,
    ) -> Result<(), CollabError> {
        self.apply_update_bytes(payload)?;
        let seq = store.append_update(self.node_id, payload, author).await?;
        let appended = self.appended_since_compact.fetch_add(1, Ordering::SeqCst) + 1;
        if appended as i64 >= COMPACT_EVERY {
            self.appended_since_compact.store(0, Ordering::SeqCst);
            let snapshot = self.full_state()?;
            store.compact(self.node_id, &snapshot, seq).await?;
        }
        Ok(())
    }

    /// アンロード前の最終圧縮（未圧縮 update を snapshot に畳む）。
    ///
    /// 呼び出し時点の全状態を snapshot にし、発番済み seq 全てを消し込む
    /// （アンロード時＝新規 update が来ない前提で hub が直列に呼ぶ）。
    pub async fn compact_now(&self, store: &DocStore) -> Result<(), CollabError> {
        if self.appended_since_compact.swap(0, Ordering::SeqCst) == 0 {
            return Ok(());
        }
        let snapshot = self.full_state()?;
        store.compact_latest(self.node_id, &snapshot).await
    }

    fn read_awareness(&self) -> Result<std::sync::RwLockReadGuard<'_, Awareness>, CollabError> {
        self.awareness
            .read()
            .map_err(|_| CollabError::InvalidUpdate("awareness lock poisoned".into()))
    }

    fn write_awareness(&self) -> Result<std::sync::RwLockWriteGuard<'_, Awareness>, CollabError> {
        self.awareness
            .write()
            .map_err(|_| CollabError::InvalidUpdate("awareness lock poisoned".into()))
    }
}

/// yrs update v1 バイト列をトランザクションに適用する（不正入力は拒否・fail-closed）。
fn apply_bytes(txn: &mut yrs::TransactionMut<'_>, payload: &[u8]) -> Result<(), CollabError> {
    let update =
        Update::decode_v1(payload).map_err(|e| CollabError::InvalidUpdate(e.to_string()))?;
    txn.apply_update(update)
        .map_err(|e| CollabError::InvalidUpdate(e.to_string()))
}

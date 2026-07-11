//! メモリ上のライブドキュメント（1 ノード = 1 [`LiveDoc`]・プロセス内共有）。
//!
//! Doc/Awareness の変更と永続化・ブロードキャストの順序は次で固定する:
//! 「適用 → 追記（DB） → ブロードキャスト」。DB 追記前に配信しないことで、
//! クラッシュ時に「他クライアントは見たが永続化されていない update」を作らない。

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::Instant;

use authz::AuthContext;
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
    /// この LiveDoc が最後に永続化した update の seq（アンロード最終圧縮の削除上限に使う）。
    /// これを固定値で `compact` に渡すことで、アンロード中に別 LiveDoc の再 join/append が
    /// 進んでも、その新 seq を snapshot 外で削除して失う事故を防ぐ（`compact_latest` の
    /// `next_seq - 1` 再計算が起こしていたデータロスの修正）。
    last_persisted_seq: AtomicI64,
    /// ノート保存（Task 11P.2）用の未保存状態。md 以外のドキュメントでは使われない。
    note: NoteDirty,
}

/// ノートの未保存編集トラッキング（デバウンス保存の判定材料）。
#[derive(Default)]
struct NoteDirty {
    dirty: AtomicBool,
    /// 最後の編集時刻。
    last_edit: Mutex<Option<Instant>>,
    /// 未保存編集の先頭時刻（SAVE_MAX 判定）。
    first_dirty: Mutex<Option<Instant>>,
    /// 保存の実行主体（最後に編集した人間/AI の AuthContext）。
    save_ctx: Mutex<Option<AuthContext>>,
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
            // ロード時点の最終 seq（next_seq は「次に発番する」ので -1 が最後の実在 seq）。
            last_persisted_seq: AtomicI64::new(persisted.next_seq - 1),
            note: NoteDirty::default(),
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
    /// 順序と失敗時の扱い（重要）:
    /// - 適用（メモリ）→ DB 追記 → 圧縮。呼び出し側（session）はこの成功後に限り他接続へ
    ///   broadcast するため、「他クライアントは見たが未永続化」の update は作らない。
    /// - `append_update` 失敗時はメモリには適用済み・DB には未記録の乖離が一過性に生じるが、
    ///   本メソッドは `Err` を返し、session はそれを受けて**当該接続を切断**する
    ///   （[`crate::session`] の掃除経路）。切断＝最終接続なら [`Self::compact_now`] が
    ///   その時点の全メモリ状態（適用済み update を含む）を snapshot として永続化するため、
    ///   通常運用ではロスしない。残る唯一の窓は「圧縮前のプロセスクラッシュ」で、これは
    ///   best-effort として許容する（クライアントは再接続の sync step1/2 で回復）。
    /// - 圧縮自体も best-effort（失敗しても update は log にあり整合は崩れない）。
    ///
    /// 併せてノート保存のための dirty マークと実行主体を記録する。
    pub async fn apply_and_persist(
        &self,
        store: &DocStore,
        payload: &[u8],
        ctx: &AuthContext,
    ) -> Result<(), CollabError> {
        self.apply_update_bytes(payload)?;
        let author = ctx.principal.id.as_str();
        let seq = store.append_update(self.node_id, payload, author).await?;
        self.last_persisted_seq.store(seq, Ordering::SeqCst);
        self.note_mark_dirty(ctx);
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
        // 削除上限はこの LiveDoc が最後に永続化した seq に固定する（DB の next_seq を
        // その場で読み直さない）。アンロード中に別 LiveDoc が再 join して新しい seq を
        // append しても、その新 update を snapshot 外で消して失う事故を防ぐ。
        let upto_seq = self.last_persisted_seq.load(Ordering::SeqCst);
        let snapshot = self.full_state()?;
        store.compact(self.node_id, &snapshot, upto_seq).await
    }

    // --- ノート保存トラッキング（Task 11P.2）---

    /// 未保存編集をマークする（保存の実行主体を最後の編集者で更新）。
    pub fn note_mark_dirty(&self, ctx: &AuthContext) {
        let now = Instant::now();
        if let Ok(mut last) = self.note.last_edit.lock() {
            *last = Some(now);
        }
        if let Ok(mut first) = self.note.first_dirty.lock() {
            first.get_or_insert(now);
        }
        if let Ok(mut save_ctx) = self.note.save_ctx.lock() {
            *save_ctx = Some(ctx.clone());
        }
        self.note.dirty.store(true, Ordering::SeqCst);
    }

    /// 保存条件（アイドル or 最大滞留）を満たしているか。
    pub fn note_should_save(&self, idle: std::time::Duration, max: std::time::Duration) -> bool {
        if !self.note.dirty.load(Ordering::SeqCst) {
            return false;
        }
        let now = Instant::now();
        let idle_ok = self
            .note
            .last_edit
            .lock()
            .ok()
            .and_then(|g| *g)
            .is_some_and(|t| now.duration_since(t) >= idle);
        let max_ok = self
            .note
            .first_dirty
            .lock()
            .ok()
            .and_then(|g| *g)
            .is_some_and(|t| now.duration_since(t) >= max);
        idle_ok || max_ok
    }

    /// dirty を消費して保存主体を取り出す（dirty でなければ None）。
    pub fn note_take_dirty(&self) -> Option<AuthContext> {
        if !self.note.dirty.swap(false, Ordering::SeqCst) {
            return None;
        }
        if let Ok(mut first) = self.note.first_dirty.lock() {
            *first = None;
        }
        self.note.save_ctx.lock().ok().and_then(|g| g.clone())
    }

    /// 現在の内容を正規化 md（frontmatter 付き）へシリアライズする。
    pub fn to_markdown(&self) -> Result<String, CollabError> {
        let awareness = self.read_awareness()?;
        Ok(crate::note::doc_to_markdown(awareness.doc()))
    }

    /// AI 編集オペを共有ドキュメントに適用し、生成された update（差分）と適用結果を返す
    /// （Task 11P.4）。トランザクション origin を AI にして人間編集と区別する。返した
    /// update は呼び出し側が永続化＋ブロードキャストする（人間の update と同じ経路）。
    pub fn apply_ai_edit(
        &self,
        ops: &[crate::note::EditOp],
        mode: crate::note::EditMode,
    ) -> Result<(Vec<u8>, crate::note::EditReport), CollabError> {
        let awareness = self.read_awareness()?;
        let doc = awareness.doc();
        let fragment = doc.get_or_insert_xml_fragment(crate::note::yjs_map::FRAGMENT_NAME);
        let meta = doc.get_or_insert_map(crate::note::yjs_map::META_MAP_NAME);
        let before = doc.transact().state_vector();
        let report = {
            let mut txn = doc.transact_mut_with(crate::note::AI_ORIGIN);
            crate::note::ai_edit::apply_ops(&mut txn, &fragment, &meta, ops, mode)
        };
        let update = doc.transact().encode_state_as_update_v1(&before);
        Ok((update, report))
    }

    /// AI 由来の update を「追記（DB）→（しきい値で）圧縮」する（適用は済んでいる前提）。
    /// dirty マークも付け、デバウンス保存で md へ落ちるようにする。
    pub async fn persist_ai_update(
        &self,
        store: &DocStore,
        update: &[u8],
        ctx: &AuthContext,
    ) -> Result<(), CollabError> {
        let author = format!("ai:{}", ctx.principal.id);
        let seq = store.append_update(self.node_id, update, &author).await?;
        self.last_persisted_seq.store(seq, Ordering::SeqCst);
        self.note_mark_dirty(ctx);
        let appended = self.appended_since_compact.fetch_add(1, Ordering::SeqCst) + 1;
        if appended as i64 >= COMPACT_EVERY {
            self.appended_since_compact.store(0, Ordering::SeqCst);
            let snapshot = self.full_state()?;
            store.compact(self.node_id, &snapshot, seq).await?;
        }
        Ok(())
    }

    /// md 全文を全置換で取り込む（外部書込のインポート・Task 11P.2 単方向規約）。
    pub fn import_markdown(&self, markdown: &str) -> Result<(), CollabError> {
        let awareness = self.read_awareness()?;
        crate::note::import_markdown(awareness.doc(), markdown);
        Ok(())
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

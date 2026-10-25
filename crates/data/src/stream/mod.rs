//! レコード変更の outbox ＋ SSE 差分配信（data-platform §9・Task 15.6）。
//!
//! ```text
//! 書込 Tx ──outbox(data.record.changed・旧値は非公開部)──▶ リレー ──delivery_seq(テーブルごとの連番)──▶ 配信
//!                                                                                                   │
//!   購読者ごとに before / after の可視性（行述語・個別共有）とフィールドマスクを判定して送る ◀────────┘
//! ```
//!
//! - [`outbox`] — 書込経路（create / update / delete / FSM 遷移 / 一括取込 / 再計算）からの発行
//! - [`relay`] — outbox → テーブルのストリームへの採番（永続化）と、派生値の再配信イベント
//! - [`hub`] — 購読の登録と 250 ms バッチの配信（[`StreamConfig`]）
//! - [`visibility`] — 述語のロール共通部／ユーザー固有部への分解とグループ評価
//! - [`auth`] — 購読中に限る認可材料の TTL（PIT-70 の明示の例外）
//! - [`invalidate`] — ロール変更・共有解除などの世代（TTL を待たない無効化）
//! - [`awareness`] — 他ユーザーのカーソル（データを含まない）
//!
//! # 製品の約束（PIT-70）
//!
//! **SSE 購読中の権限の剥奪は、配信への反映が最大 TTL（既定 5 秒）遅れる。** REST の読取は毎回
//! 解決するので遅れない。ロール変更・共有解除・テーブル権限の変更を検知した購読は TTL を待たずに
//! 引き直す（[`crate::DataStore::notify_authz_changed`]）。購読の開始・終了・材料の変化は監査に残す。

pub(crate) mod auth;
pub(crate) mod awareness;
mod batch;
pub(crate) mod events;
pub(crate) mod hub;
pub(crate) mod invalidate;
pub(crate) mod outbox;
pub(crate) mod relay;
pub(crate) mod visibility;

pub use events::{
    AwarenessEvent, RecordRemovedEvent, RecordUpsertedEvent, RecordsBulkAppliedEvent,
    SchemaChangedEvent, StreamEvent, StreamKeepaliveEvent, StreamMessage, StreamReadyEvent,
    StreamResetEvent, StreamResetReason,
};
pub use hub::{RecordStream, StreamConfig};
pub use outbox::RECORD_CHANGED_EVENT;
pub use relay::LEDGER_CONSUMER;

use std::time::Duration;

use crate::store::DataStore;

impl DataStore {
    /// 配信の設定を変える（TTL・保持期間・バッチ窓など）。
    #[must_use]
    pub fn with_stream_config(mut self, config: StreamConfig) -> Self {
        self.stream_config = config;
        self
    }

    /// 配信の設定。
    pub fn stream_config(&self) -> &StreamConfig {
        &self.stream_config
    }

    /// outbox の台帳 consumer を登録する（GC がこの consumer の配送を待つようにする・初回のみ
    /// 既存のバックログを飛ばす）。リレーを動かすプロセスの起動時に呼ぶ。
    pub async fn register_stream_consumer(&self) -> Result<(), crate::DataError> {
        for (_, pool) in self.pools.all() {
            let mut tx = pool.begin().await.map_err(crate::map_db)?;
            storage::event::register_consumer(&mut tx, &self.stream_config.consumer)
                .await
                .map_err(|e| crate::DataError::Internal(format!("outbox: {e}")))?;
            tx.commit().await.map_err(crate::map_db)?;
        }
        Ok(())
    }

    /// 台帳 consumer を廃止する（登録と台帳行を消す）。**恒久的に畳むときだけ**呼ぶ
    /// （一時停止で呼ぶと再登録の fast-forward で停止中の変更を飛ばす・storage の注意書きと同じ）。
    #[doc(hidden)]
    pub async fn unregister_stream_consumer(&self) -> Result<(), crate::DataError> {
        for (_, pool) in self.pools.all() {
            let mut tx = pool.begin().await.map_err(crate::map_db)?;
            storage::event::unregister_consumer(&mut tx, &self.stream_config.consumer)
                .await
                .map_err(|e| crate::DataError::Internal(format!("outbox: {e}")))?;
            tx.commit().await.map_err(crate::map_db)?;
        }
        Ok(())
    }
}

/// リレーの常駐ワーカー（購読が無くても採番を進める＝スナップショット読取の基準位置と GC を進める）。
#[derive(Clone)]
pub struct RecordStreamRelayWorker {
    store: DataStore,
    poll: Duration,
}

impl RecordStreamRelayWorker {
    pub fn new(store: DataStore) -> Self {
        RecordStreamRelayWorker {
            store,
            poll: Duration::from_secs(1),
        }
    }

    /// バックグラウンドで回す（台帳 consumer の登録 → 採番の繰り返し）。
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(e) = self.store.register_stream_consumer().await {
                tracing::error!(error = %e, "data-stream consumer の登録に失敗");
            }
            loop {
                match self.store.relay_record_changes_once().await {
                    // 溜まっていれば間を空けずに続ける。
                    Ok(n) if n > 0 => continue,
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "outbox の採番に失敗（次の周期で再試行）"),
                }
                tokio::time::sleep(self.poll).await;
            }
        })
    }
}

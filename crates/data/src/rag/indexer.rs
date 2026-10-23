//! 構造化データの増分索引（Task 15.8・15.6 の outbox に相乗り）。
//!
//! 台帳 consumer（既定 [`RAG_INDEX_CONSUMER`]）として outbox の `data.record.changed` を消費し、
//! オプトインしたテーブルのレコードを 1 件 = 1 チャンクへ投影して [`RecordChunkSink`] へ渡す。
//! 埋め込み・ベクタ索引・全文索引は sink の実装側（`crates/rag`）が持つ。data は rag に依存しない。
//!
//! # 規則
//!
//! - **索引は可視性を焼き込まない**。チャンクに載せる認可材料は構造タグ `data_table:<t>|<table_id>`
//!   だけで、行述語・個別共有は検索時の post-filter（[`crate::DataStore::rag_visible_records`]）が毎回評価する。
//!   したがって行述語・共有・ロールの変更では索引を書き換えない
//! - **世代が変わったら破棄して作り直す**。`field_policy`・投影の定義が変わると世代が変わる
//!   （[`super::projection::generation`]）。sink の世代と一致しなければ、テーブルのチャンクを全部捨ててから
//!   全件を投影し直す。作り直しが終わるまでの古いチャンクは post-filter が世代不一致で落とす
//! - **現在の行から作る**。イベントの payload の値は使わず、消費時点の行を読み直す（順序の入れ替わり・
//!   再配信で古い値が勝たない）。行が無ければチャンクを消す
//! - 冪等: sink は `record_id` 単位の upsert / remove。反映と `mark_delivered` は同じ Tx（sink が失敗すれば
//!   Tx ごと巻き戻り、次の周期で再配信される・at-least-once）
//! - 並行する消費は台帳 consumer の採番行のロックで直列化する
//!
//! # スパイクの範囲（本番化で変えるもの）
//!
//! 再構築は消費 Tx の中で同期に回す（PoC）。本番は jobq の世代付きジョブ（索引のバックフィル
//! [`crate::IndexBackfillWorker`] と同じ形）へ出し、消費 Tx を短く保つ。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use sqlx::types::Json;
use sqlx::PgConnection;
use uuid::Uuid;

use super::projection::{generation, projected, render};
use crate::model::DataTable;
use crate::store::DataStore;
use crate::stream::outbox::{ChangeKind, RECORD_CHANGED_EVENT};
use crate::{map_db, DataError};

/// 既定の台帳 consumer 名（他の consumer とは独立に配送を記録する）。
pub const RAG_INDEX_CONSUMER: &str = "data-rag-index";
/// 1 バッチで claim する outbox のイベント数（再構築の 1 ページの行数も同じ）。
pub const RAG_INDEX_BATCH: i64 = 500;

/// sink へ渡す 1 チャンク（レコード 1 件）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordChunk {
    pub tenant_id: String,
    /// テーブルの org（検索時の hydrate は data 側が `org = ctx.org` で絞る・PIT-45）。
    pub org: String,
    pub table_id: Uuid,
    pub record_id: Uuid,
    pub rev: i64,
    /// 索引の世代（post-filter が現在の世代と照合する）。
    pub generation: String,
    /// pre-filter の構造タグ `data_table:<tenant>|<table_id>`（folder/file タグと同型・名前空間化のまま）。
    pub authz_tag: String,
    pub title: String,
    pub body: String,
}

/// チャンクの受け手（埋め込み＋ベクタ索引・全文索引。実装は `crates/rag` 側）。
///
/// 全メソッドは冪等であること（at-least-once で再配信される）。
#[async_trait]
pub trait RecordChunkSink: Send + Sync {
    /// テーブルの索引の世代（索引していなければ `None`）。
    async fn generation(
        &self,
        tenant_id: &str,
        table_id: Uuid,
    ) -> Result<Option<String>, DataError>;
    /// テーブルの全チャンクを破棄し、世代を付け替える（`None` = 索引から外す）。
    async fn reset_table(
        &self,
        tenant_id: &str,
        table_id: Uuid,
        generation: Option<&str>,
    ) -> Result<(), DataError>;
    /// チャンクを upsert する（`record_id` 単位）。
    async fn upsert(&self, chunks: &[RecordChunk]) -> Result<(), DataError>;
    /// チャンクを消す（行の削除）。
    async fn remove(
        &self,
        tenant_id: &str,
        table_id: Uuid,
        record_ids: &[Uuid],
    ) -> Result<(), DataError>;
}

/// 増分索引の常駐ワーカー。
#[derive(Clone)]
pub struct RagIndexWorker {
    store: DataStore,
    sink: Arc<dyn RecordChunkSink>,
    consumer: String,
    poll: Duration,
}

/// 1 テーブル分の変更（このバッチ）。
#[derive(Default)]
struct Touched {
    records: BTreeSet<Uuid>,
}

#[allow(clippy::needless_pass_by_value)] // map_err へそのまま渡す。
fn st(e: storage::StorageError) -> DataError {
    DataError::Internal(format!("outbox: {e}"))
}

impl RagIndexWorker {
    pub fn new(store: DataStore, sink: Arc<dyn RecordChunkSink>) -> Self {
        RagIndexWorker {
            store,
            sink,
            consumer: RAG_INDEX_CONSUMER.to_string(),
            poll: Duration::from_secs(1),
        }
    }

    /// 台帳 consumer 名を変える（テスト・並行する別の索引系統用）。
    #[must_use]
    pub fn with_consumer(mut self, consumer: impl Into<String>) -> Self {
        self.consumer = consumer.into();
        self
    }

    /// 台帳 consumer を登録する（初回のみ既存のバックログを飛ばす。オプトイン済みテーブルは
    /// 次の変更で世代不一致を検出して全件を作り直す）。
    pub async fn register(&self) -> Result<(), DataError> {
        for (_, pool) in self.store.pools.all() {
            let mut tx = pool.begin().await.map_err(map_db)?;
            storage::event::register_consumer(&mut tx, &self.consumer)
                .await
                .map_err(st)?;
            tx.commit().await.map_err(map_db)?;
        }
        Ok(())
    }

    /// 台帳 consumer を廃止する（恒久的に畳むときだけ）。
    pub async fn unregister(&self) -> Result<(), DataError> {
        for (_, pool) in self.store.pools.all() {
            let mut tx = pool.begin().await.map_err(map_db)?;
            storage::event::unregister_consumer(&mut tx, &self.consumer)
                .await
                .map_err(st)?;
            tx.commit().await.map_err(map_db)?;
        }
        Ok(())
    }

    /// outbox の未消費の変更を 1 バッチ索引へ反映する（全プール）。処理した outbox の件数を返す。
    pub async fn run_once(&self) -> Result<usize, DataError> {
        let mut total = 0;
        for (_, pool) in self.store.pools.all() {
            let mut tx = pool.begin().await.map_err(map_db)?;
            total += self.consume_batch(&mut tx).await?;
            tx.commit().await.map_err(map_db)?;
        }
        Ok(total)
    }

    /// バックグラウンドで回す（登録 → 消費の繰り返し）。
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(e) = self.register().await {
                tracing::error!(error = %e, "data-rag-index consumer の登録に失敗");
            }
            loop {
                match self.run_once().await {
                    Ok(n) if n > 0 => continue,
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "RAG 増分索引に失敗（次の周期で再試行）"),
                }
                tokio::time::sleep(self.poll).await;
            }
        })
    }

    async fn consume_batch(&self, tx: &mut PgConnection) -> Result<usize, DataError> {
        storage::event::lock_consumer_seq(&mut *tx, &self.consumer, None)
            .await
            .map_err(st)?;
        let events = storage::event::claim_undelivered(&mut *tx, &self.consumer, RAG_INDEX_BATCH)
            .await
            .map_err(st)?;
        if events.is_empty() {
            return Ok(0);
        }
        // (tenant_id, table_id) → 触れたレコード。BTreeMap で処理順を固定する。
        let mut tables: BTreeMap<(String, Uuid), Touched> = BTreeMap::new();
        for e in &events {
            if e.payload.get("event_type").and_then(Value::as_str) != Some(RECORD_CHANGED_EVENT) {
                continue;
            }
            let uuid_at = |k: &str| {
                e.payload
                    .get(k)
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse::<Uuid>().ok())
            };
            let (Some(table_id), Some(record_id)) = (uuid_at("table_id"), uuid_at("record_id"))
            else {
                continue;
            };
            let kind = e
                .payload
                .get("change")
                .and_then(Value::as_str)
                .and_then(ChangeKind::parse);
            let t = tables.entry((e.tenant_id.clone(), table_id)).or_default();
            if matches!(
                kind,
                Some(ChangeKind::Create | ChangeKind::Update | ChangeKind::Delete)
            ) {
                t.records.insert(record_id);
            }
            // schema / refresh / derived は世代の照合だけ（派生列は投影できないので本文は変わらない）。
        }
        for ((tenant_id, table_id), touched) in &tables {
            self.apply_table(tx, tenant_id, *table_id, touched).await?;
        }
        let all: Vec<i64> = events.iter().map(|e| e.id).collect();
        storage::event::mark_delivered(&mut *tx, &self.consumer, &all)
            .await
            .map_err(st)?;
        Ok(events.len())
    }

    async fn apply_table(
        &self,
        tx: &mut PgConnection,
        tenant_id: &str,
        table_id: Uuid,
        touched: &Touched,
    ) -> Result<(), DataError> {
        let loaded = crate::stream::relay::table_row_with_org(tx, tenant_id, table_id).await?;
        let current_gen = loaded.as_ref().and_then(|(t, _)| generation(&t.schema));
        let indexed_gen = self.sink.generation(tenant_id, table_id).await?;
        let (Some((table, org)), Some(gen)) = (loaded, current_gen) else {
            // 削除済み・オプトアウト: 索引に残っていれば外す。
            if indexed_gen.is_some() {
                self.sink.reset_table(tenant_id, table_id, None).await?;
            }
            return Ok(());
        };
        if indexed_gen.as_deref() != Some(gen.as_str()) {
            // 世代の切替（初回・field_policy / 投影の変更）: 破棄して全件を作り直す。
            self.sink
                .reset_table(tenant_id, table_id, Some(&gen))
                .await?;
            return self.rebuild(tx, tenant_id, &table, &org, &gen).await;
        }
        if touched.records.is_empty() {
            return Ok(());
        }
        let ids: Vec<Uuid> = touched.records.iter().copied().collect();
        let rows = project_rows(tx, tenant_id, &table, RowSel::Ids(&ids)).await?;
        let present: BTreeSet<Uuid> = rows.iter().map(|r| r.id).collect();
        let chunks = to_chunks(tenant_id, &table, &org, &gen, rows);
        self.sink.upsert(&chunks).await?;
        let gone: Vec<Uuid> = ids.into_iter().filter(|id| !present.contains(id)).collect();
        if !gone.is_empty() {
            self.sink.remove(tenant_id, table_id, &gone).await?;
        }
        Ok(())
    }

    /// テーブルの全件を投影し直す（keyset で [`RAG_INDEX_BATCH`] 件ずつ）。
    async fn rebuild(
        &self,
        tx: &mut PgConnection,
        tenant_id: &str,
        table: &DataTable,
        org: &str,
        gen: &str,
    ) -> Result<(), DataError> {
        let mut after = 0_i64;
        loop {
            let rows = project_rows(tx, tenant_id, table, RowSel::After(after)).await?;
            let Some(last) = rows.last().map(|r| r.record_int) else {
                return Ok(());
            };
            after = last;
            let n = rows.len();
            let chunks = to_chunks(tenant_id, table, org, gen, rows);
            self.sink.upsert(&chunks).await?;
            if i64::try_from(n).unwrap_or(i64::MAX) < RAG_INDEX_BATCH {
                return Ok(());
            }
        }
    }
}

/// 投影に使う行（`record_int` は keyset の位置にだけ使い、crate の外へ出さない・PIT-75）。
struct ProjectedRow {
    record_int: i64,
    id: Uuid,
    rev: i64,
    values: Value,
}

enum RowSel<'a> {
    Ids(&'a [Uuid]),
    After(i64),
}

/// 投影列だけを引く（保存形キーはバインド・行述語は掛けない＝索引は全行を持ち、可視性は post-filter）。
async fn project_rows(
    tx: &mut PgConnection,
    tenant_id: &str,
    table: &DataTable,
    sel: RowSel<'_>,
) -> Result<Vec<ProjectedRow>, DataError> {
    let keys: Vec<String> = projected(&table.schema)
        .unwrap_or_default()
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    let (cond, order) = match sel {
        RowSel::Ids(_) => ("r.id = ANY($5::uuid[])", ""),
        RowSel::After(_) => ("r.record_int > $5", " ORDER BY r.record_int LIMIT $6"),
    };
    let sql = format!(
        "SELECT r.record_int, r.id, r.rev, \
                coalesce((SELECT jsonb_object_agg(k.k, r.data -> k.k) \
                          FROM unnest($4::text[]) AS k(k) WHERE r.data ? k.k), '{{}}'::jsonb) \
         FROM data_record r \
         WHERE r.tenant_int = $1 AND r.table_int = $2 AND r.tenant_id = $3 AND {cond}{order}"
    );
    let q = sqlx::query_as::<_, (i64, Uuid, i64, Json<Value>)>(&sql)
        .bind(table.keys.tenant_int)
        .bind(table.keys.table_int)
        .bind(tenant_id)
        .bind(keys);
    let q = match sel {
        RowSel::Ids(ids) => q.bind(ids.to_vec()),
        RowSel::After(after) => q.bind(after).bind(RAG_INDEX_BATCH),
    };
    let rows = q.fetch_all(&mut *tx).await.map_err(map_db)?;
    Ok(rows
        .into_iter()
        .map(|(record_int, id, rev, Json(values))| ProjectedRow {
            record_int,
            id,
            rev,
            values,
        })
        .collect())
}

fn to_chunks(
    tenant_id: &str,
    table: &DataTable,
    org: &str,
    gen: &str,
    rows: Vec<ProjectedRow>,
) -> Vec<RecordChunk> {
    let cols = projected(&table.schema).unwrap_or_default();
    // タグは名前空間化のまま（`data_table:<tenant>|<table_id>`・folder/file タグと同型）。
    let authz_tag = crate::stream::relay::system_ctx(tenant_id, org)
        .ns()
        .data_table(&table.id.to_string())
        .as_str()
        .to_string();
    rows.into_iter()
        .map(|r| {
            let rendered = render(&cols, &r.values);
            RecordChunk {
                tenant_id: tenant_id.to_string(),
                org: org.to_string(),
                table_id: table.id,
                record_id: r.id,
                rev: r.rev,
                generation: gen.to_string(),
                authz_tag: authz_tag.clone(),
                title: rendered.title,
                body: rendered.body,
            }
        })
        .collect()
}

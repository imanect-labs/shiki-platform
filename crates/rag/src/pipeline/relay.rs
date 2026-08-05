//! outbox → jobq relay（Task 2.8）。
//!
//! storage_event_outbox（耐久イベントログ・fan-out 点）から未処理イベントを claim し、
//! `rag_ingest` キューへコピーして mark_processed する。**全体が単一トランザクション**
//! なので、同一 Postgres 内で exactly-once（クラッシュしてもロールバックで再配信）。

use jobq::NewJob;
use sqlx::PgPool;

use super::{IngestMessage, RAG_INGEST_QUEUE};
use crate::config::RagConfig;
use crate::error::RagError;

/// 1 バッチで claim するイベント数。
const RELAY_BATCH: i64 = 64;

/// outbox を 1 バッチ処理する。転送した件数を返す。
pub async fn relay_once(pool: &PgPool, config: &RagConfig) -> Result<usize, RagError> {
    let mut tx = pool.begin().await?;
    let events = storage::event::claim(&mut tx, RELAY_BATCH).await?;
    if events.is_empty() {
        return Ok(0);
    }

    let mut ids = Vec::with_capacity(events.len());
    let mut skipped_system = 0usize;
    for event in &events {
        // システム領域（#392・`node.system`）は索引しない。自律エージェントの使い捨ての作業メモ
        // （brief / outline / notes 等）を社内検索に載せないため、**enqueue せず ack だけする**
        // （ジョブを作ってから捨てるより安い・op を問わず一律に効く）。outbox 自体は忠実な
        // ログのままなので、他の consumer（miniapp-functions / workflow）には影響しない。
        if event.system {
            ids.push(event.id);
            skipped_system += 1;
            continue;
        }
        let message = IngestMessage {
            tenant_id: event.tenant_id.clone(),
            org: event.org.clone(),
            node_id: event.node_id,
            version: event.version,
            op: event.op.clone(),
            actor: event.actor.clone(),
        };
        jobq::enqueue_on(
            &mut tx,
            NewJob {
                queue: RAG_INGEST_QUEUE,
                tenant_id: &event.tenant_id,
                payload: &serde_json::to_value(&message)?,
                trace_id: event.trace_id.as_deref(),
                max_attempts: config.job_max_attempts,
            },
        )
        .await?;
        ids.push(event.id);
    }
    storage::event::mark_processed(&mut tx, &ids).await?;
    tx.commit().await?;
    tracing::debug!(
        count = ids.len(),
        skipped_system,
        "outbox → rag_ingest へ relay"
    );
    Ok(ids.len())
}

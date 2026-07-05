//! インジェスト・パイプライン（Task 2.8/2.9）。
//!
//! storage の outbox（Task 1.8）→ jobq（`rag_ingest` キュー）→ consumer
//! （parse → chunk → embed → Qdrant/Tantivy/rag_chunk）の配線。
//!
//! # リーダー選出（advisory lock）
//!
//! Tantivy の IndexWriter は index あたり単一プロセスの制約があるため、relay/consumer は
//! **Postgres advisory lock を獲得した 1 プロセスだけ**が動かす。将来レプリカを増やしても
//! ライタ二重起動が構造的に起きない。ロック接続が切れたら自動で再獲得を試みる
//! （その間、待機プロセスが昇格する）。

pub mod consumer;
pub mod indexer;
pub mod job_state;
pub mod relay;

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use storage::IndexerStorage;
use uuid::Uuid;

use crate::config::RagConfig;
use crate::embedding::EmbeddingProvider;
use crate::fulltext::FulltextIndex;
use crate::parser::DocumentParser;
use crate::vector_store::VectorStore;

/// RAG インジェストのキュー名。
pub const RAG_INGEST_QUEUE: &str = "rag_ingest";

/// パイプラインのリーダー選出に使う advisory lock キー（プロジェクト内で一意）。
const PIPELINE_LOCK_KEY: i64 = 0x7368_696b_695f_7261; // "shiki_ra"

/// jobq に載せるインジェストメッセージ。
///
/// `tenant_id` は job_queue の第一級カラムが正本だが、メッセージにも必須で持たせて
/// consumer 側で突合する（design §4.3: インジェスト経路の tenant_id 必須化）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestMessage {
    pub tenant_id: String,
    pub org: String,
    pub node_id: Uuid,
    pub version: i64,
    pub op: String,
    pub actor: String,
}

/// パイプラインの依存束。全てトレイト裏（テストはフェイク注入）。
pub struct PipelineDeps {
    pub pool: PgPool,
    pub config: RagConfig,
    pub parser: Arc<dyn DocumentParser>,
    pub embedder: Arc<dyn EmbeddingProvider>,
    pub vector: Arc<dyn VectorStore>,
    pub fulltext: Arc<dyn FulltextIndex>,
    pub indexer_storage: Arc<IndexerStorage>,
}

/// relay ＋ consumer をバックグラウンド起動する（shiki-server の main から呼ぶ）。
///
/// 返した JoinHandle は保持不要（プロセス生存中は動き続ける）。
pub fn spawn_pipeline(deps: PipelineDeps) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let deps = Arc::new(deps);
        loop {
            match run_as_leader(&deps).await {
                Ok(()) => {
                    // run_as_leader はエラー時のみ返る想定。念のため小休止して再試行。
                }
                Err(e) => {
                    tracing::warn!(error = %e, "RAG パイプラインが停止しました。再起動します");
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    })
}

/// advisory lock を獲得できたら relay/consumer ループを回す（リーダー）。
/// 獲得できなければ待機（スタンバイ）として戻る。
async fn run_as_leader(deps: &Arc<PipelineDeps>) -> Result<(), crate::error::RagError> {
    // ロックは専用接続に紐づく（接続が生きている間だけ保持される）。
    let mut lock_conn = deps.pool.acquire().await?;
    let acquired: bool = sqlx::query_scalar("select pg_try_advisory_lock($1)")
        .bind(PIPELINE_LOCK_KEY)
        .fetch_one(&mut *lock_conn)
        .await?;
    if !acquired {
        tracing::debug!("RAG パイプライン: 他プロセスがリーダーのため待機");
        return Ok(());
    }
    tracing::info!("RAG パイプライン: リーダーとして relay/consumer を開始");

    let relay_deps = Arc::clone(deps);
    let consume_deps = Arc::clone(deps);
    let relay_loop = async move {
        let interval = std::time::Duration::from_millis(relay_deps.config.relay_poll_ms);
        loop {
            if let Err(e) = relay::relay_once(&relay_deps.pool, &relay_deps.config).await {
                tracing::error!(error = %e, "outbox relay に失敗（次周期で再試行）");
            }
            tokio::time::sleep(interval).await;
        }
    };
    let consume_loop = async move {
        let idle = std::time::Duration::from_millis(consume_deps.config.relay_poll_ms);
        loop {
            match consumer::consume_once(&consume_deps).await {
                // バッチが満杯だった場合は待たずに続けて取りに行く。
                Ok(n) if n >= consume_deps.config.consumer_concurrency => {}
                Ok(_) => tokio::time::sleep(idle).await,
                Err(e) => {
                    tracing::error!(error = %e, "consumer ループに失敗（次周期で再試行）");
                    tokio::time::sleep(idle).await;
                }
            }
        }
    };

    // どちらも無限ループ。ロック接続の死活を監視し、切れたら返ってリーダー再選出へ。
    let health_loop = async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            if sqlx::query_scalar::<_, i32>("select 1")
                .fetch_one(&mut *lock_conn)
                .await
                .is_err()
            {
                return;
            }
        }
    };

    tokio::select! {
        () = relay_loop => {}
        () = consume_loop => {}
        () = health_loop => {
            tracing::warn!("RAG パイプライン: ロック接続が失われました（リーダー再選出）");
        }
    }
    Ok(())
}

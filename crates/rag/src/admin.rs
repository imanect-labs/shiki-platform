//! RAG のテナント消去（SAAS.2・テナント削除フローから呼ぶ）。
//!
//! rag_chunk / rag_ingest_job / jobq のジョブは常に消去する（過去に RAG が有効だった
//! テナントの残骸も残さない）。Qdrant / Tantivy は RAG 有効時のみ実体があるため
//! Option（無効構成では DB 行の消去だけ行う）。

use std::sync::Arc;

use sqlx::PgPool;

use crate::error::RagError;
use crate::fulltext::FulltextIndex;
use crate::store;
use crate::vector_store::VectorStore;

pub struct RagAdmin {
    pool: PgPool,
    vector: Option<Arc<dyn VectorStore>>,
    fulltext: Option<Arc<dyn FulltextIndex>>,
}

impl RagAdmin {
    pub fn new(
        pool: PgPool,
        vector: Option<Arc<dyn VectorStore>>,
        fulltext: Option<Arc<dyn FulltextIndex>>,
    ) -> Self {
        RagAdmin {
            pool,
            vector,
            fulltext,
        }
    }

    /// テナントの RAG 状態を全て破棄する（DB 行・待機/死配ジョブ・ベクタ・全文索引）。
    /// 冪等（対象が無ければ no-op）。
    pub async fn purge_tenant(&self, tenant_id: &str) -> Result<(), RagError> {
        let mut conn = self.pool.acquire().await?;
        let rows = store::purge_tenant(&mut conn, tenant_id).await?;
        let jobs = jobq::delete_tenant(&mut conn, tenant_id).await?;
        drop(conn);
        if let Some(vector) = &self.vector {
            vector.purge_tenant(tenant_id).await?;
        }
        if let Some(fulltext) = &self.fulltext {
            let fulltext = Arc::clone(fulltext);
            let tenant = tenant_id.to_string();
            tokio::task::spawn_blocking(move || fulltext.purge_tenant(&tenant))
                .await
                .map_err(|e| RagError::Fulltext(format!("spawn_blocking: {e}")))??;
        }
        tracing::info!(%tenant_id, rows, jobs, "RAG テナント消去完了");
        Ok(())
    }
}

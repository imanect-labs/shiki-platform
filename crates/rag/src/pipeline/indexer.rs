//! 1 インジェスト・ジョブの実処理（Task 2.8/2.9）。
//!
//! op ごとの索引整合:
//! - `create` / `update` / `restore`: parse → chunk → embed → rag_chunk 差替え →
//!   Qdrant upsert（＋旧版掃除）→ Tantivy 差替え。
//! - `move`: authz_tags 再評価のみ（rag_chunk 更新・Qdrant payload 再書込・Tantivy 再投入。
//!   再パース/再埋め込みはしない）。
//! - `rename`: no-op（ファイル名は検索ハイドレーション時に node JOIN で現在値を返す）。
//! - `delete`: 全索引＋rag_chunk から除去。
//!
//! 冪等性: `(tenant_id, node_id, version, op)` を rag_ingest_job の一意キーとし、
//! 成功/スキップ済みのジョブは再配信されても再処理しない。stale イベント
//! （node の現行 version と不一致）は skipped として捨てる。

use std::sync::Arc;

use authz::{AuthContext, Principal};
use uuid::Uuid;

use super::job_state::{begin_job, finish_job};
use super::{IngestMessage, PipelineDeps};
use crate::chunker::{chunk_document, ChunkParams};
use crate::embedding::EmbedInput;
use crate::error::RagError;
use crate::fulltext::FulltextDoc;
use crate::parser::ParseRequest;
use crate::store;
use crate::types::ChunkKind;
use crate::vector_store::ChunkPoint;

/// パース対象の MIME（worker の対応形式と対にする）。
const SUPPORTED_TYPES: &[&str] = &[
    "application/pdf",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "text/html",
    "text/markdown",
    "text/csv",
    "text/plain",
];

/// ジョブの結果（ログ・rag_ingest_job の status に対応）。
#[derive(Debug)]
pub enum IndexOutcome {
    Indexed {
        chunks: usize,
    },
    Retagged,
    Deleted,
    /// フォルダイベントを子孫ファイルの個別ジョブへ展開した。
    FannedOut {
        files: usize,
    },
    Skipped(&'static str),
}

impl std::fmt::Display for IndexOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexOutcome::Indexed { chunks } => write!(f, "indexed({chunks} chunks)"),
            IndexOutcome::Retagged => write!(f, "retagged"),
            IndexOutcome::Deleted => write!(f, "deleted"),
            IndexOutcome::FannedOut { files } => write!(f, "fanned-out({files} files)"),
            IndexOutcome::Skipped(reason) => write!(f, "skipped({reason})"),
        }
    }
}

/// イベント由来の認可コンテキスト（actor/org/tenant）。識別子構築（`ns()`）の起点。
fn event_context(message: &IngestMessage) -> AuthContext {
    AuthContext::new(
        Principal {
            id: message.actor.clone(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(message.tenant_id.clone()),
        },
        message.org.clone(),
        message.tenant_id.clone(),
    )
}

/// ジョブ本体。冪等判定 → op 分岐 → rag_ingest_job へ結果記録。
pub async fn handle(
    deps: &Arc<PipelineDeps>,
    message: &IngestMessage,
    trace_id: Option<&str>,
) -> Result<IndexOutcome, RagError> {
    if begin_job(&deps.pool, message, trace_id).await? {
        return Ok(IndexOutcome::Skipped("already-processed"));
    }

    let ctx = event_context(message);
    // 削除中/削除済みテナントへの書き戻しを fail-closed で止める（purge と並行して
    // claim 済みジョブが rag_chunk/Qdrant/Tantivy を復活させない・SAAS.2）。
    if !deps
        .indexer_storage
        .tenant_is_active(&ctx.tenant_id)
        .await?
    {
        let outcome = IndexOutcome::Skipped("tenant-deleting");
        finish_job(&deps.pool, message, "skipped", None).await?;
        return Ok(outcome);
    }
    // storage はフォルダ操作でもフォルダ 1 件のイベントしか発行しないため、
    // フォルダの move/delete/restore は子孫ファイルの個別ジョブへ展開する。
    let is_folder = deps
        .indexer_storage
        .node_snapshot(&ctx.tenant_id, message.node_id)
        .await?
        .is_some_and(|s| s.kind == "folder");
    let result = match (message.op.as_str(), is_folder) {
        ("move" | "delete" | "restore", true) => fan_out_folder(deps, &ctx, message).await,
        ("create" | "update" | "restore", _) => index_node(deps, &ctx, message).await,
        ("move", _) => retag_node(deps, &ctx, message).await,
        ("rename", _) => Ok(IndexOutcome::Skipped("rename-noop")),
        ("delete", _) => remove_node(deps, &ctx, message).await,
        _ => Ok(IndexOutcome::Skipped("unknown-op")),
    };

    match &result {
        Ok(outcome) => {
            let status = match outcome {
                IndexOutcome::Skipped(_) => "skipped",
                _ => "succeeded",
            };
            finish_job(&deps.pool, message, status, None).await?;
        }
        Err(e) => {
            finish_job(&deps.pool, message, "failed", Some(&e.to_string())).await?;
        }
    }
    result
}

/// create/update/restore: parse → chunk → embed → 3 系統（DB/Qdrant/Tantivy）差替え。
async fn index_node(
    deps: &Arc<PipelineDeps>,
    ctx: &AuthContext,
    message: &IngestMessage,
) -> Result<IndexOutcome, RagError> {
    let Some(snapshot) = deps
        .indexer_storage
        .node_snapshot(&ctx.tenant_id, message.node_id)
        .await?
    else {
        return Ok(IndexOutcome::Skipped("node-missing"));
    };
    if snapshot.deleted {
        return Ok(IndexOutcome::Skipped("node-deleted"));
    }
    if snapshot.kind != "file" {
        return Ok(IndexOutcome::Skipped("folder"));
    }
    // stale イベント: 現行版と不一致（更新が続いた）なら最新版のイベントに任せる。
    if snapshot.version != message.version {
        return Ok(IndexOutcome::Skipped("superseded"));
    }
    let content_type = snapshot.content_type.as_deref().unwrap_or("");
    let base_type = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if !SUPPORTED_TYPES.contains(&base_type.as_str()) {
        return Ok(IndexOutcome::Skipped("unsupported-type"));
    }
    if snapshot.size_bytes.unwrap_or(0) > deps.config.max_parse_bytes {
        return Ok(IndexOutcome::Skipped("too-large"));
    }
    let Some(blob_sha256) = snapshot.blob_sha256.as_deref() else {
        return Ok(IndexOutcome::Skipped("no-blob"));
    };

    // parse（worker への blob 受け渡しは内部向け・短 TTL presigned GET）。
    let source_url = deps
        .indexer_storage
        .presign_internal_get(&ctx.tenant_id, &ctx.org, blob_sha256)
        .await?;
    let parsed = deps
        .parser
        .parse(
            ctx,
            ParseRequest {
                source_url: &source_url,
                content_type: &base_type,
                file_name: &snapshot.name,
            },
        )
        .await?;

    // chunk（決定的 ID）＋ authz_tags（構造タグ: file 自身＋祖先フォルダ・PIT-1 (b)）。
    let chunks = chunk_document(
        message.node_id,
        message.version,
        &parsed.blocks,
        &ChunkParams::default(),
    );
    let authz_tags = compute_authz_tags(deps, ctx, message.node_id).await?;

    // embed（leaf/table のみ）。版突合ガードは EmbeddingProvider 実装内。
    let embeddable: Vec<_> = chunks
        .iter()
        .filter(|c| c.kind != ChunkKind::Parent)
        .collect();
    let texts: Vec<String> = embeddable.iter().map(|c| c.searchable_text()).collect();
    let embedded = deps
        .embedder
        .embed(ctx, EmbedInput::Document, &texts)
        .await?;
    if embedded.vectors.len() != embeddable.len() {
        return Err(RagError::Worker(format!(
            "埋め込み応答数の不一致: 期待 {} 実際 {}",
            embeddable.len(),
            embedded.vectors.len()
        )));
    }

    // 差替え: rag_chunk（正本）→ Qdrant → Tantivy。
    // 途中失敗はジョブ再試行で全段やり直し（決定的 ID により冪等）。
    store::replace_chunks(
        &deps.pool,
        ctx,
        message.node_id,
        message.version,
        &chunks,
        &authz_tags,
        deps.embedder.model_version(),
    )
    .await?;

    let points: Vec<ChunkPoint> = embeddable
        .iter()
        .zip(embedded.vectors)
        .map(|(chunk, vector)| ChunkPoint {
            chunk_id: chunk.id,
            node_id: message.node_id,
            version: message.version,
            vector,
            authz_tags: authz_tags.clone(),
        })
        .collect();
    if points.is_empty() {
        // 見出しのみ等で埋め込み対象ゼロ: dimension=0 の collection 初期化を避け、
        // 旧版の残骸だけ掃除する（未インジェストなら 404 no-op）。
        deps.vector.delete_node(ctx, message.node_id).await?;
    } else {
        deps.vector.ensure_ready(embedded.dimension).await?;
        deps.vector.upsert(ctx, &points).await?;
        deps.vector
            .delete_stale_versions(ctx, message.node_id, message.version)
            .await?;
    }

    replace_fulltext(
        deps,
        ctx,
        message.node_id,
        embeddable
            .iter()
            .map(|c| (c.id, message.version, c.searchable_text()))
            .collect(),
        authz_tags,
    )
    .await?;

    Ok(IndexOutcome::Indexed {
        chunks: chunks.len(),
    })
}

/// フォルダの move/delete/restore を子孫ファイルの個別ジョブへ展開する。
///
/// 1 フォルダ＝1 ジョブで巨大サブツリーを処理せず、ファイル単位のジョブに分割して
/// jobq の vt/リトライ/冪等（(tenant, node, version, op)）へ乗せる。展開先の version は
/// 各ファイルの現行版（フォルダ操作で子の version は変わらないため、同一フォルダの
/// 再操作は展開先ジョブの冪等キーで自然に重複排除される）。
async fn fan_out_folder(
    deps: &Arc<PipelineDeps>,
    ctx: &AuthContext,
    message: &IngestMessage,
) -> Result<IndexOutcome, RagError> {
    let files = deps
        .indexer_storage
        .descendant_files(&ctx.tenant_id, message.node_id)
        .await?;
    if files.is_empty() {
        return Ok(IndexOutcome::Skipped("empty-folder"));
    }
    let mut tx = deps.pool.begin().await?;
    for (file_id, version) in &files {
        let child = IngestMessage {
            tenant_id: message.tenant_id.clone(),
            org: message.org.clone(),
            node_id: *file_id,
            version: *version,
            op: message.op.clone(),
            actor: message.actor.clone(),
        };
        jobq::enqueue_on(
            &mut tx,
            jobq::NewJob {
                queue: super::RAG_INGEST_QUEUE,
                tenant_id: &message.tenant_id,
                payload: &serde_json::to_value(&child)?,
                trace_id: None,
                max_attempts: deps.config.job_max_attempts,
            },
        )
        .await?;
    }
    tx.commit().await?;
    Ok(IndexOutcome::FannedOut { files: files.len() })
}

/// move: authz_tags の再評価のみ（Task 2.9）。
async fn retag_node(
    deps: &Arc<PipelineDeps>,
    ctx: &AuthContext,
    message: &IngestMessage,
) -> Result<IndexOutcome, RagError> {
    let Some(snapshot) = deps
        .indexer_storage
        .node_snapshot(&ctx.tenant_id, message.node_id)
        .await?
    else {
        return Ok(IndexOutcome::Skipped("node-missing"));
    };
    if snapshot.deleted || snapshot.kind != "file" {
        return Ok(IndexOutcome::Skipped("not-indexable"));
    }

    let authz_tags = compute_authz_tags(deps, ctx, message.node_id).await?;
    store::update_tags(&deps.pool, ctx, message.node_id, &authz_tags).await?;
    deps.vector
        .set_authz_tags(ctx, message.node_id, &authz_tags)
        .await?;

    // Tantivy は payload 更新ができないため rag_chunk（正本）から再投入する。
    let stored = store::chunks_for_node(&deps.pool, ctx, message.node_id).await?;
    let docs: Vec<(Uuid, i64, String)> = stored
        .iter()
        .filter(|c| c.kind != "parent")
        .map(|c| (c.id, c.version, c.searchable_text()))
        .collect();
    replace_fulltext(deps, ctx, message.node_id, docs, authz_tags).await?;
    Ok(IndexOutcome::Retagged)
}

/// delete: 全索引から除去（Task 2.9）。
async fn remove_node(
    deps: &Arc<PipelineDeps>,
    ctx: &AuthContext,
    message: &IngestMessage,
) -> Result<IndexOutcome, RagError> {
    // 復元後に遅延到着した古い delete イベントを無視する（生きている索引を消さない）。
    // 削除→復元で node.version は必ず進むため、「現行版が新しく・生存中」なら stale。
    if let Some(snapshot) = deps
        .indexer_storage
        .node_snapshot(&ctx.tenant_id, message.node_id)
        .await?
    {
        if !snapshot.deleted && snapshot.version > message.version {
            return Ok(IndexOutcome::Skipped("superseded"));
        }
    }
    deps.vector.delete_node(ctx, message.node_id).await?;
    let deps2 = Arc::clone(deps);
    let ctx2 = ctx.clone();
    let node_id = message.node_id;
    tokio::task::spawn_blocking(move || deps2.fulltext.delete_node(&ctx2, node_id))
        .await
        .map_err(|e| RagError::Fulltext(format!("spawn_blocking: {e}")))??;
    store::delete_node(&deps.pool, ctx, message.node_id).await?;
    Ok(IndexOutcome::Deleted)
}

/// authz_tags = file 自身 ＋ 祖先フォルダ群（名前空間化のまま・PIT-1 (b)）。
async fn compute_authz_tags(
    deps: &Arc<PipelineDeps>,
    ctx: &AuthContext,
    node_id: Uuid,
) -> Result<Vec<String>, RagError> {
    let ancestors = deps
        .indexer_storage
        .ancestor_folder_ids(&ctx.tenant_id, node_id)
        .await?;
    let mut tags = Vec::with_capacity(ancestors.len() + 1);
    tags.push(ctx.ns().file(&node_id.to_string()).as_str().to_string());
    for folder in ancestors {
        tags.push(ctx.ns().folder(&folder.to_string()).as_str().to_string());
    }
    Ok(tags)
}

/// Tantivy 差替え（同期 API のため spawn_blocking）。
async fn replace_fulltext(
    deps: &Arc<PipelineDeps>,
    ctx: &AuthContext,
    node_id: Uuid,
    docs: Vec<(Uuid, i64, String)>,
    authz_tags: Vec<String>,
) -> Result<(), RagError> {
    let deps2 = Arc::clone(deps);
    let ctx2 = ctx.clone();
    tokio::task::spawn_blocking(move || {
        let fulltext_docs: Vec<FulltextDoc<'_>> = docs
            .iter()
            .map(|(chunk_id, version, text)| FulltextDoc {
                chunk_id: *chunk_id,
                node_id,
                version: *version,
                text,
                authz_tags: &authz_tags,
            })
            .collect();
        deps2.fulltext.replace_node(&ctx2, node_id, &fulltext_docs)
    })
    .await
    .map_err(|e| RagError::Fulltext(format!("spawn_blocking: {e}")))?
}

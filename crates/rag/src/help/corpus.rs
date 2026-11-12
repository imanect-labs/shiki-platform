use crate::RagError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, fs::File, io::Read, path::Path};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct HelpConfig {
    pub enabled: bool,
    pub data_path: String,
    pub base_url: String,
    pub max_query_chars: usize,
    pub max_top_k: usize,
    pub max_result_chars: usize,
    pub max_corpus_bytes: usize,
    pub max_chunks: usize,
}
impl Default for HelpConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            data_path: "/opt/shiki/help".into(),
            base_url: "https://shiki.imanect.app".into(),
            max_query_chars: 1000,
            max_top_k: 10,
            max_result_chars: 2000,
            max_corpus_bytes: 64 * 1024 * 1024,
            max_chunks: 20000,
        }
    }
}
#[derive(Debug, Clone, Deserialize)]
pub struct HelpManifest {
    pub schema_version: u32,
    pub corpus_id: String,
    pub corpus_version: String,
    pub source_commit: String,
    pub visibility: String,
    pub inclusion: String,
    pub chunk_count: usize,
    pub asset_count: usize,
    pub chunks_bytes: usize,
    pub chunks_sha256: String,
}
#[derive(Debug, Clone, Deserialize)]
pub struct HelpChunk {
    pub schema_version: u32,
    pub corpus_version: String,
    pub page_id: String,
    pub chunk_id: String,
    pub title: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub heading_path: Vec<String>,
    pub relative_url: String,
    pub text: String,
    pub status: String,
    pub content_sha256: String,
}
/// ビルド時に作る密ベクトル契約。コーパス原本の manifest は変更しない。
#[derive(Debug, Deserialize, Serialize)]
pub struct VectorManifest {
    pub schema_version: u32,
    pub corpus_version: String,
    pub chunks_sha256: String,
    pub model_version: String,
    pub dimension: usize,
    pub normalized: bool,
    pub query_prefix: String,
    pub document_prefix: String,
    pub vectors_sha256: String,
}
#[derive(Debug, Deserialize, Serialize)]
pub(super) struct VectorRow {
    pub chunk_id: String,
    pub vector: Vec<f32>,
}
pub(super) fn invalid(message: &str) -> RagError {
    RagError::Config(format!("help corpus: {message}"))
}
pub(super) fn read(path: &Path, limit: usize) -> Result<Vec<u8>, RagError> {
    let file = File::open(path).map_err(|e| invalid(&e.to_string()))?;
    if !file
        .metadata()
        .map_err(|e| invalid(&e.to_string()))?
        .is_file()
    {
        return Err(invalid("regular file required"));
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| invalid(&e.to_string()))?;
    if bytes.len() > limit {
        return Err(invalid("size limit"));
    }
    Ok(bytes)
}
pub(super) fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(super) fn load(
    path: &Path,
    config: &HelpConfig,
) -> Result<(HelpManifest, Vec<HelpChunk>), RagError> {
    let manifest: HelpManifest =
        serde_json::from_slice(&read(&path.join("manifest.json"), 65536)?)?;
    if manifest.schema_version != 1
        || manifest.corpus_id != "shiki-help"
        || manifest.corpus_version != manifest.source_commit
        || manifest.source_commit.len() != 40
        || !manifest
            .source_commit
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        || manifest.visibility != "public"
        || manifest.asset_count != 0
        || manifest.chunk_count > config.max_chunks
    {
        return Err(invalid("manifest contract"));
    }
    let bytes = read(&path.join("chunks.jsonl"), config.max_corpus_bytes)?;
    if bytes.len() != manifest.chunks_bytes || hash(&bytes) != manifest.chunks_sha256 {
        return Err(invalid("chunks digest/size mismatch"));
    }
    let mut chunks = Vec::new();
    let mut ids = HashSet::new();
    for line in bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        let chunk: HelpChunk = serde_json::from_slice(line)?;
        if chunks.len() >= config.max_chunks
            || chunk.schema_version != 1
            || chunk.corpus_version != manifest.corpus_version
            || !matches!(chunk.status.as_str(), "reviewed" | "final")
            || hash(chunk.text.as_bytes()) != chunk.content_sha256
            || chunk.chunk_id.len() != 64
            || !chunk.chunk_id.bytes().all(|b| b.is_ascii_hexdigit())
            || !ids.insert(chunk.chunk_id.clone())
        {
            return Err(invalid("chunk contract/hash/duplicate"));
        }
        if chunk.summary.chars().count() > 2000
            || chunk.tags.len() > 100
            || chunk.aliases.len() > 100
            || chunk
                .tags
                .iter()
                .chain(&chunk.aliases)
                .map(String::len)
                .sum::<usize>()
                > 4000
            || chunk.title.chars().count() > 300
            || chunk.heading_path.len() > 32
            || chunk
                .heading_path
                .iter()
                .map(|h| h.chars().count())
                .sum::<usize>()
                > 1000
            || chunk.text.chars().count() > 4000
            || chunk.relative_url.len() > 2048
            || chunk.page_id.len() > 256
        {
            return Err(invalid("chunk field limits"));
        }
        validate_relative_url(&chunk.relative_url)?;
        chunks.push(chunk);
    }
    if chunks.len() != manifest.chunk_count {
        return Err(invalid("chunk count mismatch"));
    }
    Ok((manifest, chunks))
}
/// URL の解決前に正規化で消える traversal と外部参照を拒否する。
pub(super) fn validate_relative_url(value: &str) -> Result<(), RagError> {
    if !value.starts_with("/help/")
        || value.contains(['\\', '?'])
        || value.chars().any(char::is_control)
    {
        return Err(invalid("relative URL"));
    }
    let path = value
        .split('#')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if path.contains('%') || path.split('/').any(|s| matches!(s, "." | "..")) {
        return Err(invalid("noncanonical help path"));
    }
    Ok(())
}

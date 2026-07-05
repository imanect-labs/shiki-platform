//! Tantivy＋Lindera による [`FulltextIndex`] 実装（Task 2.5）。
//!
//! - **index-per-tenant**: `<index_data_dir>/<tenant_id>/` に 1 テナント 1 索引。
//!   テナント境界は index 選択で強制される（authz_tags と独立の防壁）。
//! - **日本語形態素**: Lindera（IPADIC 埋め込み）を `lang_ja` トークナイザとして登録。
//! - **単一ライタ**: IndexWriter はテナントごとに 1 本を Mutex で保持する。
//!   プロセス間の二重ライタは pipeline 側の advisory lock リーダー選出で防ぐ。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use authz::AuthContext;
use lindera::dictionary::load_dictionary;
use lindera::mode::Mode;
use lindera::segmenter::Segmenter;
use lindera_tantivy::tokenizer::LinderaTokenizer;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, Query, TermSetQuery};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value, INDEXED, STORED,
    STRING,
};
use tantivy::{Index, IndexReader, IndexWriter, TantivyDocument, Term};
use uuid::Uuid;

use crate::error::RagError;
use crate::fulltext::{FulltextDoc, FulltextIndex};
use crate::vector_store::{PreFilter, ScoredChunk};

const TOKENIZER_NAME: &str = "lang_ja";
/// IndexWriter のヒープ（テナントごと）。小規模テナント多数を想定した控えめな値。
const WRITER_HEAP_BYTES: usize = 20 * 1024 * 1024;

/// スキーマのフィールド束。
#[derive(Clone, Copy)]
struct Fields {
    chunk_id: Field,
    node_id: Field,
    text: Field,
    authz_tags: Field,
}

/// 1 テナント分の索引状態。
struct TenantIndex {
    index: Index,
    fields: Fields,
    writer: Arc<Mutex<IndexWriter>>,
    reader: IndexReader,
}

pub struct TantivyFulltext {
    base_dir: PathBuf,
    tenants: Mutex<HashMap<String, Arc<TenantIndex>>>,
}

impl TantivyFulltext {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        TantivyFulltext {
            base_dir: base_dir.into(),
            tenants: Mutex::new(HashMap::new()),
        }
    }

    fn schema() -> (Schema, Fields) {
        let mut builder = Schema::builder();
        let text_indexing = TextFieldIndexing::default()
            .set_tokenizer(TOKENIZER_NAME)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions);
        let text_options = TextOptions::default().set_indexing_options(text_indexing);
        let chunk_id = builder.add_text_field("chunk_id", STRING | STORED);
        let node_id = builder.add_text_field("node_id", STRING | STORED);
        let _version = builder.add_i64_field("version", INDEXED | STORED);
        let text = builder.add_text_field("text", text_options);
        let authz_tags = builder.add_text_field("authz_tags", STRING);
        let schema = builder.build();
        (
            schema,
            Fields {
                chunk_id,
                node_id,
                text,
                authz_tags,
            },
        )
    }

    fn register_tokenizer(index: &Index) -> Result<(), RagError> {
        let dictionary = load_dictionary("embedded://ipadic")
            .map_err(|e| RagError::Fulltext(format!("Lindera 辞書のロードに失敗: {e}")))?;
        let segmenter = Segmenter::new(Mode::Normal, dictionary, None);
        index
            .tokenizers()
            .register(TOKENIZER_NAME, LinderaTokenizer::from_segmenter(segmenter));
        Ok(())
    }

    /// テナント index を開く/作る（遅延・プロセス内キャッシュ）。
    fn tenant_index(&self, tenant_id: &str) -> Result<Arc<TenantIndex>, RagError> {
        let mut tenants = self
            .tenants
            .lock()
            .map_err(|_| RagError::Fulltext("tenants ロックが毒化しています".into()))?;
        if let Some(t) = tenants.get(tenant_id) {
            return Ok(Arc::clone(t));
        }

        let dir = self.tenant_dir(tenant_id);
        std::fs::create_dir_all(&dir).map_err(|e| {
            RagError::Fulltext(format!("index dir 作成失敗 {}: {e}", dir.display()))
        })?;
        let (schema, fields) = Self::schema();
        let mmap = tantivy::directory::MmapDirectory::open(&dir).map_err(|e| {
            RagError::Fulltext(format!("index dir オープン失敗 {}: {e}", dir.display()))
        })?;
        let index = Index::open_or_create(mmap, schema)
            .map_err(|e| RagError::Fulltext(format!("index オープン失敗: {e}")))?;
        Self::register_tokenizer(&index)?;
        let writer = index
            .writer(WRITER_HEAP_BYTES)
            .map_err(|e| RagError::Fulltext(format!("writer 作成失敗: {e}")))?;
        // commit 直後に明示 reload する（read-your-writes。OnCommitWithDelay は反映が
        // 非同期で、インジェスト完了＝検索可能の保証が崩れる）。
        let reader = index
            .reader_builder()
            .reload_policy(tantivy::ReloadPolicy::Manual)
            .try_into()
            .map_err(|e| RagError::Fulltext(format!("reader 作成失敗: {e}")))?;

        let tenant = Arc::new(TenantIndex {
            index,
            fields,
            writer: Arc::new(Mutex::new(writer)),
            reader,
        });
        tenants.insert(tenant_id.to_string(), Arc::clone(&tenant));
        Ok(tenant)
    }

    /// テナント index のディレクトリ。tenant_id は解決時に `| : # @`・空白を fail-closed
    /// 検証済み（SAAS.1）だが、パス区切りも防御的に置換する。
    fn tenant_dir(&self, tenant_id: &str) -> PathBuf {
        let safe: String = tenant_id
            .chars()
            .map(|c| {
                if c == '/' || c == '\\' || c == '.' {
                    '_'
                } else {
                    c
                }
            })
            .collect();
        self.base_dir.join(safe)
    }
}

impl FulltextIndex for TantivyFulltext {
    fn replace_node(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        docs: &[FulltextDoc<'_>],
    ) -> Result<(), RagError> {
        let tenant = self.tenant_index(&ctx.tenant_id)?;
        let mut writer = tenant
            .writer
            .lock()
            .map_err(|_| RagError::Fulltext("writer ロックが毒化しています".into()))?;
        writer.delete_term(Term::from_field_text(
            tenant.fields.node_id,
            &node_id.to_string(),
        ));
        for doc in docs {
            let mut d = TantivyDocument::new();
            d.add_text(tenant.fields.chunk_id, doc.chunk_id.to_string());
            d.add_text(tenant.fields.node_id, doc.node_id.to_string());
            d.add_text(tenant.fields.text, doc.text);
            for tag in doc.authz_tags {
                d.add_text(tenant.fields.authz_tags, tag);
            }
            writer
                .add_document(d)
                .map_err(|e| RagError::Fulltext(format!("文書追加失敗: {e}")))?;
        }
        writer
            .commit()
            .map_err(|e| RagError::Fulltext(format!("commit 失敗: {e}")))?;
        // read-your-writes: この呼び出しが返った時点で検索に反映されている。
        tenant
            .reader
            .reload()
            .map_err(|e| RagError::Fulltext(format!("reader reload 失敗: {e}")))?;
        Ok(())
    }

    fn delete_node(&self, ctx: &AuthContext, node_id: Uuid) -> Result<(), RagError> {
        self.replace_node(ctx, node_id, &[])
    }

    fn search(
        &self,
        ctx: &AuthContext,
        query_text: &str,
        limit: usize,
        prefilter: &PreFilter,
        exclude: &[Uuid],
    ) -> Result<Vec<ScoredChunk>, RagError> {
        // index-per-tenant: 自テナントの索引だけを開く（存在しなければヒット 0）。
        if !self.tenant_dir(&ctx.tenant_id).join("meta.json").exists() {
            return Ok(Vec::new());
        }
        let tenant = self.tenant_index(&ctx.tenant_id)?;

        // 本文クエリはクエリ側も同じ Lindera で形態素化し、**形態素の OR（BM25）**にする。
        // QueryParser は空白区切り 1 語が複数トークンに展開されると PhraseQuery（連続一致）
        // を組むため、「上期の拠点別売上は？」のような自然文クエリが 0 件になる。
        let Some(mut analyzer) = tenant.index.tokenizers().get(TOKENIZER_NAME) else {
            return Err(RagError::Fulltext("lang_ja トークナイザ未登録".into()));
        };
        let mut term_queries: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        {
            let mut stream = analyzer.token_stream(query_text);
            while let Some(token) = stream.next() {
                term_queries.push((
                    Occur::Should,
                    Box::new(tantivy::query::TermQuery::new(
                        Term::from_field_text(tenant.fields.text, &token.text),
                        tantivy::schema::IndexRecordOption::WithFreqs,
                    )),
                ));
            }
        }
        if term_queries.is_empty() {
            return Ok(Vec::new());
        }
        let text_query: Box<dyn Query> = Box::new(BooleanQuery::new(term_queries));

        let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![(Occur::Must, text_query)];
        // pre-filter: 可読タグのいずれかを持つ chunk のみ（dense 側と同じ権限境界）。
        if let PreFilter::Tags(tags) = prefilter {
            let terms: Vec<Term> = tags
                .iter()
                .map(|t| Term::from_field_text(tenant.fields.authz_tags, t))
                .collect();
            clauses.push((Occur::Must, Box::new(TermSetQuery::new(terms))));
        }
        if !exclude.is_empty() {
            let terms: Vec<Term> = exclude
                .iter()
                .map(|id| Term::from_field_text(tenant.fields.chunk_id, &id.to_string()))
                .collect();
            clauses.push((Occur::MustNot, Box::new(TermSetQuery::new(terms))));
        }
        let query = BooleanQuery::new(clauses);

        let searcher = tenant.reader.searcher();
        let top = searcher
            .search(&query, &TopDocs::with_limit(limit.max(1)))
            .map_err(|e| RagError::Fulltext(format!("検索失敗: {e}")))?;

        let mut out = Vec::with_capacity(top.len());
        for (score, addr) in top {
            let doc: TantivyDocument = searcher
                .doc(addr)
                .map_err(|e| RagError::Fulltext(format!("doc 取得失敗: {e}")))?;
            let get_uuid = |field: Field| -> Option<Uuid> {
                doc.get_first(field)
                    .and_then(|v| v.as_str())
                    .and_then(|s| Uuid::parse_str(s).ok())
            };
            let (Some(chunk_id), Some(node_id)) = (
                get_uuid(tenant.fields.chunk_id),
                get_uuid(tenant.fields.node_id),
            ) else {
                return Err(RagError::Fulltext("索引文書の ID が不正です".into()));
            };
            out.push(ScoredChunk {
                chunk_id,
                node_id,
                score,
            });
        }
        Ok(out)
    }

    fn purge_tenant(&self, tenant_id: &str) -> Result<(), RagError> {
        let mut tenants = self
            .tenants
            .lock()
            .map_err(|_| RagError::Fulltext("tenants ロックが毒化しています".into()))?;
        tenants.remove(tenant_id);
        let dir = self.tenant_dir(tenant_id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| {
                RagError::Fulltext(format!("index dir 削除失敗 {}: {e}", dir.display()))
            })?;
        }
        Ok(())
    }
}

impl TantivyFulltext {
    /// テナント index が物理的に存在するか（テスト・運用可視化用）。
    pub fn tenant_index_exists(&self, tenant_id: &str) -> bool {
        Path::new(&self.tenant_dir(tenant_id))
            .join("meta.json")
            .exists()
    }
}

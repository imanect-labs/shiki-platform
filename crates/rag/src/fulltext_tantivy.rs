//! Tantivy＋Lindera による [`FulltextIndex`] 実装（Task 2.5）。
//!
//! - **index-per-tenant**: `<index_data_dir>/<tenant_id>/` に 1 テナント 1 索引。
//!   テナント境界は index 選択で強制される（authz_tags と独立の防壁）。
//! - **日本語形態素**: Lindera（IPADIC 埋め込み）を `lang_ja` トークナイザとして登録。
//! - **単一ライタ**: IndexWriter はテナントごとに高々 1 本を Mutex で保持する。
//!   プロセス間の二重ライタは pipeline 側の advisory lock リーダー選出で防ぐ。
//!
//! # 常駐資源の上限（#816）
//!
//! フルプールでは 1 プロセスが数千テナントの索引を扱う。開いた索引（reader＋トークナイザ辞書）と
//! IndexWriter（ワーカー/マージスレッド＋ヒープ）を全テナント分常駐させるとスレッド・メモリ・
//! mmap が枯渇するため、[`TantivyLimits`] で上限を設ける:
//!
//! - **writer** は書込のときだけ開き、`idle_timeout` 書込が無ければ閉じる（[`TantivyFulltext::close_idle`]）。
//!   同時オープン数が `max_open_writers` に達したら、最も長く書いていない他テナントの writer を閉じて
//!   から開く。書込は毎回 commit して返るので、閉じても未 commit の操作は失われない。
//! - **索引**（reader）は LRU で `max_open_indexes` まで。writer を開いていない・誰も使っていない
//!   索引だけを閉じる（検索中の Searcher や書込中の writer を奪わない）。
//! - 上限は**ソフト**: 全部が使用中なら一時的に超えて開く（待たせて検索・インジェストを止めない）。
//!
//! 閉じ方の不変条件: 同じテナントの writer は**プロセス内で常に高々 1 本**（Tantivy のディレクトリ
//! ロック）。writer は自分の Mutex の中でだけ開閉し、索引エントリは writer が閉じていて参照が
//! 他に無いときだけマップから外すので、閉じかけの writer と新しい writer が並ぶことはない。
//! ロック順は「テナントマップ → writer」で、マップを持ったまま writer を**待つ**のは purge だけ
//! （他は `try_lock`）。writer を持ったままマップを待つ経路は無い。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use authz::AuthContext;
use lindera::dictionary::load_dictionary;
use lindera::mode::Mode;
use lindera::segmenter::Segmenter;
use lindera::tokenizer::Tokenizer as LTokenizer;
use lindera_tantivy::stream::LinderaTokenStream;
use tantivy::collector::TopDocs;
use tantivy::indexer::IndexWriterOptions;
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
/// writer 1 本あたりのマージスレッド数。既定（4）だと 1 writer で 6 スレッドになる
/// （索引 1・segment updater 1・マージ 4）。小さなテナント索引のマージは 1 本で足りる。
const WRITER_MERGE_THREADS: usize = 1;

/// プロセスで 1 部だけ持つ形態素解析器（#816）。
///
/// `LinderaTokenizer` は辞書ごと**深く複製**される（IPADIC の double-array だけで約 16MB を
/// deserialize / コピー）ので、索引ごとに作ると「開いている索引数 × 辞書」のメモリになり、
/// LRU・アイドル解放で索引を開き直すたびに読み直しも走る。辞書は読取専用なので `Arc` で共有する。
static JA_TOKENIZER: OnceLock<Result<Arc<LTokenizer>, String>> = OnceLock::new();

/// [`JA_TOKENIZER`] を共有する Tantivy トークナイザ。複製は `Arc` の参照を増やすだけ。
#[derive(Clone)]
struct SharedJaTokenizer {
    inner: Arc<LTokenizer>,
    token: tantivy::tokenizer::Token,
}

impl SharedJaTokenizer {
    fn new() -> Result<Self, RagError> {
        let inner = JA_TOKENIZER
            .get_or_init(|| {
                let dictionary = load_dictionary("embedded://ipadic")
                    .map_err(|e| format!("Lindera 辞書のロードに失敗: {e}"))?;
                let segmenter = Segmenter::new(Mode::Normal, dictionary, None);
                Ok(Arc::new(LTokenizer::new(segmenter)))
            })
            .clone()
            .map_err(RagError::Fulltext)?;
        Ok(SharedJaTokenizer {
            inner,
            token: tantivy::tokenizer::Token::default(),
        })
    }
}

impl tantivy::tokenizer::Tokenizer for SharedJaTokenizer {
    type TokenStream<'a> = LinderaTokenStream<'a>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> LinderaTokenStream<'a> {
        self.token.reset();
        // 上流（lindera-tantivy）は失敗で panic するが、ここではトークン 0 個として続ける
        // （埋め込み辞書の形態素解析は入力によらず失敗しない想定・失敗は記録する）。
        let tokens = self.inner.tokenize(text).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Lindera の形態素解析に失敗（トークン 0 個として扱う）");
            Vec::new()
        });
        LinderaTokenStream {
            tokens,
            token: &mut self.token,
            current_index: 0,
        }
    }
}

/// 常駐資源の上限（#816・モジュール冒頭）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TantivyLimits {
    /// 同時に開いておく索引（reader）の上限。`max_open_writers` 未満なら引き上げる。
    pub max_open_indexes: usize,
    /// 同時に開いておく IndexWriter の上限（1 本 = 3 スレッド＋最大 20MB のヒープ）。
    pub max_open_writers: usize,
    /// この時間使われなかった writer・索引を閉じる。
    pub idle_timeout: Duration,
}

impl Default for TantivyLimits {
    fn default() -> Self {
        TantivyLimits {
            max_open_indexes: 64,
            max_open_writers: 8,
            idle_timeout: Duration::from_mins(5),
        }
    }
}

impl TantivyLimits {
    /// 0 や逆転した値を、動く最小構成へ丸める（writer を開けない上限は書込を止める）。
    fn normalized(self) -> Self {
        let max_open_writers = self.max_open_writers.max(1);
        TantivyLimits {
            max_open_writers,
            max_open_indexes: self.max_open_indexes.max(max_open_writers),
            idle_timeout: self.idle_timeout,
        }
    }
}

/// スキーマのフィールド束。
#[derive(Clone, Copy)]
struct Fields {
    chunk_id: Field,
    node_id: Field,
    version: Field,
    text: Field,
    authz_tags: Field,
}

/// 1 テナント分の索引状態。
struct TenantIndex {
    index: Index,
    fields: Fields,
    /// `None` = 閉じている（次の書込で開き直す）。開閉はこの Mutex の中でだけ行う。
    writer: Mutex<Option<IndexWriter>>,
    reader: IndexReader,
    /// 最終利用（検索・書込）時刻。[`TantivyFulltext::epoch`] からのミリ秒。
    last_used: AtomicU64,
    /// 最終書込時刻（writer の LRU・アイドル判定用）。
    last_write: AtomicU64,
    /// purge 済み（マップから外した後も古い `Arc` を持つ呼び出しが、消したディレクトリへ
    /// writer を開き直して書かないようにする）。writer の Mutex の中で立てる・読む。
    retired: AtomicBool,
    /// [`TantivyFulltext::open_writers`] の共有カウンタ（writer を持ったまま捨てられたときに戻す）。
    open_writers: Arc<AtomicUsize>,
}

impl Drop for TenantIndex {
    fn drop(&mut self) {
        // 通常の閉じ方は take してから数を戻すので、ここに来るのは writer を持ったまま
        // 捨てられたとき（プロセス終了・purge 後に古い Arc で開き直した場合）だけ。
        if self.writer.get_mut().is_ok_and(|w| w.is_some()) {
            self.open_writers.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

impl TenantIndex {
    /// writer を開いているか（ロックせずに覗く。判定は目安で、確定は Mutex の中で行う）。
    fn writer_is_open(&self) -> Option<bool> {
        self.writer.try_lock().ok().map(|w| w.is_some())
    }
}

pub struct TantivyFulltext {
    base_dir: PathBuf,
    limits: TantivyLimits,
    tenants: Mutex<HashMap<String, Arc<TenantIndex>>>,
    /// 開いている writer の本数（上限判定用）。
    open_writers: Arc<AtomicUsize>,
    epoch: Instant,
}

impl TantivyFulltext {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self::with_limits(base_dir, TantivyLimits::default())
    }

    pub fn with_limits(base_dir: impl Into<PathBuf>, limits: TantivyLimits) -> Self {
        TantivyFulltext {
            base_dir: base_dir.into(),
            limits: limits.normalized(),
            tenants: Mutex::new(HashMap::new()),
            open_writers: Arc::new(AtomicUsize::new(0)),
            epoch: Instant::now(),
        }
    }

    /// 開いている（索引数, writer 数）。運用可視化・テスト用。
    pub fn open_counts(&self) -> (usize, usize) {
        let indexes = self.tenants.lock().map_or(0, |t| t.len());
        (indexes, self.open_writers.load(Ordering::Relaxed))
    }

    /// アイドルの writer・索引を閉じる周期タスクを起動する（tokio ランタイム上で呼ぶ）。
    ///
    /// `Weak` で持つので、`TantivyFulltext` が捨てられたら次の周期で終わる。
    pub fn spawn_idle_reaper(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let weak: Weak<Self> = Arc::downgrade(self);
        let period =
            (self.limits.idle_timeout / 2).clamp(Duration::from_secs(1), Duration::from_mins(1));
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(period);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                let Some(this) = weak.upgrade() else { return };
                // writer を閉じるとスレッド join・マージ待ちが入るので blocking プールで回す。
                match tokio::task::spawn_blocking(move || this.close_idle()).await {
                    Ok((writers, indexes)) if writers + indexes > 0 => {
                        tracing::debug!(
                            writers,
                            indexes,
                            "Tantivy: アイドルの writer・索引を閉じた"
                        );
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "Tantivy: アイドル解放タスクが失敗"),
                }
            }
        })
    }

    /// `idle_timeout` 使われていない writer と索引を閉じる。閉じた（writer 数, 索引数）を返す。
    ///
    /// 書込中の writer は `try_lock` で飛ばす（次の周期で拾う）。
    pub fn close_idle(&self) -> (usize, usize) {
        let now = self.now_ms();
        let idle = u64::try_from(self.limits.idle_timeout.as_millis()).unwrap_or(u64::MAX);
        let is_idle = |at: u64| now.saturating_sub(at) >= idle;
        let mut writers = 0;
        for tenant in self.snapshot() {
            if is_idle(tenant.last_write.load(Ordering::Relaxed)) && self.try_close_writer(&tenant)
            {
                writers += 1;
            }
        }
        let indexes = match self.tenants.lock() {
            Ok(mut map) => Self::evict(
                &mut map,
                |t| is_idle(t.last_used.load(Ordering::Relaxed)),
                0,
            ),
            Err(_) => 0,
        };
        (writers, indexes)
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// マップの Arc を複製して返す（マップのロックは直ちに離す）。
    fn snapshot(&self) -> Vec<Arc<TenantIndex>> {
        self.tenants
            .lock()
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default()
    }

    /// 他の誰も使っておらず writer も閉じている索引を、`eligible` に合う古い順に外す。
    ///
    /// `target_len` 以下になるまで外し、外した数を返す。参照数 1（マップだけが持つ）の判定は
    /// マップのロック下なので確定的（新たな複製はマップ経由でしか作れない）。
    fn evict(
        map: &mut HashMap<String, Arc<TenantIndex>>,
        eligible: impl Fn(&TenantIndex) -> bool,
        target_len: usize,
    ) -> usize {
        let mut victims: Vec<(u64, String)> = map
            .iter()
            .filter(|(_, t)| Arc::strong_count(t) == 1 && t.writer_is_open() == Some(false))
            .filter(|(_, t)| eligible(t))
            .map(|(k, t)| (t.last_used.load(Ordering::Relaxed), k.clone()))
            .collect();
        victims.sort_unstable();
        let mut evicted = 0;
        for (_, key) in victims {
            if map.len() <= target_len {
                break;
            }
            map.remove(&key);
            evicted += 1;
        }
        evicted
    }

    /// writer を（使用中でなければ）閉じる。閉じたら true。
    ///
    /// `wait_merging_threads` でワーカーとマージの完了まで待つ（ディレクトリロックの解放もここ）。
    /// 書込は毎回 commit しているので未 commit の操作は無い。
    fn try_close_writer(&self, tenant: &TenantIndex) -> bool {
        let Ok(mut slot) = tenant.writer.try_lock() else {
            return false;
        };
        let Some(writer) = slot.take() else {
            return false;
        };
        self.open_writers.fetch_sub(1, Ordering::Relaxed);
        if let Err(e) = writer.wait_merging_threads() {
            // commit 済みの内容は失われない（マージが途中で止まっただけ）。次に開けば続きから。
            tracing::warn!(error = %e, "Tantivy writer のクローズでマージが失敗");
        }
        true
    }

    /// writer の上限に達していたら、最も長く書いていない他テナントの writer を閉じて空ける。
    ///
    /// **自分の writer の Mutex を持つ前に**呼ぶ（マップのロックを取るため・ロック順）。
    fn make_room_for_writer(&self, current: &Arc<TenantIndex>) {
        if self.open_writers.load(Ordering::Relaxed) < self.limits.max_open_writers {
            return;
        }
        let mut others: Vec<Arc<TenantIndex>> = self
            .snapshot()
            .into_iter()
            .filter(|t| !Arc::ptr_eq(t, current))
            .collect();
        others.sort_unstable_by_key(|t| t.last_write.load(Ordering::Relaxed));
        for t in others {
            if self.open_writers.load(Ordering::Relaxed) < self.limits.max_open_writers {
                return;
            }
            self.try_close_writer(&t);
        }
        // 全部が書込中なら一時的に上限を超えて開く（ソフト上限・待たせない）。
    }

    fn open_writer(index: &Index) -> Result<IndexWriter, RagError> {
        let options = IndexWriterOptions::builder()
            .memory_budget_per_thread(WRITER_HEAP_BYTES)
            .num_worker_threads(1)
            .num_merge_threads(WRITER_MERGE_THREADS)
            .build();
        index
            .writer_with_options(options)
            .map_err(|e| RagError::Fulltext(format!("writer 作成失敗: {e}")))
    }

    /// テナントの writer で `op` を実行し commit・reader reload まで行う（read-your-writes）。
    ///
    /// 失敗したら writer を**捨てる**: 途中まで積んだ delete/add は commit されていないが writer の
    /// 中に残っており、そのままにすると次の書込の commit で一緒に確定してしまう。
    fn write_with(
        &self,
        tenant: &Arc<TenantIndex>,
        op: impl FnOnce(&mut IndexWriter, &Fields) -> Result<(), RagError>,
    ) -> Result<(), RagError> {
        if tenant.writer_is_open() != Some(true) {
            self.make_room_for_writer(tenant);
        }
        let mut slot = tenant
            .writer
            .lock()
            .map_err(|_| RagError::Fulltext("writer ロックが毒化しています".into()))?;
        if tenant.retired.load(Ordering::Relaxed) {
            return Err(RagError::Fulltext("テナント索引は撤去済みです".into()));
        }
        if slot.is_none() {
            *slot = Some(Self::open_writer(&tenant.index)?);
            self.open_writers.fetch_add(1, Ordering::Relaxed);
        }
        let Some(writer) = slot.as_mut() else {
            return Err(RagError::Fulltext("writer を開けませんでした".into()));
        };
        let result = op(writer, &tenant.fields).and_then(|()| {
            writer
                .commit()
                .map(|_| ())
                .map_err(|e| RagError::Fulltext(format!("commit 失敗: {e}")))
        });
        if let Err(e) = result {
            // drop = ワーカー停止（未 commit の操作は破棄される）＋ディレクトリロック解放。
            drop(slot.take());
            self.open_writers.fetch_sub(1, Ordering::Relaxed);
            return Err(e);
        }
        let now = self.now_ms();
        tenant.last_write.store(now, Ordering::Relaxed);
        tenant.last_used.store(now, Ordering::Relaxed);
        // read-your-writes: この呼び出しが返った時点で検索に反映されている。
        tenant
            .reader
            .reload()
            .map_err(|e| RagError::Fulltext(format!("reader reload 失敗: {e}")))
    }

    fn schema() -> (Schema, Fields) {
        let mut builder = Schema::builder();
        let text_indexing = TextFieldIndexing::default()
            .set_tokenizer(TOKENIZER_NAME)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions);
        let text_options = TextOptions::default().set_indexing_options(text_indexing);
        let chunk_id = builder.add_text_field("chunk_id", STRING | STORED);
        let node_id = builder.add_text_field("node_id", STRING | STORED);
        let version = builder.add_i64_field("version", INDEXED | STORED);
        let text = builder.add_text_field("text", text_options);
        let authz_tags = builder.add_text_field("authz_tags", STRING);
        let schema = builder.build();
        (
            schema,
            Fields {
                chunk_id,
                node_id,
                version,
                text,
                authz_tags,
            },
        )
    }

    fn register_tokenizer(index: &Index) -> Result<(), RagError> {
        index
            .tokenizers()
            .register(TOKENIZER_NAME, SharedJaTokenizer::new()?);
        Ok(())
    }

    /// テナント index を開く/作る（遅延・プロセス内キャッシュ・LRU で上限まで）。
    fn tenant_index(&self, tenant_id: &str) -> Result<Arc<TenantIndex>, RagError> {
        let mut tenants = self
            .tenants
            .lock()
            .map_err(|_| RagError::Fulltext("tenants ロックが毒化しています".into()))?;
        let now = self.now_ms();
        if let Some(t) = tenants.get(tenant_id) {
            t.last_used.store(now, Ordering::Relaxed);
            return Ok(Arc::clone(t));
        }
        if tenants.len() >= self.limits.max_open_indexes {
            // 1 つ空ける（使用中・writer を開いているものは外さないので、超過はソフト）。
            Self::evict(&mut tenants, |_| true, self.limits.max_open_indexes - 1);
        }

        // 開くのはマップのロック下で行う（索引ごとの直列化）。ロック外で開いて挿入時に再確認する形に
        // すると、同じテナントを 2 本が同時に開いたとき、遅れた側の `open_or_create` が先行側の
        // commit 後に空の meta.json を作り直して**索引を消し得る**。辞書は共有済み（[`JA_TOKENIZER`]）
        // なので、ロック下に残るのはディレクトリと meta の読取だけ。
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
        // commit 直後に明示 reload する（read-your-writes。OnCommitWithDelay は反映が
        // 非同期で、インジェスト完了＝検索可能の保証が崩れる）。
        let reader = index
            .reader_builder()
            .reload_policy(tantivy::ReloadPolicy::Manual)
            .try_into()
            .map_err(|e| RagError::Fulltext(format!("reader 作成失敗: {e}")))?;

        // writer は書込時に開く（検索しかしないテナントにスレッドとヒープを持たせない）。
        let tenant = Arc::new(TenantIndex {
            index,
            fields,
            writer: Mutex::new(None),
            reader,
            last_used: AtomicU64::new(now),
            last_write: AtomicU64::new(now),
            retired: AtomicBool::new(false),
            open_writers: Arc::clone(&self.open_writers),
        });
        tenants.insert(tenant_id.to_string(), Arc::clone(&tenant));
        Ok(tenant)
    }

    /// テナント index のディレクトリ。tenant_id は解決時に `| : # @`・空白を fail-closed
    /// 検証済み（SAAS.1）だが、パス安全化は**可逆エンコーディング**で行い、別テナントが
    /// 同一ディレクトリへ衝突しないことを保証する（`a.b` と `a_b` 等）。
    /// 英数と `-` はそのまま、`_` は `__`、それ以外は `_XX`（バイトの 16 進）に写す
    /// （単射なので逆像が一意＝衝突不能）。
    fn tenant_dir(&self, tenant_id: &str) -> PathBuf {
        self.base_dir.join(encode_tenant_dir(tenant_id))
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
        self.write_with(&tenant, |writer, fields| {
            writer.delete_term(Term::from_field_text(fields.node_id, &node_id.to_string()));
            for doc in docs {
                let mut d = TantivyDocument::new();
                d.add_text(fields.chunk_id, doc.chunk_id.to_string());
                d.add_text(fields.node_id, doc.node_id.to_string());
                d.add_i64(fields.version, doc.version);
                d.add_text(fields.text, doc.text);
                for tag in doc.authz_tags {
                    d.add_text(fields.authz_tags, tag);
                }
                writer
                    .add_document(d)
                    .map_err(|e| RagError::Fulltext(format!("文書追加失敗: {e}")))?;
            }
            Ok(())
        })
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
        scope_tags: &[String],
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
        // 知識スコープ（Task 6.8）: 権限境界と独立の AND 句（TenantOnly 縮退時も維持）。
        if !scope_tags.is_empty() {
            let terms: Vec<Term> = scope_tags
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
        // マップを持ったまま消す（並行する open がディレクトリを作り直さないように）。
        let mut tenants = self
            .tenants
            .lock()
            .map_err(|_| RagError::Fulltext("tenants ロックが毒化しています".into()))?;
        if let Some(tenant) = tenants.remove(tenant_id) {
            // 書込中ならその完了を待ってから writer を閉じる（スレッドを残したまま
            // ディレクトリを消さない）。writer を持ったままマップを待つ経路は無いので
            // ここで待ってもデッドロックしない（モジュール冒頭のロック順）。
            let mut slot = tenant
                .writer
                .lock()
                .map_err(|_| RagError::Fulltext("writer ロックが毒化しています".into()))?;
            tenant.retired.store(true, Ordering::Relaxed);
            if let Some(writer) = slot.take() {
                self.open_writers.fetch_sub(1, Ordering::Relaxed);
                if let Err(e) = writer.wait_merging_threads() {
                    tracing::warn!(error = %e, "purge: Tantivy writer のクローズでマージが失敗");
                }
            }
        }
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

/// tenant_id → ディレクトリ名の単射エンコーディング。
fn encode_tenant_dir(tenant_id: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(tenant_id.len());
    for b in tenant_id.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' => out.push(b as char),
            b'_' => out.push_str("__"),
            other => {
                let _ = write!(out, "_{other:02x}");
            }
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn tenant_dir_encoding_is_injective() {
        // `a.b` / `a_b` / `a_2eb` のような紛らわしい組が衝突しないこと。
        let cases = ["a.b", "a_b", "a-b", "a_2eb", "a__b", "A.B", "テナント"];
        let encoded: Vec<String> = cases.iter().map(|t| encode_tenant_dir(t)).collect();
        let unique: std::collections::HashSet<&String> = encoded.iter().collect();
        assert_eq!(unique.len(), cases.len(), "エンコード衝突: {encoded:?}");
        // パス区切り・親参照が残らないこと。
        for e in &encoded {
            assert!(!e.contains('/') && !e.contains('\\') && !e.contains(".."));
        }
        assert_eq!(encode_tenant_dir("a.b"), "a_2eb");
        assert_eq!(encode_tenant_dir("a_b"), "a__b");
    }
}

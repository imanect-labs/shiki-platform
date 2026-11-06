//! `web_translate` ツール: URL のページ本文を指定言語へ訳し、**訳文を会話に直接表示する**
//! （設計メモ `docs/agent/web-translate.md`）。
//!
//! 固定で進めたい処理（取得・抽出・保護・分割・用語集・翻訳・検査・組み立て）をツールに置き、
//! 裁量の部分（訳調・対象言語・用語指定・いつ使うか）は公式 skill `translate` が持つ。
//!
//! # 境界（守る不変条件）
//!
//! - **取得は `web_fetch` と同じ経路**（[`WebFetchTool::fetch`]・PIT-48。ガードを二重実装しない）。
//! - **LLM は llm-gateway だけ**（予算・使用量記録に乗る）。翻訳・用語集の呼び出しには**ツールを一切渡さない**。
//! - **訳文をモデルへ戻さない**。モデルへの `content` はこちらが生成した要約値だけで、訳文は
//!   `ToolOutcome::translations` から UI のブロックになる（転記で出力トークンを二重に払わない・PIT-56）。
//! - **LLM の出力を固定ルールで検査し**、外れたチャンクだけ理由を添えて再試行する。通らなければ原文のまま残す。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use authz::AuthContext;
use futures::StreamExt as _;
use llm_gateway::{Effort, GenerateRequest, LlmError, LlmGateway, Message, Role, StopReason};

use crate::tool::{Tool, ToolError, ToolOutcome, ToolUsage};
use crate::tools::web_fetch::{Fetched, FetchedPage, WebFetchTool};
use crate::vocab::ToolName;

mod assemble;
mod backend;
mod check;
mod chunk;
mod prompt;
mod protect;

#[cfg(test)]
mod tests;

use check::Script;
use prompt::{Context, Term};

/// 翻訳の上限と並列度。
#[derive(Debug, Clone, Copy)]
pub struct TranslateLimits {
    /// 1 チャンクの本文の目安（文字）。
    pub chunk_chars: usize,
    /// これ未満の塊は次の見出しで切らずに結合する（呼び出し回数を抑える）。
    pub min_merge_chars: usize,
    /// チャンク数の上限（超えたら翻訳せず範囲を絞ってもらう・敵対的入力とコストの歯止め）。
    pub max_chunks: usize,
    /// 同時に訳すチャンク数。
    pub parallel: usize,
    /// 1 チャンクの試行回数（初回＋再試行）。
    pub max_attempts: usize,
}

impl Default for TranslateLimits {
    fn default() -> Self {
        TranslateLimits {
            chunk_chars: 2_500,
            min_merge_chars: 300,
            max_chunks: 60,
            parallel: 4,
            max_attempts: 3,
        }
    }
}

/// 1 チャンクの出力上限の天井（トークン）。
const MAX_OUTPUT_TOKENS: usize = 32_768;

/// コンテキスト長が分からないモデルの保守的な既定（トークン）。
const DEFAULT_CONTEXT_TOKENS: usize = 32_000;

/// 対象言語のコードの上限（`zh-Hant-TW` 程度まで）。
const MAX_LANG_CHARS: usize = 16;

/// `web_translate` ツール（1 run につき 1 インスタンス・呼び出し回数を冪等キーに使う）。
pub struct WebTranslateTool {
    fetch: WebFetchTool,
    backend: Arc<dyn backend::Backend>,
    model: Option<String>,
    session_id: Option<String>,
    /// 冪等キーの接頭辞（親の `<run_id>:<fencing>`）。`:wt<n>:<チャンク>:<試行>` を足す。
    idempotency_prefix: String,
    cancel: Arc<AtomicBool>,
    calls: AtomicUsize,
    limits: TranslateLimits,
}

impl WebTranslateTool {
    /// 翻訳ツールを作る。`fetch` は会話に提示している `web_fetch` と同じ設定のもの（同じ取得経路）。
    #[must_use]
    pub fn new(
        fetch: WebFetchTool,
        gateway: LlmGateway,
        idempotency_prefix: String,
        cancel: Arc<AtomicBool>,
    ) -> Self {
        WebTranslateTool {
            fetch,
            backend: Arc::new(backend::GatewayBackend(gateway)),
            model: None,
            session_id: None,
            idempotency_prefix,
            cancel,
            calls: AtomicUsize::new(0),
            limits: TranslateLimits::default(),
        }
    }

    /// 訳に使う論理モデル（未指定は gateway 既定。会話で選んだモデルを渡す）。
    #[must_use]
    pub fn with_model(mut self, model: Option<String>) -> Self {
        self.model = model;
        self
    }

    /// 会話 ID（プロバイダのルーティング・キャッシュの手がかり）。
    #[must_use]
    pub fn with_session_id(mut self, session_id: Option<String>) -> Self {
        self.session_id = session_id;
        self
    }

    #[must_use]
    pub fn with_limits(mut self, limits: TranslateLimits) -> Self {
        self.limits = limits;
        self
    }
}

/// 入力の解釈結果。
struct Request {
    url: String,
    lang: String,
    style: Option<String>,
    terms: Vec<Term>,
}

fn parse_input(input: &serde_json::Value) -> Result<Request, ToolError> {
    let url = input
        .get("url")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| ToolError::Invalid("missing 'url'".into()))?
        .to_string();
    let lang = input
        .get("target_lang")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("ja")
        .trim()
        .to_string();
    if lang.is_empty()
        || lang.chars().count() > MAX_LANG_CHARS
        || !lang.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err(ToolError::Invalid(
            "target_lang は言語コード（例: ja・en・zh-CN）で指定してください".into(),
        ));
    }
    let style = input
        .get("style")
        .and_then(serde_json::Value::as_str)
        .map(|s| prompt::one_line(s, prompt::MAX_STYLE_CHARS))
        .filter(|s| !s.is_empty());
    let terms = input
        .get("glossary")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|t| {
                    prompt::valid_term(t.get("source")?.as_str()?, t.get("target")?.as_str()?)
                })
                .take(prompt::MAX_USER_TERMS)
                .collect()
        })
        .unwrap_or_default();
    Ok(Request {
        url,
        lang,
        style,
        terms,
    })
}

/// LLM の呼び出し 1 回の結果。
struct Generated {
    text: String,
    truncated: bool,
}

/// チャンク 1 つの結果。
pub(super) struct ChunkResult {
    /// 訳文（失敗なら原文）。プレースホルダは未復元。
    pub text: String,
    pub failed: bool,
    pub attempts: usize,
}

/// 1 回の呼び出しで共有する入力（チャンクの翻訳へ渡す束）。
struct Job<'a> {
    ctx: &'a AuthContext,
    trace_id: Option<&'a str>,
    shared: &'a Shared,
    req: &'a Request,
    call_no: usize,
    title: Option<&'a str>,
    chunks: &'a [chunk::Chunk],
    terms: &'a [Term],
    protected: &'a protect::Protected,
}

/// 呼び出し全体で共有する状態（使用量の合計・打ち切り）。
#[derive(Default)]
struct Shared {
    usage: std::sync::Mutex<(u64, u64, i64)>,
    /// 利用枠の超過など、続けても通らない失敗（以降のチャンクを訳さない）。
    abort: std::sync::Mutex<Option<String>>,
}

impl WebTranslateTool {
    /// 1 回生成する（会計と計装は backend が記録する。消費は失敗しても親の予算へ積む）。
    async fn generate(
        &self,
        job: &Job<'_>,
        system: String,
        user: String,
        max_tokens: u32,
        key: String,
    ) -> Result<Generated, LlmError> {
        let mut req = GenerateRequest::new(vec![Message::text(Role::User, user)]);
        req.model.clone_from(&self.model);
        req.system = Some(system);
        req.max_tokens = Some(max_tokens);
        req.temperature = Some(0.2);
        // 訳は思考を長く要しない。思考強度に対応しないモデルでは gateway が落とす。
        req.effort = Some(Effort::Low);
        req.session_id.clone_from(&self.session_id);
        let reply = self
            .backend
            .generate(job.ctx, req, key, job.trace_id)
            .await?;
        if let Ok(mut u) = job.shared.usage.lock() {
            u.0 += reply.spent.0;
            u.1 += reply.spent.1;
            u.2 += reply.spent.2;
        }
        if let Some(e) = reply.error {
            return Err(e);
        }
        Ok(Generated {
            text: reply.text,
            truncated: reply.stop == StopReason::MaxTokens,
        })
    }

    fn input_budget_tokens(&self, source: &str, system: &str) -> usize {
        let window = self
            .backend
            .context_window(self.model.as_deref())
            .map_or(DEFAULT_CONTEXT_TOKENS, |w| w as usize);
        // 出力の確保分: 本文の見積もり × 膨張係数 2 ＋ 思考の余裕。
        let reserve = prompt::estimate_tokens(source) * 2 + 4_096;
        window.saturating_sub(reserve + prompt::estimate_tokens(system))
    }

    /// 1 チャンクを訳す（検査に通るまで理由を添えて再試行し、通らなければ原文のまま）。
    async fn translate_chunk(&self, job: &Job<'_>, i: usize) -> ChunkResult {
        let (shared, req, call_no) = (job.shared, job.req, job.call_no);
        let chunk = &job.chunks[i];
        let original = || ChunkResult {
            text: chunk.text.clone(),
            failed: false,
            attempts: 0,
        };
        // プレースホルダと空白しか無いチャンク（コードだけ等）は送らない。
        if !check::has_translatable_text(&chunk.text) {
            return original();
        }
        let system = prompt::translate_system(&req.lang, req.style.as_deref());
        let mut ctx_parts = Context::build(job.title, job.chunks, i, job.terms);
        let budget = self.input_budget_tokens(&chunk.text, &system);
        while prompt::estimate_tokens(&prompt::translate_user(&ctx_parts, &chunk.text, &[]))
            > budget
        {
            if !ctx_parts.shrink() {
                // 文脈を空にしても収まらない（巨大な 1 行の表など）。訳さず原文のまま残す。
                return ChunkResult {
                    text: chunk.text.clone(),
                    failed: true,
                    attempts: 0,
                };
            }
        }
        #[allow(clippy::cast_possible_truncation)]
        let mut max_tokens = (prompt::estimate_tokens(&chunk.text) * 3 + 4_096)
            .clamp(8_192, MAX_OUTPUT_TOKENS) as u32;
        let target = Script::of_lang(&req.lang);
        let mut feedback: Vec<String> = Vec::new();
        let mut attempts = 0;
        for attempt in 0..self.limits.max_attempts {
            if self.cancel.load(Ordering::SeqCst) || shared.abort.lock().is_ok_and(|a| a.is_some())
            {
                break;
            }
            attempts += 1;
            let user = prompt::translate_user(&ctx_parts, &chunk.text, &feedback);
            let key = format!("{}:wt{call_no}:{i}:{attempt}", self.idempotency_prefix);
            match self
                .generate(job, system.clone(), user, max_tokens, key)
                .await
            {
                Ok(out) => {
                    let text = check::unwrap_output(&out.text);
                    let mut reasons = check::check(&chunk.text, &text, job.protected, target);
                    if out.truncated {
                        // 推論型のモデルは思考で枠を使い切ることがある（実測で本文が空のまま切れた）。
                        // 次の試行は枠を広げる。
                        #[allow(clippy::cast_possible_truncation)]
                        {
                            max_tokens = (max_tokens * 2).min(MAX_OUTPUT_TOKENS as u32);
                        }
                        reasons.insert(
                            0,
                            "the output was cut off; translate without commentary".into(),
                        );
                    }
                    // 最後の試行で段落の訳し残しだけが残ったら、その訳を採る（全体を原文へ戻すより良い）。
                    let last = attempt + 1 == self.limits.max_attempts;
                    if reasons.is_empty() || (last && check::only_soft(&reasons)) {
                        return ChunkResult {
                            text,
                            failed: false,
                            attempts,
                        };
                    }
                    tracing::info!(
                        target: "web_translate",
                        chunk = i,
                        attempt,
                        reasons = ?reasons,
                        "訳文の検査に失敗"
                    );
                    feedback = reasons;
                }
                Err(LlmError::RateLimited(msg)) => {
                    if let Ok(mut a) = shared.abort.lock() {
                        a.get_or_insert(msg);
                    }
                    break;
                }
                Err(e) => {
                    tracing::warn!(target: "web_translate", chunk = i, attempt, error = %e, "翻訳の呼び出しに失敗");
                    feedback.clear();
                }
            }
        }
        ChunkResult {
            text: chunk.text.clone(),
            failed: true,
            attempts,
        }
    }

    /// 用語集を作る（失敗しても翻訳は続ける）。訳したタイトルも返す。
    ///
    /// `job.terms` はまだ空（ここで作る）。
    async fn glossary(&self, job: &Job<'_>) -> (Option<String>, Vec<Term>) {
        let (shared, req, call_no) = (job.shared, job.req, job.call_no);
        let head: String = job
            .protected
            .text
            .chars()
            .take(prompt::GLOSSARY_SOURCE_CHARS)
            .collect();
        if !check::has_translatable_text(&head) {
            return (None, Vec::new());
        }
        let key = format!("{}:wt{call_no}:g", self.idempotency_prefix);
        match self
            .generate(
                job,
                prompt::glossary_system(&req.lang),
                prompt::glossary_user(job.title, &head),
                // 推論型のモデルは思考で枠を使う（8,192 では JSON を書く前に切れた・Wikipedia の実測）。
                16_384,
                key,
            )
            .await
        {
            Ok(out) => {
                let (title, terms) = prompt::parse_glossary(&out.text, &head);
                if terms.is_empty() {
                    // 形の合わない応答・出力上限での打ち切りを観測できるようにする（本文は載せない）。
                    tracing::info!(
                        target: "web_translate",
                        chars = out.text.chars().count(),
                        truncated = out.truncated,
                        "用語集が空（応答の形が合わないか、該当する語が無い）"
                    );
                }
                (title, terms)
            }
            Err(LlmError::RateLimited(msg)) => {
                if let Ok(mut a) = shared.abort.lock() {
                    a.get_or_insert(msg);
                }
                (None, Vec::new())
            }
            Err(e) => {
                tracing::warn!(target: "web_translate", error = %e, "用語集の呼び出しに失敗（用語集なしで続ける）");
                (None, Vec::new())
            }
        }
    }
}

#[async_trait::async_trait]
impl Tool for WebTranslateTool {
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        ToolName::WebTranslate.as_str()
    }

    #[allow(clippy::unnecessary_literal_bound)]
    fn description(&self) -> &str {
        "URL のページを取得し、本文を指定した言語へ翻訳して**会話に直接表示する**（見出し・リスト・表・\
         コード・リンク・画像の位置を保つ）。ユーザーが Web ページの翻訳を求めたときに使う。\
         訳文はこのツールが表示するので、回答に訳文を書き写さないこと（結果には要約だけが返る）。\
         長いページはチャンクに割って並列に訳す。取得できるのは公開ホストの http/https のみ。"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "翻訳するページの URL（http/https）" },
                "target_lang": {
                    "type": "string",
                    "description": "訳す先の言語コード（例: ja・en・zh-CN・ko）。既定は ja"
                },
                "style": {
                    "type": "string",
                    "description": "訳調の指定（任意・200 文字まで。例: 「だ・である調」「技術文書として簡潔に」）"
                },
                "glossary": {
                    "type": "array",
                    "description": "ユーザーが指定した訳語（任意・30 語まで）。ページから作る用語集より優先する。\
                                    訳さない語は target を source と同じにする",
                    "items": {
                        "type": "object",
                        "properties": {
                            "source": { "type": "string" },
                            "target": { "type": "string" }
                        },
                        "required": ["source", "target"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["url"],
            "additionalProperties": false
        })
    }

    // 書込の副作用は無い（出力は会話の表示だけ）。取得は egress なので自律版ではゲートが掛かる。
    fn requires_confirmation(&self) -> bool {
        false
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let req = parse_input(&input)?;
        let call_no = self.calls.fetch_add(1, Ordering::SeqCst);
        let fetched = match self.fetch.fetch(ctx, &req.url).await? {
            Fetched::Page(f) => f,
            Fetched::Observation(outcome) => return Ok(outcome),
        };
        let FetchedPage {
            page,
            url,
            status,
            source_capped,
            ..
        } = fetched;
        if !(200..300).contains(&status) {
            return Ok(ToolOutcome::error(format!(
                "HTTP {status} | {url}\nページを取得できなかったため翻訳しません"
            )));
        }
        if page.body.trim().is_empty() {
            return Ok(ToolOutcome::error(format!(
                "HTTP {status} | {url}\n本文を取り出せなかったため翻訳しません"
            )));
        }

        let protected = protect::protect(&page.body);
        let chunks = chunk::split(
            &protected.text,
            self.limits.chunk_chars,
            self.limits.min_merge_chars,
        );
        if chunks.len() > self.limits.max_chunks {
            return Ok(ToolOutcome::error(format!(
                "{url}\nページが長すぎるため翻訳しません（{} チャンク・上限 {}）。\
                 ユーザーに訳したい節を確かめ、web_fetch の query で範囲を絞って読んでください。",
                chunks.len(),
                self.limits.max_chunks
            )));
        }

        let shared = Shared::default();
        let started = std::time::Instant::now();
        let mut job = Job {
            ctx,
            trace_id,
            shared: &shared,
            req: &req,
            call_no,
            title: page.title.as_deref(),
            chunks: &chunks,
            terms: &[],
            protected: &protected,
        };
        let (translated_title, page_terms) = self.glossary(&job).await;
        let terms = prompt::merge_terms(&req.terms, page_terms);
        job.terms = &terms;

        let job = &job;
        let mut results: Vec<(usize, ChunkResult)> = futures::stream::iter(0..chunks.len())
            .map(|i| async move { (i, self.translate_chunk(job, i).await) })
            .buffer_unordered(self.limits.parallel.max(1))
            .collect()
            .await;
        results.sort_by_key(|(i, _)| *i);
        let results: Vec<ChunkResult> = results.into_iter().map(|(_, r)| r).collect();

        let (tokens, fresh_tokens, cost) = shared.usage.lock().map_or((0, 0, 0), |u| *u);
        let usage = Some(ToolUsage {
            tokens,
            fresh_tokens,
            cost_usd_micros: cost,
        });
        let stopped = if self.cancel.load(Ordering::SeqCst) {
            Some("翻訳は停止されました".to_string())
        } else {
            shared
                .abort
                .lock()
                .ok()
                .and_then(|a| a.clone())
                .map(|msg| format!("{msg} 翻訳はやり直さないこと。"))
        };
        if let Some(msg) = stopped {
            let mut out = ToolOutcome::error(msg);
            out.usage = usage;
            return Ok(out);
        }

        let assembled = assemble::assemble(&chunks, &results, &protected);
        tracing::info!(
            target: "web_translate",
            url = %url,
            lang = %req.lang,
            chunks = chunks.len(),
            failed = assembled.failed_chunks.len(),
            attempts = results.iter().map(|r| r.attempts).sum::<usize>(),
            glossary = terms.len(),
            chars_in = page.body.chars().count(),
            chars_out = assembled.markdown.chars().count(),
            elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            tokens,
            "web_translate 完了"
        );
        let mut out = ToolOutcome::ok(summary(
            &url,
            &req.lang,
            chunks.len(),
            &assembled,
            source_capped,
        ));
        out.translations.push(serde_json::json!({
            "source_url": url.as_str(),
            "title": page.title,
            "translated_title": translated_title,
            "target_lang": req.lang,
            "markdown": assembled.markdown,
            "failed_chunks": assembled.failed_chunks,
            "chunks": chunks.len(),
            "source_images": protected.image_urls,
            "source_truncated": source_capped,
            "fetched_at": chrono::Utc::now().to_rfc3339(),
        }));
        out.usage = usage;
        Ok(out)
    }
}

/// モデルへ返す要約（こちらが生成した値だけ。訳文・タイトル・用語集はページ由来なので載せない）。
fn summary(
    url: &reqwest::Url,
    lang: &str,
    chunks: usize,
    assembled: &assemble::Assembled,
    source_capped: bool,
) -> String {
    use std::fmt::Write as _;
    let mut content = format!(
        "{url} を {lang} へ翻訳し、訳文を会話に表示しました（{chunks} チャンク・訳文 {} 文字",
        assembled.markdown.chars().count()
    );
    let failed = assembled.failed_chunks.len();
    if failed > 0 {
        let _ = write!(content, "・翻訳できず原文のまま残した節 {failed}");
    }
    content.push('）');
    if source_capped {
        content.push_str("。ページが長く、取得上限までの範囲だけを訳しています");
    }
    content.push_str("。訳文は回答に書き写さず、表示したことを短く伝えてください。");
    content
}

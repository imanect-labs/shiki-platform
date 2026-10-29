//! llm_decider — chat モデル（Haiku 5.5 など）に構造化出力で決定を真似させる決定器（FEAT-1）。
//!
//! 設計の正: docs/design.md §4.5（決定モデル）。プロンプト・出力の形・読み取りは
//! [`super::llm_decider_prompt`]、確からしさの出し方は [`super::llm_decider_vote`]。
//!
//! - 呼ぶのは構築時に渡された元のモデルのアダプタ（[`LlmProvider`]）。組織の許可（決定モデルと元の
//!   モデルの両方）・利用上限・会計は gateway の [`decide`] が先に済ませている。
//! - 出力の形はモデルのモードに合わせる（[`structured::apply_mode`]。Anthropic の対応モデルは制約付き
//!   デコード、それ以外は指示）。どのモードでも本文は型付きで読み直す。
//! - 1 回の答えは、形に合わなければ 1 回だけ直させる。拒否と上限での途切れは直させない。直しても合わない
//!   回が 1 つでもあれば判定全体を失敗にし、それまでに使ったトークン量を合わせて返す。
//! - 温度は送らない（Haiku 5.5 は既定以外の温度を受けない）。思考の深さは既定 low、上限のトークン数は
//!   low で 2048、それより深いと 4096（思考も上限に数えるため）。
//! - 確からしさは較正されていない（`calibrated = false`・PIT-80）。自己申告（1 回）で、一様より低い申告を引き上げた
//!   ときは、引き上げた質問のキーと数をログに残す（取り違えや迷いの合図が値から消えるので・BUG-124。材料と答えは
//!   載せない）。
//!
//! [`decide`]: crate::gateway::LlmGateway::decide

use std::sync::Arc;

use futures::StreamExt;

/// プロンプトの版（答えた版 `resolved_model` の `llm_decider/<版>/` に載る）。評価の凍結（`eval/decision/frozen.json`）
/// はこの値と照らす（版を上げたら凍結し直す・PIT-80）。
pub use super::llm_decider_prompt::PROMPT_VERSION;
use super::llm_decider_prompt::{self as prompt, Reply};
use super::llm_decider_vote::{aggregate, raised_below_uniform};
use crate::config::DecisionTuning;
use crate::decision::{
    DecideFailure, DecideRequest, DecideResponse, DecisionMethod, DecisionProvider,
    INVALID_OUTPUT_HEAD, REFUSED, TRUNCATED,
};
use crate::model::{
    Effort, GenerateRequest, Message, ResponseFormat, Role, StopReason, StreamDelta, Usage,
};
use crate::provider::{LlmError, LlmProvider};
use crate::retry::{resume_unsettled, stream_with_retry, MIDSTREAM_RETRIES, UNAVAILABLE_BACKOFF};
use crate::structured::{self, StructuredOutputMode};

/// 自己申告の確からしさを引き上げたときのログの target（数えるときに絞り込む名前・BUG-124）。
pub const LOW_SELF_REPORT_TARGET: &str = "llm_decider.low_self_report";

/// 思考の深さが low のときの上限のトークン数。
const MAX_TOKENS_LOW: u32 = 2048;
/// medium・high のときの上限のトークン数（思考が長くなる分）。
const MAX_TOKENS_DEEP: u32 = 4096;

/// chat モデルに決定を真似させる決定器。
pub struct LlmDecider {
    provider: Arc<dyn LlmProvider>,
    real_id: String,
    effort: Option<Effort>,
    mode: StructuredOutputMode,
    samples: u8,
    max_tokens: u32,
    resolved: String,
}

impl LlmDecider {
    /// `real_id` は元のモデルの実 ID、`supports_effort` は元のモデルが思考の深さを受けるか、`mode` は
    /// 元のモデルの構造化出力のモード。`tuning.samples` は 1・3・5（構築時に確かめてある）。
    #[must_use]
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        real_id: impl Into<String>,
        supports_effort: bool,
        mode: StructuredOutputMode,
        tuning: DecisionTuning,
    ) -> Self {
        let real_id = real_id.into();
        let effort = supports_effort.then_some(tuning.effort);
        let max_tokens = if tuning.effort == Effort::Low {
            MAX_TOKENS_LOW
        } else {
            MAX_TOKENS_DEEP
        };
        let resolved = format!(
            "{real_id}+llm_decider/{}/k{}/{}",
            prompt::PROMPT_VERSION,
            tuning.samples,
            effort.map_or("none", Effort::as_str)
        );
        LlmDecider {
            provider,
            real_id,
            effort,
            mode,
            samples: tuning.samples.max(1),
            max_tokens,
            resolved,
        }
    }

    /// 答えるときの決定器の版（`{元の実 ID}+llm_decider/{プロンプトの版}/k{回数}/{思考の深さ}`・決定キャッシュの
    /// キーにも入る）。
    #[must_use]
    pub fn resolved_model(&self) -> &str {
        &self.resolved
    }

    /// 1 回目の要求（形の指定はモデルのモードに合わせて整えたもの）。
    fn base_request(&self, req: &DecideRequest) -> GenerateRequest {
        let mut g = GenerateRequest::new(vec![Message::text(Role::User, prompt::user_text(req))]);
        g.model = Some(self.real_id.clone());
        g.system = Some(prompt::SYSTEM.to_string());
        g.effort = self.effort;
        g.max_tokens = Some(self.max_tokens);
        g.response_format = Some(ResponseFormat::JsonSchema {
            name: prompt::SCHEMA_NAME.to_string(),
            schema: prompt::schema(req),
        });
        structured::apply_mode(&mut g, self.mode);
        g
    }

    /// 1 回呼んで本文・停止理由・使用量を集める（途中で切れたら使用量は分からないので 0）。
    async fn call(&self, req: GenerateRequest) -> Result<Collected, LlmError> {
        let first = stream_with_retry(self.provider.as_ref(), &req, &UNAVAILABLE_BACKOFF).await?;
        let mut stream =
            resume_unsettled(Arc::clone(&self.provider), req, first, MIDSTREAM_RETRIES);
        let mut out = Collected::default();
        while let Some(delta) = stream.next().await {
            match delta? {
                StreamDelta::TextDelta { text } => out.text.push_str(&text),
                StreamDelta::Done { stop_reason, usage } => {
                    out.stop = Some(stop_reason);
                    out.usage = usage;
                }
                _ => {}
            }
        }
        Ok(out)
    }

    /// 1 回分の答え（形に合わなければ 1 回だけ直させる）。使用量は失敗しても返す。
    async fn sample(
        &self,
        base: &GenerateRequest,
        req: &DecideRequest,
    ) -> (Usage, Result<Vec<Reply>, LlmError>) {
        let mut usage = Usage::default();
        let first = match self.call(base.clone()).await {
            Ok(c) => c,
            Err(e) => return (usage, Err(e)),
        };
        add(&mut usage, first.usage);
        if let Some(e) = stop_error(first.stop) {
            return (usage, Err(e));
        }
        let problems = match prompt::parse(req, &first.text) {
            Ok(replies) => return (usage, Ok(replies)),
            Err(p) => p,
        };
        let mut repair = base.clone();
        repair
            .messages
            .push(Message::text(Role::Assistant, first.text));
        repair.messages.push(Message::text(
            Role::User,
            prompt::repair_instruction(&problems),
        ));
        let second = match self.call(repair).await {
            Ok(c) => c,
            Err(e) => return (usage, Err(e)),
        };
        add(&mut usage, second.usage);
        if let Some(e) = stop_error(second.stop) {
            return (usage, Err(e));
        }
        let result = prompt::parse(req, &second.text).map_err(|p| {
            LlmError::BadRequest(format!("{INVALID_OUTPUT_HEAD}（{}）", prompt::summary(&p)))
        });
        (usage, result)
    }
}

#[derive(Default)]
struct Collected {
    text: String,
    stop: Option<StopReason>,
    usage: Usage,
}

/// 直させない停止（拒否・上限での途切れ）。
fn stop_error(stop: Option<StopReason>) -> Option<LlmError> {
    match stop {
        Some(StopReason::Refusal) => Some(LlmError::BadRequest(REFUSED.to_string())),
        Some(StopReason::MaxTokens) => Some(LlmError::BadRequest(TRUNCATED.to_string())),
        _ => None,
    }
}

fn add(total: &mut Usage, u: Usage) {
    total.prompt_tokens = total.prompt_tokens.saturating_add(u.prompt_tokens);
    total.completion_tokens = total.completion_tokens.saturating_add(u.completion_tokens);
    total.cache_read_tokens = total.cache_read_tokens.saturating_add(u.cache_read_tokens);
    total.cache_write_tokens = total
        .cache_write_tokens
        .saturating_add(u.cache_write_tokens);
}

#[async_trait::async_trait]
impl DecisionProvider for LlmDecider {
    fn name(&self) -> &'static str {
        "llm_decider"
    }

    async fn decide(&self, req: &DecideRequest) -> Result<DecideResponse, DecideFailure> {
        let base = self.base_request(req);
        // 多数決の回は並べて呼ぶ（既定の温度でそれぞれ独立に答える）。
        let runs =
            futures::future::join_all((0..self.samples).map(|_| self.sample(&base, req))).await;
        let mut usage = Usage::default();
        let mut replies = Vec::with_capacity(runs.len());
        let mut failure = None;
        for (u, result) in runs {
            add(&mut usage, u);
            match result {
                Ok(r) => replies.push(r),
                Err(e) => {
                    failure.get_or_insert(e);
                }
            }
        }
        if let Some(error) = failure {
            return Err(DecideFailure::new(error, usage));
        }
        let raised = raised_below_uniform(req, &replies);
        if !raised.is_empty() {
            tracing::info!(
                target: LOW_SELF_REPORT_TARGET,
                resolved_model = %self.resolved,
                raised = raised.len(),
                questions = ?raised,
                "自己申告の確からしさが一様より低い答えを一様まで引き上げた"
            );
        }
        Ok(DecideResponse {
            resolved_model: self.resolved.clone(),
            answers: aggregate(req, &replies),
            usage,
            calibrated: false,
            method: if self.samples == 1 {
                DecisionMethod::Verbalized
            } else {
                DecisionMethod::Vote
            },
            samples: self.samples,
        })
    }
}

#[cfg(test)]
mod tests;

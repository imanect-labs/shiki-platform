//! 判定を実際の `LlmGateway::decide` に流す（並べて呼び、止める規則を当てる）。
//!
//! 止める規則（付録 B §6）:
//! - 実行中の累計（中継の累計額の差分）が 1 回の起動の上限を超えたら、新しい呼び出しを始めない。
//! - 429/5xx（`RateLimited`・`Unavailable`）が 3 回続いたら止める。設定の誤り（`Config`）は 1 回で止める。

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::time::Instant;

use authz::AuthContext;
use futures::StreamExt;
use llm_gateway::{
    decide_error_kind, Answer, DecideCall, DecideErrorKind, DecideRequest, LlmError, LlmGateway,
    UsageSource,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::data::Dataset;
use crate::setup::relay_spend;

/// 質問 1 件への答え（値は choice・score なら名前、boolean なら真偽値）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct AnswerOut {
    pub(crate) value: Value,
    pub(crate) confidence: f64,
}

/// 1 回の判定の結果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CallResult {
    pub(crate) dataset: String,
    pub(crate) id: String,
    /// 反復の番号（1 始まり）。
    pub(crate) rep: u32,
    pub(crate) latency_ms: u64,
    /// 誤りの種類（`refused`・`truncated`・`invalid_output`・`invalid_response`・`rate_limited`・
    /// `unavailable`・`config`・`bad_request`・`internal`・`invalid_request`）。
    pub(crate) error: Option<String>,
    #[serde(default)]
    pub(crate) resolved_model: Option<String>,
    #[serde(default)]
    pub(crate) answers: BTreeMap<String, AnswerOut>,
}

impl CallResult {
    pub(crate) fn answer(&self, key: &str) -> Option<&AnswerOut> {
        self.answers.get(key)
    }
}

/// 誤りの種類の名前。
pub(crate) fn error_kind(e: &LlmError) -> &'static str {
    match decide_error_kind(e) {
        Some(DecideErrorKind::Refused) => "refused",
        Some(DecideErrorKind::Truncated) => "truncated",
        Some(DecideErrorKind::InvalidOutput) => "invalid_output",
        Some(DecideErrorKind::InvalidResponse) => "invalid_response",
        Some(DecideErrorKind::InvalidRequest) => "invalid_request",
        None => match e {
            LlmError::RateLimited(_) => "rate_limited",
            LlmError::Unavailable(_) => "unavailable",
            LlmError::Config(_) => "config",
            LlmError::BadRequest(_) => "bad_request",
            LlmError::Internal(_) => "internal",
        },
    }
}

fn answer_out(ds: &Dataset, key: &str, a: &Answer) -> AnswerOut {
    match a {
        Answer::Choice {
            value, confidence, ..
        } => AnswerOut {
            value: Value::String(value.clone()),
            confidence: *confidence,
        },
        Answer::Score {
            level_index,
            confidence,
            ..
        } => AnswerOut {
            value: ds
                .levels(key)
                .get(*level_index)
                .map_or(Value::Null, |l| Value::String(l.clone())),
            confidence: *confidence,
        },
        Answer::Boolean {
            value, confidence, ..
        } => AnswerOut {
            value: Value::Bool(*value),
            confidence: *confidence,
        },
    }
}

/// 1 回の判定の仕事。
pub(crate) struct Job<'a> {
    pub(crate) ds: &'a Dataset,
    pub(crate) row: usize,
    pub(crate) rep: u32,
}

/// 実行の設定。
pub(crate) struct Runner<'a> {
    pub(crate) gw: &'a LlmGateway,
    pub(crate) ctx: &'a AuthContext,
    pub(crate) http: &'a reqwest::Client,
    pub(crate) relay: &'a str,
    pub(crate) model: &'a str,
    /// 会計の冪等キーの頭（`eval:decision:{run_id}:`）。
    pub(crate) key_prefix: &'a str,
    pub(crate) concurrency: usize,
    pub(crate) budget_usd: f64,
    pub(crate) relay_before_usd: f64,
}

/// 流した結果と、途中で止めた理由。
pub(crate) struct Outcome {
    pub(crate) calls: Vec<CallResult>,
    pub(crate) stopped: Option<String>,
}

impl Runner<'_> {
    async fn one(&self, job: &Job<'_>) -> CallResult {
        let row = &job.ds.rows[job.row];
        let req = DecideRequest {
            model: self.model.to_string(),
            state: row.state.clone(),
            questions: job.ds.questions.clone(),
        };
        let call = DecideCall {
            idempotency_key: format!("{}{}:{}:r{}", self.key_prefix, job.ds.name, row.id, job.rep),
            trace_id: None,
            source: UsageSource::Other,
            app_id: None,
        };
        let started = Instant::now();
        let got = self.gw.decide(self.ctx, &req, &call).await;
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let mut out = CallResult {
            dataset: job.ds.name.to_string(),
            id: row.id.clone(),
            rep: job.rep,
            latency_ms,
            error: None,
            resolved_model: None,
            answers: BTreeMap::new(),
        };
        match got {
            Ok(resp) => {
                out.resolved_model = Some(resp.resolved_model.clone());
                out.answers = resp
                    .answers
                    .iter()
                    .map(|(k, a)| (k.clone(), answer_out(job.ds, k, a)))
                    .collect();
            }
            Err(e) => {
                let kind = error_kind(&e);
                eprintln!("  {} {} r{}: {kind}: {e}", job.ds.name, row.id, job.rep);
                out.error = Some(kind.to_string());
            }
        }
        out
    }

    /// 仕事を並べて流す（止めたら新しい呼び出しを始めない。走っている分は待って記録する）。
    pub(crate) async fn run(&self, jobs: Vec<Job<'_>>) -> Outcome {
        let total = jobs.len();
        let stop = Cell::new(false);
        let stopped: RefCell<Option<String>> = RefCell::new(None);
        let mut calls = Vec::with_capacity(total);
        let mut transient_in_a_row = 0usize;
        let stop_ref = &stop;
        let mut stream = futures::stream::iter(jobs)
            .map(|job| async move {
                if stop_ref.get() {
                    return None;
                }
                Some(self.one(&job).await)
            })
            .buffer_unordered(self.concurrency.max(1));
        while let Some(done) = stream.next().await {
            let Some(result) = done else { continue };
            match result.error.as_deref() {
                Some("rate_limited" | "unavailable") => transient_in_a_row += 1,
                Some("config") => {
                    stop.set(true);
                    stopped
                        .borrow_mut()
                        .get_or_insert_with(|| "設定の誤り（config）で止めた".into());
                }
                _ => transient_in_a_row = 0,
            }
            if transient_in_a_row >= 3 {
                stop.set(true);
                stopped
                    .borrow_mut()
                    .get_or_insert_with(|| "429/5xx が 3 回続いたので止めた".into());
            }
            calls.push(result);
            let spent = relay_spend(self.http, self.relay).await.total_usd - self.relay_before_usd;
            if spent > self.budget_usd {
                stop.set(true);
                stopped.borrow_mut().get_or_insert_with(|| {
                    format!(
                        "累計 ${spent:.4} が 1 回の起動の上限 ${:.2} を超えたので止めた",
                        self.budget_usd
                    )
                });
            }
            if calls.len() % 10 == 0 || calls.len() == total {
                eprintln!("  {}/{total} 件（中継の差分 ${spent:.4}）", calls.len());
            }
        }
        drop(stream);
        Outcome {
            calls,
            stopped: stopped.into_inner(),
        }
    }
}

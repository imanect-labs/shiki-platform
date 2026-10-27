//! llm_decider の試験（台本どおりに答える偽のアダプタで、要求の形・直させる経路・失敗の会計・多数決を見る）。

use std::collections::VecDeque;
use std::sync::Mutex;

use futures::stream;
use serde_json::json;

use super::super::llm_decider_prompt::tests::request;
use super::*;
use crate::decision::{decide_error_kind, Answer, DecideErrorKind};
use crate::model::Block;
use crate::provider::DeltaStream;

type Reply = Result<(String, StopReason, Usage), LlmError>;

/// 台本の応答を順に返し、受けた要求を記録するアダプタ。
#[derive(Default)]
struct Scripted {
    replies: Mutex<VecDeque<Reply>>,
    seen: Mutex<Vec<GenerateRequest>>,
}

impl Scripted {
    fn new(replies: Vec<Reply>) -> Arc<Self> {
        Arc::new(Scripted {
            replies: Mutex::new(replies.into()),
            seen: Mutex::new(Vec::new()),
        })
    }

    fn seen(&self) -> Vec<GenerateRequest> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl LlmProvider for Scripted {
    fn name(&self) -> &'static str {
        "scripted"
    }

    async fn stream(&self, req: &GenerateRequest) -> Result<DeltaStream, LlmError> {
        self.seen.lock().unwrap().push(req.clone());
        let next = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("台本が尽きた");
        let (text, stop_reason, usage) = next?;
        Ok(stream::iter(vec![
            Ok(StreamDelta::TextDelta { text }),
            Ok(StreamDelta::Done { stop_reason, usage }),
        ])
        .boxed())
    }
}

fn usage(prompt: u64, completion: u64) -> Usage {
    Usage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        ..Default::default()
    }
}

fn ok(text: &str) -> Reply {
    Ok((text.to_string(), StopReason::EndTurn, usage(100, 20)))
}

const GOOD: &str = r#"{"01_category":{"answer":"billing","confidence":90},"02_priority":{"answer":"中","confidence":70},"03_angry":{"answer":false,"confidence":80}}"#;
const BAD: &str = r#"{"01_category":{"answer":"refund","confidence":90},"02_priority":{"answer":"中","confidence":70},"03_angry":{"answer":false,"confidence":80}}"#;

fn decider(p: &Arc<Scripted>, mode: StructuredOutputMode, tuning: DecisionTuning) -> LlmDecider {
    LlmDecider::new(
        Arc::clone(p) as Arc<dyn LlmProvider>,
        "claude-haiku-5-5",
        true,
        mode,
        tuning,
    )
}

fn k(samples: u8) -> DecisionTuning {
    DecisionTuning {
        samples,
        ..Default::default()
    }
}

fn texts(g: &GenerateRequest) -> Vec<(Role, String)> {
    g.messages
        .iter()
        .map(|m| {
            let t = m
                .content
                .iter()
                .filter_map(|b| match b {
                    Block::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>();
            (m.role, t)
        })
        .collect()
}

#[tokio::test]
async fn the_request_uses_a_closed_schema_low_effort_and_no_temperature() {
    let p = Scripted::new(vec![ok(GOOD)]);
    let d = decider(&p, StructuredOutputMode::JsonSchema, k(1));
    let got = d.decide(&request()).await.unwrap();
    let seen = p.seen();
    assert_eq!(seen.len(), 1);
    let g = &seen[0];
    assert_eq!(g.model.as_deref(), Some("claude-haiku-5-5"));
    assert_eq!(g.system.as_deref(), Some(prompt::SYSTEM));
    assert_eq!(g.effort, Some(Effort::Low));
    assert_eq!(g.max_tokens, Some(2048));
    assert_eq!(g.temperature, None, "温度は送らない");
    let Some(ResponseFormat::JsonSchema { name, schema }) = &g.response_format else {
        panic!("{:?}", g.response_format)
    };
    assert_eq!(name, "decision");
    assert_eq!(
        schema["required"],
        json!(["01_category", "02_priority", "03_angry"])
    );
    // messages は user だけで終わる（prefill にしない）。
    let t = texts(g);
    assert_eq!(t.len(), 1);
    assert_eq!(t[0].0, Role::User);
    assert!(t[0].1.starts_with("<state>"));

    assert_eq!(got.resolved_model, "claude-haiku-5-5+llm_decider/v3/k1/low");
    assert_eq!(got.method, DecisionMethod::Verbalized);
    assert_eq!(got.samples, 1);
    assert!(!got.calibrated);
    assert_eq!(got.usage, usage(100, 20));
    assert!(
        matches!(&got.answers["category"], Answer::Choice { value, confidence, .. } if value == "billing" && (*confidence - 0.9).abs() < 1e-9)
    );
    assert!(matches!(
        got.answers["priority"],
        Answer::Score { level_index: 1, .. }
    ));
    assert!(matches!(
        got.answers["angry"],
        Answer::Boolean { value: false, .. }
    ));
}

#[tokio::test]
async fn prompt_mode_moves_the_schema_into_system_and_effort_follows_the_model() {
    let p = Scripted::new(vec![Ok((
        format!("```json\n{GOOD}\n```"),
        StopReason::EndTurn,
        usage(1, 1),
    ))]);
    let d = LlmDecider::new(
        Arc::clone(&p) as Arc<dyn LlmProvider>,
        "local-model",
        false,
        StructuredOutputMode::Prompt,
        DecisionTuning {
            samples: 1,
            effort: Effort::Medium,
        },
    );
    let got = d.decide(&request()).await.unwrap();
    let g = &p.seen()[0];
    assert!(g.response_format.is_none());
    assert!(g.system.as_deref().unwrap().contains("<json_schema>"));
    // 思考の深さに対応しないモデルには送らない（版にもそう残す）。深い設定の上限は 4096。
    assert_eq!(g.effort, None);
    assert_eq!(g.max_tokens, Some(4096));
    assert_eq!(got.resolved_model, "local-model+llm_decider/v3/k1/none");
}

#[tokio::test]
async fn an_answer_out_of_range_is_repaired_once() {
    let p = Scripted::new(vec![ok(BAD), ok(GOOD)]);
    let got = decider(&p, StructuredOutputMode::JsonSchema, k(1))
        .decide(&request())
        .await
        .unwrap();
    let seen = p.seen();
    assert_eq!(seen.len(), 2);
    let t = texts(&seen[1]);
    assert_eq!(
        t.iter().map(|(r, _)| *r).collect::<Vec<_>>(),
        [Role::User, Role::Assistant, Role::User]
    );
    assert_eq!(t[1].1, BAD);
    assert!(t[2].1.contains("category"), "{}", t[2].1);
    // 直させる依頼（user の turn）に、モデルが出した値を写さない（PIT-56）。
    assert!(!t[2].1.contains("refund"), "{}", t[2].1);
    // 直させた分も使用量に数える。
    assert_eq!(got.usage, usage(200, 40));
}

#[tokio::test]
async fn an_answer_still_out_of_range_after_repair_fails_with_its_usage() {
    let p = Scripted::new(vec![ok(BAD), ok(BAD)]);
    let f = decider(&p, StructuredOutputMode::JsonSchema, k(1))
        .decide(&request())
        .await
        .unwrap_err();
    assert_eq!(
        decide_error_kind(&f.error),
        Some(DecideErrorKind::InvalidOutput)
    );
    // 利用者に出る要約にも、モデルが出した値を載せない。
    assert!(!f.error.to_string().contains("refund"), "{}", f.error);
    assert_eq!(f.usage, usage(200, 40));
    assert_eq!(p.seen().len(), 2, "直させるのは 1 回だけ");
}

#[tokio::test]
async fn refusals_and_truncation_are_not_repaired() {
    for (stop, kind) in [
        (StopReason::Refusal, DecideErrorKind::Refused),
        (StopReason::MaxTokens, DecideErrorKind::Truncated),
    ] {
        let p = Scripted::new(vec![Ok(("{\"".into(), stop, usage(50, 7)))]);
        let f = decider(&p, StructuredOutputMode::JsonSchema, k(1))
            .decide(&request())
            .await
            .unwrap_err();
        assert_eq!(decide_error_kind(&f.error), Some(kind));
        assert_eq!(f.usage, usage(50, 7), "失敗しても使った分を返す");
        assert_eq!(p.seen().len(), 1);
    }
}

#[tokio::test]
async fn majority_vote_calls_k_times_and_sums_usage() {
    let other = r#"{"01_category":{"answer":"bug","confidence":60},"02_priority":{"answer":"高","confidence":70},"03_angry":{"answer":true,"confidence":80}}"#;
    let p = Scripted::new(vec![ok(GOOD), ok(other), ok(GOOD), ok(other), ok(GOOD)]);
    let got = decider(&p, StructuredOutputMode::JsonSchema, k(5))
        .decide(&request())
        .await
        .unwrap();
    assert_eq!(p.seen().len(), 5);
    assert_eq!(got.method, DecisionMethod::Vote);
    assert_eq!(got.samples, 5);
    assert_eq!(got.resolved_model, "claude-haiku-5-5+llm_decider/v3/k5/low");
    assert_eq!(got.usage, usage(500, 100));
    assert!(
        matches!(&got.answers["category"], Answer::Choice { value, confidence, .. } if value == "billing" && (*confidence - 0.6).abs() < 1e-9)
    );
    assert!(
        matches!(got.answers["angry"], Answer::Boolean { value: false, probability, .. } if (probability - 0.4).abs() < 1e-9)
    );
}

#[tokio::test]
async fn one_failed_vote_fails_the_decision_with_everything_used() {
    let p = Scripted::new(vec![
        ok(GOOD),
        Err(LlmError::BadRequest("prompt is too long".into())),
        ok(GOOD),
        ok(BAD),
        ok(BAD),
        ok(GOOD),
    ]);
    let f = decider(&p, StructuredOutputMode::JsonSchema, k(5))
        .decide(&request())
        .await
        .unwrap_err();
    // 5 回のうち 1 回はプロバイダの誤り（使用量なし）、1 回は直しても合わない（2 回分）。
    assert!(matches!(&f.error, LlmError::BadRequest(m) if m == "prompt is too long"));
    assert_eq!(f.usage, usage(500, 100));
}

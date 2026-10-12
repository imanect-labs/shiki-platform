//! 記録済み軌跡でコンテキスト剪定を再生する（#532）。
//!
//! 実 LLM を呼ばずに、実 run のステップ列（assistant 応答＋ツール結果）を、自律プロファイルの
//! 本番ループが各ステップ前に呼ぶのと同じ [`ContextLimits::apply`] に通し、ステップごとの prompt
//! 見積り・溢れ・畳み込み量を測る。
//! 閾値（設定値・窓比・`keep_recent`）を実データで比べるための計器（arXiv:2609.20804 の
//! 管理方式 × 窓予算の比較を、費用ゼロ・決定的に行う）。
//!
//! 限界（レポートを読む側が知っておくこと）:
//! - 剪定後にモデルが**別の行動をとる**（畳まれた出力を読み直す等）効果は測れない。軌跡は記録時の
//!   ままなので、情報損失がタスク成否に与える影響は実 LLM の評価でしか分からない。
//! - system プロンプト・ツール定義の分は固定の `overhead_tokens` で与える（本番ループは実測で
//!   補正するが、再生にはプロバイダが無い）。
//! - **自律プロファイルの run を前提にする**。Chat プロファイルは本番で剪定しないが、記録から
//!   区別できないため、Chat の run を渡すと本番で起きない畳み込みが報告される。
//! - 記録に残らない入力は見積りに入らない: 残予算の観測文（自律 run の各ステップ・数十トークン）、
//!   空応答への催促と、その空応答に使った 1 ステップ、予算着地の指示文。予算着地のステップは本番
//!   では剪定せずに送るが、通常の最終ステップと区別できないので剪定して見積もる（過小側）。

use llm_gateway::{Block, Message};
use serde::Serialize;

use crate::context::{estimate_prompt_tokens, is_folded, ContextGauge, ContextLimits};

/// 1 ステップ（1 回の LLM 応答と、それに続くツール結果）。
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayStep {
    /// assistant 応答（本文＋tool_use）。
    pub assistant: Message,
    /// ツール結果（最終応答のステップでは `None`）。
    pub observation: Option<Message>,
}

/// 再生条件（本番の `AgentOptions` の剪定関連とモデルの窓に対応する）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ReplayConfig {
    /// 剪定の設定値（`context_soft_limit_tokens`・0 は管理なし＝ベースライン）。
    pub soft_limit_tokens: usize,
    pub keep_recent: usize,
    /// モデルのコンテキスト窓（`None` は窓を知らない＝溢れを判定しない）。
    pub window_tokens: Option<u32>,
    /// 1 応答の出力上限（有効窓と溢れ判定に使う）。
    pub max_output_tokens: Option<u32>,
    /// system プロンプト・ツール定義ぶんのトークン（固定）。
    pub overhead_tokens: usize,
}

/// 再生結果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReplayReport {
    pub config: ReplayConfig,
    pub steps: usize,
    /// 各ステップで送る prompt の見積りの最大値。
    pub peak_prompt_tokens: usize,
    /// 各ステップの prompt 見積りの合計（毎ステップ履歴を再送するため、入力課金の代理指標）。
    pub total_prompt_tokens: u64,
    /// prompt ＋ 出力上限が窓を超えたステップ数（窓を知らなければ 0）。
    pub overflow_steps: usize,
    /// 最初に溢れたステップ（0 始まり）。本番ならここでプロバイダが拒否して run が失敗する。
    pub first_overflow_step: Option<usize>,
    /// 最終状態で畳まれていたツール結果の件数。
    pub folded_results: usize,
    /// 畳み込みで削ったツール結果のバイト数の合計。
    pub folded_bytes: usize,
}

/// 軌跡を再生する。各ステップの直前に本番ループと同じ順序で剪定し、送る prompt を見積もる。
#[must_use]
pub fn replay(initial: &[Message], steps: &[ReplayStep], config: ReplayConfig) -> ReplayReport {
    let limits = ContextLimits::resolve(
        config.soft_limit_tokens,
        config.keep_recent,
        config.window_tokens,
        config.max_output_tokens,
    );
    let gauge = ContextGauge::with_overhead(config.overhead_tokens);
    let reserve = config.max_output_tokens.unwrap_or(0) as usize;
    let mut messages = initial.to_vec();
    let mut report = ReplayReport {
        config,
        steps: steps.len(),
        peak_prompt_tokens: 0,
        total_prompt_tokens: 0,
        overflow_steps: 0,
        first_overflow_step: None,
        folded_results: 0,
        folded_bytes: 0,
    };
    for (i, step) in steps.iter().enumerate() {
        let before = result_bytes(&messages);
        limits.apply(&mut messages, &gauge);
        report.folded_bytes += before.saturating_sub(result_bytes(&messages));

        let prompt = estimate_prompt_tokens(&messages).saturating_add(config.overhead_tokens);
        report.peak_prompt_tokens = report.peak_prompt_tokens.max(prompt);
        report.total_prompt_tokens = report.total_prompt_tokens.saturating_add(prompt as u64);
        if config
            .window_tokens
            .is_some_and(|w| prompt.saturating_add(reserve) > w as usize)
        {
            report.overflow_steps += 1;
            report.first_overflow_step.get_or_insert(i);
        }

        messages.push(step.assistant.clone());
        if let Some(obs) = &step.observation {
            messages.push(obs.clone());
        }
    }
    report.folded_results = tool_results(&messages).filter(|c| is_folded(c)).count();
    report
}

fn tool_results(messages: &[Message]) -> impl Iterator<Item = &str> {
    messages
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|b| match b {
            Block::ToolResult { content, .. } => Some(content.as_str()),
            _ => None,
        })
}

fn result_bytes(messages: &[Message]) -> usize {
    tool_results(messages).map(str::len).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use llm_gateway::Role;

    /// ツールを 1 回呼び、`result_bytes` バイトの結果を得るステップ。
    fn tool_step(i: usize, result_bytes: usize) -> ReplayStep {
        let id = format!("c{i}");
        ReplayStep {
            assistant: Message {
                role: Role::Assistant,
                content: vec![Block::ToolUse {
                    id: id.clone(),
                    name: "web_fetch".into(),
                    input: serde_json::json!({}),
                }],
            },
            observation: Some(Message {
                role: Role::Tool,
                content: vec![Block::ToolResult {
                    tool_use_id: id,
                    content: "w".repeat(result_bytes),
                    is_error: false,
                }],
            }),
        }
    }

    fn trajectory(n: usize, result_bytes: usize) -> Vec<ReplayStep> {
        let mut steps: Vec<ReplayStep> = (0..n).map(|i| tool_step(i, result_bytes)).collect();
        steps.push(ReplayStep {
            assistant: Message::text(Role::Assistant, "まとめ"),
            observation: None,
        });
        steps
    }

    fn config(soft: usize, window: Option<u32>) -> ReplayConfig {
        ReplayConfig {
            soft_limit_tokens: soft,
            keep_recent: 6,
            window_tokens: window,
            max_output_tokens: Some(4_096),
            overhead_tokens: 2_000,
        }
    }

    #[test]
    fn baseline_overflows_where_managed_run_does_not() {
        // 10 回 × 16KB（≒ 4k トークン）の取得。32k 窓では管理なしだと途中で溢れる。
        let initial = vec![Message::text(Role::User, "調べて")];
        let steps = trajectory(10, 16_000);

        let baseline = replay(&initial, &steps, config(0, Some(32_000)));
        assert_eq!(baseline.steps, 11);
        assert!(baseline.overflow_steps > 0);
        // prompt ≒ 2k + 4k×i。出力上限 4096 を足して 32k を超えるのは i = 7 から。
        assert_eq!(baseline.first_overflow_step, Some(7));
        assert_eq!(baseline.folded_results, 0);

        let managed = replay(&initial, &steps, config(24_000, Some(32_000)));
        assert_eq!(managed.overflow_steps, 0, "{managed:?}");
        assert!(managed.folded_results > 0 && managed.folded_bytes > 0);
        assert!(managed.peak_prompt_tokens < baseline.peak_prompt_tokens);
        assert!(managed.total_prompt_tokens < baseline.total_prompt_tokens);
    }

    #[test]
    fn unknown_window_reports_no_overflow() {
        let initial = vec![Message::text(Role::User, "調べて")];
        let r = replay(&initial, &trajectory(10, 16_000), config(0, None));
        assert_eq!((r.overflow_steps, r.first_overflow_step), (0, None));
        assert!(r.peak_prompt_tokens > 32_000);
    }

    #[test]
    fn empty_trajectory_is_all_zero() {
        let r = replay(&[], &[], config(24_000, Some(32_000)));
        assert_eq!(r.steps, 0);
        assert_eq!(r.total_prompt_tokens, 0);
        assert_eq!(r.first_overflow_step, None);
    }

    #[test]
    fn report_serializes_with_its_config() {
        let r = replay(&[], &[], config(24_000, Some(32_000)));
        let v = serde_json::to_value(&r).unwrap_or_default();
        assert_eq!(v["config"]["window_tokens"], 32_000);
        assert_eq!(v["overflow_steps"], 0);
    }
}

//! agent.invoke の本体（agent-core のツールループにつなぐ・BUG-6・D-4・D-22・D-29）。
//!
//! エンジンは agent-core・llm-gateway に依存しない（トレイト境界）。ループは api のポート
//! （[`NodePorts::agent_invoke`](super::ports::NodePorts::agent_invoke)）が回し、道具の呼び出しは
//! [`ExecutorAgentGateway`] でエンジンへ戻す（権限の判断はエンジンとチョークポイントに残す）。
//!
//! - 提示する道具 = `allowed_tools`（省略は道具なし）∩ 今のステージで選べる ∩ scope 天井。web.fetch は
//!   読んでよいサイト（`egress_allowlist ∩ web の天井`）が 1 つ以上あるときだけ。提示しなかったものは
//!   出力の `skipped_tools` に出す（黙って弱くしない）。
//! - 上限（手順・合計トークン・時間・費用）はデプロイ設定を天井に、ノード設定で縮めるだけ。
//! - 同時実行はプロセス内の枠で抑える（長く走る step がワーカーを占有して全テナントの step を止めない）。
//!   取れなければ `concurrency_limited`（attempt を消費しない順番待ち・`retry.rs`）。

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::ir::params::AgentInvokeParams;
use crate::ir::validate::pattern_within;
use crate::run::NodeContext;
use crate::vocab::{
    AgentTool, AGENT_DURATION_RANGE_SEC, DEFAULT_AGENT_DURATION_SEC, DEFAULT_AGENT_STEPS,
    MAX_AGENT_STEPS, MAX_CONTEXT_BYTES, MAX_INSTRUCTION_BYTES,
};

use super::agent_gateway::ExecutorAgentGateway;
use super::capability::parse_params;
use super::exec::CapabilityNodeExecutor;
use super::ports::{AgentInvokeReq, ExecCtx, PortError};
use super::resolver::{as_string, as_u32, opt_param, param, typed, ParamResolver};

/// agent.invoke の上限と天井（デプロイ設定 `workflow.agent.*`・ノード設定は縮めるだけ）。
#[derive(Debug, Clone)]
pub struct AgentLimits {
    /// プロセス内で同時に走らせる agent.invoke の数（1 以上）。
    pub max_concurrent: usize,
    /// ノードで手順の上限を省略したときの値（[`MAX_AGENT_STEPS`] 以下に丸める）。
    pub default_max_steps: u32,
    /// 1 回の実行の合計トークンの天井。
    pub max_total_tokens: u64,
    /// 1 回の実行の費用の天井（マイクロ USD）。
    pub max_cost_usd_micros: i64,
    /// web.fetch が読んでよいサイトの天井（管理者が決める・空なら web.fetch は提示しない・D-29）。
    pub web_ceiling: Vec<String>,
}

impl Default for AgentLimits {
    fn default() -> Self {
        AgentLimits {
            max_concurrent: 2,
            default_max_steps: DEFAULT_AGENT_STEPS,
            max_total_tokens: 200_000,
            max_cost_usd_micros: 500_000,
            web_ceiling: Vec::new(),
        }
    }
}

impl CapabilityNodeExecutor {
    pub(super) async fn node_agent_invoke(
        &self,
        params: &Value,
        ctx: &NodeContext,
        ec: &ExecCtx,
        r: &ParamResolver<'_>,
    ) -> Result<Value, PortError> {
        // 枠は step の終わりまで持つ（drop で返る）。レートの枠より先に取る（順番待ちでレートを消費しない）。
        let Ok(_slot) = Arc::clone(&self.agent_slots).try_acquire_owned() else {
            return Err(PortError::new(
                "concurrency_limited",
                "AI エージェントの同時実行の上限に達しました（順番待ち）",
                true,
            ));
        };
        self.rate_check(ec, "agent.invoke").await?;
        let p: AgentInvokeParams = parse_params(params)?;
        let req = self.agent_request(&p, ctx, r)?;
        let (offered, skipped) = self.offer_tools(&p, ctx, ec);
        let scopes: BTreeSet<&str> = offered
            .iter()
            .map(|t| t.required_scope().as_str())
            .collect();
        let ceiling = ctx
            .scope_ceiling
            .iter()
            .filter(|s| scopes.contains(s.as_str()))
            .cloned()
            .collect();
        let gateway = ExecutorAgentGateway {
            ports: Arc::clone(&self.ports),
            audit: Arc::clone(&self.audit),
            ec: ec.clone(),
            offered: offered.clone(),
            ceiling,
            web_hosts: self.web_hosts(&p),
            cancel: ctx.cancel.clone(),
        };
        let requested_model = req.model.clone();
        let mut out = self.ports.agent_invoke(ec, req, Arc::new(gateway)).await?;
        // エンジンが提示しなかった道具も出力に足す（ポートが未配線で外した分と合わせる）。
        let mut all_skipped: Vec<String> = skipped.iter().map(|t| t.as_str().to_string()).collect();
        if let Some(Value::Array(port_skipped)) = out.get("skipped_tools") {
            for s in port_skipped.iter().filter_map(Value::as_str) {
                if !all_skipped.iter().any(|x| x == s) {
                    all_skipped.push(s.to_string());
                }
            }
        }
        if let Some(obj) = out.as_object_mut() {
            obj.insert("skipped_tools".into(), json!(all_skipped));
        }
        // 依頼の本文は監査に載せない（道具・停止理由・消費だけ）。
        self.audit(
            &ec.tenant_id,
            "agent.invoke",
            true,
            &json!({
                "model": out.get("model"),
                "requested_model": requested_model,
                "tools": offered.iter().map(|t| t.as_str()).collect::<Vec<_>>(),
                "skipped_tools": all_skipped,
                "stop": out.get("stop"),
                "steps": out.pointer("/usage/steps"),
                "tool_calls": out.get("tool_calls").and_then(Value::as_array).map(Vec::len),
            }),
        );
        Ok(out)
    }

    /// params とデプロイ設定から要求を組む（上限は天井へ丸める）。
    fn agent_request(
        &self,
        p: &AgentInvokeParams,
        ctx: &NodeContext,
        r: &ParamResolver<'_>,
    ) -> Result<AgentInvokeReq, PortError> {
        let what = "agent.invoke: instruction";
        let instruction = typed(&param(&p.instruction, r, what)?, what, "文字列", as_string)?;
        if instruction.trim().is_empty() {
            return Err(PortError::invalid(
                "エージェントへの依頼が空です。ノードの設定で依頼を書いてください",
            ));
        }
        if instruction.len() > MAX_INSTRUCTION_BYTES {
            return Err(PortError::invalid(format!(
                "エージェントへの依頼が長すぎます（{} KB まで）",
                MAX_INSTRUCTION_BYTES / 1024
            )));
        }
        let context = opt_param(p.context.as_ref(), r, "agent.invoke: context")?;
        if context
            .as_ref()
            .is_some_and(|c| c.to_string().len() > MAX_CONTEXT_BYTES)
        {
            return Err(PortError::invalid(format!(
                "参考データが大きすぎます（{} KB まで）。必要な項目だけを渡してください",
                MAX_CONTEXT_BYTES / 1024
            )));
        }
        let what = "agent.invoke: max_duration_sec";
        let (min_sec, max_sec) = AGENT_DURATION_RANGE_SEC;
        let duration_sec = opt_param(p.max_duration_sec.as_ref(), r, what)?
            .map(|v| typed(&v, what, "0 以上の整数", as_u32))
            .transpose()?
            .unwrap_or(DEFAULT_AGENT_DURATION_SEC)
            .clamp(min_sec, max_sec);
        let lim = &self.agent_limits;
        let default_steps = lim.default_max_steps.clamp(1, MAX_AGENT_STEPS);
        let max_total_tokens = p
            .max_tokens
            .map_or(lim.max_total_tokens, |t| t.min(lim.max_total_tokens))
            .max(1);
        Ok(AgentInvokeReq {
            instruction,
            context,
            // 空文字は省略と同じ（組織の既定・保存時検証と揃える）。
            model: p.model.clone().filter(|m| !m.is_empty()),
            max_steps: p
                .max_steps
                .unwrap_or(default_steps)
                .clamp(1, MAX_AGENT_STEPS),
            max_total_tokens,
            max_cost_usd_micros: lim.max_cost_usd_micros,
            max_duration_ms: u64::from(duration_sec) * 1000,
            idempotency_key: ctx.idempotency_key.clone(),
            attempt: ctx.attempt,
        })
    }

    /// web.fetch が読んでよいサイト（ノードの指定のうち web の天井に収まるもの・縮小のみ）。
    fn web_hosts(&self, p: &AgentInvokeParams) -> Vec<String> {
        p.egress_allowlist
            .iter()
            .filter(|h| pattern_within(h, &self.agent_limits.web_ceiling))
            .cloned()
            .collect()
    }

    /// 提示する道具と、指定されたが提示しない道具（予約・scope 天井の外・web の天井が空）。
    fn offer_tools(
        &self,
        p: &AgentInvokeParams,
        ctx: &NodeContext,
        ec: &ExecCtx,
    ) -> (Vec<AgentTool>, Vec<AgentTool>) {
        let has_web_hosts = !self.web_hosts(p).is_empty();
        let mut offered: Vec<AgentTool> = Vec::new();
        let mut skipped: Vec<AgentTool> = Vec::new();
        for t in p.allowed_tools.iter().flatten() {
            if offered.contains(t) || skipped.contains(t) {
                continue;
            }
            let in_ceiling = ctx
                .scope_ceiling
                .iter()
                .any(|s| s == t.required_scope().as_str());
            if !in_ceiling {
                // 保存時に宣言は補われるので、ここに来るのは宣言を外した IR だけ（監査に残す）。
                self.audit.record_out_of_scope(
                    ec,
                    t.as_str(),
                    &json!({ "reason": "out_of_scope", "via": "agent", "step": ctx.step_path }),
                );
            }
            let ok = t.available() && in_ceiling && (*t != AgentTool::WebFetch || has_web_hosts);
            if ok {
                offered.push(*t);
            } else {
                skipped.push(*t);
            }
        }
        (offered, skipped)
    }
}

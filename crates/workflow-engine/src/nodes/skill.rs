//! skill.invoke ノード（ir.md §7.7・#344 Task 10.1b）。
//!
//! `skill:<name>@<version>` を **skill_resolve ポート**（server 側が実行主体 ReBAC で
//! レジストリ→artifact を解決・fail-closed の実行時再検証）で引き、中身に応じて既存経路へ
//! dispatch する（新しい実行機構は作らない・miniapp-platform.md §4）:
//!
//! - `.shiki` script を持つ skill → script-runtime（script.run と同じ engine/HostBridge・
//!   `Shiki.*` は同じ能力ゲートウェイに合流＝scope ceiling が効く）
//! - instructions のみの skill → agent.invoke 経路（サンドボックス・egress 全遮断）

use serde_json::{json, Value};

use crate::control::eval::resolve_value;
use crate::ir::params::SkillInvokeParams;
use crate::ir::validate::parse_skill_ref;
use crate::run::NodeContext;

use super::capability::parse_params;
use super::exec::CapabilityNodeExecutor;
use super::ports::{ExecCtx, LlmInvokeReq, PortError};
use super::resolver::ParamResolver;

impl CapabilityNodeExecutor {
    pub(super) async fn node_skill_invoke(
        &self,
        params: &Value,
        ctx: &NodeContext,
        ec: &ExecCtx,
        r: &ParamResolver<'_>,
    ) -> Result<Value, PortError> {
        let p: SkillInvokeParams = parse_params(params)?;
        let Some((name, version)) = parse_skill_ref(&p.skill) else {
            return Err(PortError::invalid(format!(
                "skill 参照は skill:<name>@<version> 形式です: {}",
                p.skill
            )));
        };

        // 実行時再検証（保存時 OK でもアンインストール/剥奪で消えている・fail-closed）。
        let skill = self.ports.skill_resolve(ec, name, version).await?;

        let input = p
            .input
            .as_ref()
            .and_then(|e| resolve_value(e, r))
            .unwrap_or_else(|| ctx.input.clone());

        let out = if let Some(source) = &skill.shiki_script {
            // `.shiki` script: script.run と同じ engine/HostBridge（Shiki.* も同じゲートウェイ）。
            let engine = self
                .script_engine
                .clone()
                .ok_or_else(|| PortError::unavailable("script engine が未設定です"))?;
            // 実効 scope = workflow ceiling ∩ skill 宣言スコープ（宣言があるときのみ絞る・
            // 広い workflow scope 下でも skill script が宣言外 API を呼べないように・レビュー指摘）。
            let ceiling = skill.allowed_scopes.as_ref().map(|declared| {
                let allow: std::collections::BTreeSet<&str> =
                    declared.iter().map(String::as_str).collect();
                ctx.scope_ceiling
                    .iter()
                    .filter(|s| allow.contains(s.as_str()))
                    .cloned()
                    .collect::<Vec<_>>()
            });
            self.run_script_source(super::script::ScriptRunReq {
                source,
                audit_api: "skill.invoke",
                input,
                ctx,
                ec,
                engine,
                ceiling_override: ceiling,
            })
            .await?
        } else {
            // instructions のみ: llm.invoke 経路（instructions を system・入力を prompt に）。
            // agent_invoke ポートはコードのサンドボックス実行（ExecRequest::Python）であり、
            // 自然言語 instructions を code として渡すと Python として実行されてしまう
            // （レビュー指摘）。エージェンティックなツールループが要るスキルはチャット側の
            // skill ツール（PR #354）が担い、ワークフローの instructions-only skill は
            // 「skill 指示付きの LLM 呼び出し」として実行する。
            self.rate_check(ec, "llm.invoke").await?;
            self.ports
                .llm_invoke(
                    ec,
                    LlmInvokeReq {
                        model: None,
                        system: Some(format!("# Skill: {}\n\n{}", skill.name, skill.instructions)),
                        prompt: input.to_string(),
                        max_tokens: None,
                        idempotency_key: ctx.idempotency_key.clone(),
                    },
                )
                .await?
        };

        self.audit(
            &ec.tenant_id,
            "skill.invoke",
            true,
            &json!({ "skill": p.skill, "via": if skill.shiki_script.is_some() { "script" } else { "llm" } }),
        );
        Ok(out)
    }
}

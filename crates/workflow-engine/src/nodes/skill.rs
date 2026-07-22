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
use super::ports::{AgentInvokeReq, ExecCtx, PortError};
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
            self.run_script_source(source, "skill.invoke", input, ctx, ec, engine)
                .await?
        } else {
            // instructions のみ: agent.invoke 経路（サンドボックス・外部通信なし）。
            self.rate_check(ec, "agent.invoke").await?;
            let code = format!(
                "# Skill: {}\n\n{}\n\n## 入力\n{}",
                skill.name, skill.instructions, input
            );
            self.ports
                .agent_invoke(
                    ec,
                    AgentInvokeReq {
                        code,
                        timeout_ms: None,
                        egress_allowlist: Vec::new(),
                    },
                )
                .await?
        };

        self.audit(
            &ec.tenant_id,
            "skill.invoke",
            true,
            &json!({ "skill": p.skill, "via": if skill.shiki_script.is_some() { "script" } else { "agent" } }),
        );
        Ok(out)
    }
}

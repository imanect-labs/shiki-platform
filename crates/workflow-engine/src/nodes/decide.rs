//! 判定のノード（`ai.decide`・FEAT-1・ir.md §7.11）の実行。
//!
//! 順序: レート制御 → params（V1 の検査をもう一度・保存を通らない経路の IR でも緩めない）→ 判定の材料を解決
//! （文字列・オブジェクト・配列・32 KiB まで）→ ポート（`LlmGateway::decide`・組織の許可・利用上限・会計は
//! チョークポイントの中）→ 応答を型付きで読み、質問と突き合わせる（[`super::decide_reply`]）→ 出口を選ぶ
//! （`AiDecideParams::port_for`・保存時の V2 と同じ規則）→ 監査。
//!
//! - 監査には出口と、振り分けに使った質問の確からしさだけを残す（判定の材料と全部の答えは載せない・H5。
//!   run の step の出力には残る）。
//! - しきい値に届かなければ `uncertain` へ出すだけで、承認待ちを知らない（`uncertain` を human.approval へ
//!   つなぐのは作者・D-7）。較正されていない確からしさでも同じ規則で振り分ける（H4・出力の `calibrated`）。
//! - 失敗は再試行の分類に従う（設定・応答の誤りは permanent・到達障害は retryable）。会計はチョークポイントが
//!   済ませているので、ここでは記録しない。
//! - 決定キャッシュ（step 属性 `cache`・T10）は、書いたノードだけポートへ範囲（このワークフロー）と期限を
//!   渡す。当たったかは出力と監査の `cached` に出す（当たっても許可はチョークポイントが確かめる）。

use serde_json::{json, Value};

use crate::ir::params::{AiDecideParams, MAX_DECIDE_STATE_BYTES};
use crate::run::{NodeContext, NodeResult};

use super::capability::parse_params;
use super::decide_reply;
use super::exec::CapabilityNodeExecutor;
use super::ports::{DecideCache, ExecCtx, LlmDecideReq, PortError};
use super::resolver::{param, ParamResolver};

/// 判定の材料が大きすぎる（permanent・材料を絞る）。
pub const DECISION_STATE_TOO_LARGE: &str = "decision_state_too_large";
/// 判定の応答が質問と合わない（permanent・ポートか決定器の実装・設定の誤り）。
pub const DECISION_INVALID_RESPONSE: &str = "decision_invalid_response";
/// [`DECISION_INVALID_RESPONSE`] の利用者向けの文（原因はサーバのログだけに残す・ポートも同じ文を使う）。
pub const DECISION_INVALID_RESPONSE_MESSAGE: &str =
    "判定モデルの応答が質問と合いませんでした。管理者にお問い合わせください。";

/// 能力 API 名（監査・レート制御・スコープの語彙）。
const API: &str = "ai.decide";

impl CapabilityNodeExecutor {
    /// ai.decide を実行し、選んだ出口で返す。
    pub(super) async fn node_ai_decide(&self, params: &Value, ctx: &NodeContext) -> NodeResult {
        let ec = Self::exec_ctx(ctx);
        match self.decide(params, ctx, &ec).await {
            Ok((output, port)) => NodeResult::ok_port(output, &port),
            Err(e) => NodeResult::fail(&e.code, e.message, e.retryable),
        }
    }

    async fn decide(
        &self,
        params: &Value,
        ctx: &NodeContext,
        ec: &ExecCtx,
    ) -> Result<(Value, String), PortError> {
        self.rate_check(ec, API).await?;
        let p: AiDecideParams = parse_params(params)?;
        p.check_cross_fields().map_err(|issue| {
            PortError::new(
                "bad_params",
                format!("ai.decide: {}: {}", issue.path, issue.message),
                false,
            )
        })?;
        let r = ParamResolver::new(ctx);
        let what = "ai.decide: state";
        let state = param(&p.state, &r, what)?;
        if !(state.is_string() || state.is_object() || state.is_array()) {
            return Err(PortError::invalid(format!(
                "{what} が文字・オブジェクト・配列ではありません"
            )));
        }
        let size = serde_json::to_vec(&state).map_or(usize::MAX, |b| b.len());
        if size > MAX_DECIDE_STATE_BYTES {
            return Err(PortError::new(
                DECISION_STATE_TOO_LARGE,
                format!(
                    "判定の材料が大きすぎます（{} KiB まで）。判定に要る項目だけを渡してください",
                    MAX_DECIDE_STATE_BYTES / 1024
                ),
                false,
            ));
        }
        let raw = self
            .ports
            .llm_decide(
                ec,
                LlmDecideReq {
                    model: p.model.clone(),
                    state,
                    questions: p.questions.clone(),
                    idempotency_key: ctx.idempotency_key.clone(),
                    // 決定キャッシュはノードに cache を書いたときだけ（既定は使わない・H6）。
                    cache: ctx.cache.map(|c| DecideCache {
                        workflow_id: ctx.workflow_id,
                        ttl_sec: c.ttl_sec,
                    }),
                },
            )
            .await?;
        let reply = decide_reply::read(&raw, &p.questions).map_err(|why| {
            tracing::error!(
                target: "workflow.decide",
                tenant = %ec.tenant_id,
                run_id = ?ec.run_id,
                step = ?ec.step_path,
                model = %p.model,
                %why,
                "判定の応答が質問と合わない"
            );
            PortError::new(
                DECISION_INVALID_RESPONSE,
                DECISION_INVALID_RESPONSE_MESSAGE,
                false,
            )
        })?;
        let routed = p
            .route
            .as_ref()
            .and_then(|route| reply.routed(&route.question));
        let port = p.port_for(routed);
        let routed_out = p
            .route
            .as_ref()
            .map(|route| json!({ "question": route.question, "port": port }));
        let output = json!({
            "answers": reply.answers_output(&p.questions),
            "model": p.model,
            "resolved_model": reply.resolved_model,
            "provider": reply.provider,
            "calibrated": reply.calibrated,
            "method": reply.method,
            "samples": reply.samples,
            "cached": reply.cached,
            "routed": routed_out,
        });
        // 材料と全部の答えは載せない（出口と、振り分けに使った質問の確からしさだけ・H5）。
        let keys: Vec<&str> = p.questions.iter().map(|q| q.key.as_str()).collect();
        let routed_audit = p.route.as_ref().map(|route| {
            json!({ "question": route.question,
                    "confidence": routed.map(|(_, c)| c) })
        });
        self.audit(
            &ec.tenant_id,
            API,
            true,
            &json!({
                "model": p.model,
                "resolved_model": reply.resolved_model,
                "provider": reply.provider,
                "method": reply.method,
                "samples": reply.samples,
                "calibrated": reply.calibrated,
                "cached": reply.cached,
                "questions": keys,
                "port": port,
                "routed": routed_audit,
            }),
        );
        Ok((output, port))
    }
}

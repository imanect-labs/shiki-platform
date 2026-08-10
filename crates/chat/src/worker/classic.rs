//! 古典チャット経路（`classic_rag=true` の後方互換フォールバック）。
//!
//! ツールループを持たず、直近発話で事前検索した文脈を system へ注入して gateway を直叩きする。
//! 既定は agent-core ループ（[`super::generate`]）であり、こちらは運用で明示的に選んだ場合のみ。
//! `generate.rs` から切り出した（1 ファイル行数規約）。

use authz::AuthContext;
use futures::stream::StreamExt;
use llm_gateway::{GenerateRequest, GenerationRecord, Message as LlmMessage, StreamDelta};

use super::history::message_preview;
use super::sink::WorkerSink;
use super::ChatWorker;
use crate::store::ClaimedRun;
use crate::ChatError;

impl ChatWorker {
    /// 通常チャット（OFF）。古典 RAG 注入＋llm-gateway 直叩き（ツールループ無し）。
    pub(super) async fn run_classic_mode(
        &self,
        ctx: &AuthContext,
        run: &ClaimedRun,
        history: Vec<LlmMessage>,
        sink: &mut WorkerSink,
    ) -> Result<(), ChatError> {
        use agent_core::{run_doc_search, AgentEvent, EventSink};

        // skill のピン解決（通常チャットにも適用する・複数可・fail-closed・Task 6.9/#344）。
        let skills = crate::skill::AppliedSkill::load_pins(
            ctx,
            self.skill_artifacts.as_ref(),
            run,
            run.trace_id.as_deref(),
        )
        .await?;
        let scope = crate::skill::combined_scope(&skills);
        let mut history = history;

        // 直近ユーザー発話で事前検索し、文脈注入＋引用イベント。
        let query = history.last().map(message_preview).unwrap_or_default();
        let mut system = self.config.system_prompt.clone();
        for skill in &skills {
            skill.apply_system(&mut system);
            skill.audit_apply(&self.db, ctx, run).await;
        }
        // few-shot は「ピン順に前から並ぶ」よう逆順で先頭 splice する。
        for skill in skills.iter().rev() {
            skill.apply_few_shot(&mut history);
        }
        // 全ピンが doc_search を宣言に含む時のみ古典事前検索を行う（Task 6.9 の意味を
        // classic では維持する。ツールループが無い classic に「誘導」は存在しないため）。
        let search_allowed = skills
            .iter()
            .all(|s| s.allows(agent_core::ToolName::DocSearch.as_str()));
        if let (Some(search), true) = (&self.search, search_allowed) {
            match run_doc_search(
                search,
                ctx,
                &query,
                None,
                scope.as_ref(),
                run.trace_id.as_deref(),
            )
            .await
            {
                Ok(result) => {
                    system.push_str("\n\n# 参考（社内文書検索の結果）\n");
                    system.push_str(&result.context_text);
                    for c in result.citations {
                        // 古典注入でも引用を UI/監査へ流す（post-filter は検索内で済み）。
                        sink.emit(AgentEvent::Citation(agent_core::Citation {
                            node_id: c.node_id,
                            chunk_id: c.chunk_id,
                            snippet: c.snippet,
                            page: c.page,
                            heading_path: c.heading_path,
                            score: c.score,
                        }))
                        .await
                        .map_err(|e| ChatError::Internal(e.to_string()))?;
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "classic doc_search failed; continuing without");
                }
            }
        }

        // skill のモデル既定（Task 6.9・指定があるものだけ上書き・複数ピンは後勝ち・#344）。
        // 未指定時の上限はチャット設定（reasoning モデルの思考で 2048 では足りない・#352）。
        let (model, max_tokens, temperature) = match crate::skill::combined_model_defaults(&skills)
        {
            Some(defaults) => (
                defaults.model.clone().or_else(|| self.config.model.clone()),
                defaults.max_tokens.or(Some(self.config.max_tokens)),
                defaults.temperature,
            ),
            None => (
                self.config.model.clone(),
                Some(self.config.max_tokens),
                None,
            ),
        };
        let effective_model = model
            .clone()
            .unwrap_or_else(|| self.gateway.default_model().to_string());
        let req = GenerateRequest {
            model,
            system: Some(system),
            messages: history,
            tools: Vec::new(),
            effort: None,
            max_tokens,
            temperature,
        };
        let mut stream = self
            .gateway
            .stream(req)
            .await
            .map_err(|e| ChatError::Unavailable(format!("llm: {e}")))?;

        let mut text_acc = String::new();
        let mut usage = llm_gateway::Usage::default();
        while let Some(delta) = stream.next().await {
            if sink.is_cancelled() {
                break;
            }
            match delta.map_err(|e| ChatError::Unavailable(e.to_string()))? {
                StreamDelta::TextDelta { text } => {
                    text_acc.push_str(&text);
                    sink.emit(AgentEvent::Text(text))
                        .await
                        .map_err(|e| ChatError::Internal(e.to_string()))?;
                }
                StreamDelta::ThinkingDelta { text } => {
                    sink.emit(AgentEvent::Thinking(text))
                        .await
                        .map_err(|e| ChatError::Internal(e.to_string()))?;
                }
                StreamDelta::Done { usage: u, .. } => usage = u,
                _ => {} // 通常チャットはツールを使わない
            }
        }

        self.gateway
            .record_generation(
                ctx,
                &GenerationRecord {
                    idempotency_key: format!("{}:{}:0", run.run_id, run.fencing_token),
                    // 会計は実効モデル（skill 既定の上書き込み）で刻む。
                    model: effective_model,
                    usage,
                    trace_id: run.trace_id.clone(),
                    input_preview: query,
                    output_preview: text_acc.chars().take(2000).collect(),
                    app_id: None,
                },
            )
            .await;
        Ok(())
    }
}

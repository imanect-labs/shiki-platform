//! 古典チャット経路（`classic_rag=true` の後方互換フォールバック）。
//!
//! ツールループを持たず、直近発話で事前検索した文脈を system へ注入して gateway を直叩きする。
//! 既定は agent-core ループ（[`super::generate`]）であり、こちらは運用で明示的に選んだ場合のみ。
//! `generate.rs` から切り出した（1 ファイル行数規約）。

use authz::AuthContext;
use llm_gateway::{GenerateRequest, GenerationRecord, Message as LlmMessage, StreamDelta};

use super::history::message_preview;
use super::sink::WorkerSink;
use super::ChatWorker;
use crate::store::ClaimedRun;
use crate::ChatError;

/// gateway のエラーを run の失敗へ写す。
///
/// **利用上限だけは種別を保つ**（agent 経路の `generate.rs` と同じ扱い）。`Unavailable` に
/// 潰すと、llm-gateway が整形した再開目安付きの 1 行が会話では定型文に置き換わる。
fn map_llm_error(e: llm_gateway::LlmError) -> ChatError {
    match e {
        llm_gateway::LlmError::RateLimited(msg) => ChatError::RateLimited(msg),
        other => ChatError::Unavailable(format!("llm: {other}")),
    }
}

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
        // 発話ごとのツール指定（`run.requested_tools`・#734）はここでは使わない。classic は
        // ツールループを持たず（`tools: Vec::new()`）、常時ロードも「使ってよい」の注記も
        // 意味を持たない。事前検索の有無を指定で変えると、指定が能力の追加になってしまう
        // （指定は誘導であって権限ではない）。classic は運用が明示的に選ぶ後方互換経路でもある。
        //
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
                // この回答のための検索として監査に残す（#829）。
                rag::AuditTrail::for_answer(run.trace_id.as_deref(), run.thread_id, run.message_id),
            )
            .await
            {
                Ok(mut result) => {
                    // 検索は 1 回だけなので 1 から振る（ツールループと同じ台帳で番号を確定する）。
                    agent_core::CitationLedger::default()
                        .number(&mut result.citations, &mut result.context_text);
                    system.push_str("\n\n# 参考（社内文書検索の結果）\n");
                    system.push_str(&result.context_text);
                    for c in result.citations {
                        // 古典注入でも引用を UI/監査へ流す（post-filter は検索内で済み）。
                        sink.emit(AgentEvent::Citation(c))
                            .await
                            .map_err(|e| ChatError::Internal(e.to_string()))?;
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "classic doc_search failed; continuing without");
                }
            }
        }

        // 今日の日付（#546）は検索結果のあと、system の最後に置く（ほかの経路と同じ）。
        system.push_str(&super::opts::today_note(
            run.created_at,
            self.config.timezone,
        ));

        // skill のモデル既定（Task 6.9・指定があるものだけ上書き・複数ピンは後勝ち・#344）。
        // 未指定時の上限はチャット設定（reasoning モデルの思考で 2048 では足りない・#352）。
        let defaults = crate::skill::combined_model_defaults(&skills);
        let (max_tokens, temperature) = match &defaults {
            Some(d) => (d.max_tokens.or(Some(self.config.max_tokens)), d.temperature),
            None => (Some(self.config.max_tokens), None),
        };
        // モデルと思考強度: ユーザーの選択 → skill → Auto の振り先 → 設定（#553・run_model.rs）。
        let chosen = super::run_model::effective_model(
            &self.gateway,
            ctx,
            run,
            defaults.as_ref().and_then(|d| d.model.as_deref()),
            self.config.model.as_deref(),
        )
        .await?;
        let model = chosen.model;
        let effective_model = model
            .clone()
            .unwrap_or_else(|| self.gateway.default_model().to_string());
        // コンテキスト表示の内訳（送る中身から見積り、応答の実測で総量を合わせる）。
        let context_estimate = agent_core::context_usage::estimate(Some(&system), [], &history);
        let window_tokens = self.gateway.context_window_tokens(model.as_deref());
        let req = GenerateRequest {
            model,
            system: Some(system),
            messages: history,
            tools: Vec::new(),
            effort: chosen.effort,
            max_tokens,
            temperature,
            session_id: Some(run.thread_id.to_string()),
        };
        // Langfuse の所要時間に使う（未指定だと 0 秒と表示される）。
        let started_at = chrono::Utc::now();
        // 確立待ち（応答ヘッダ・再試行）の間の停止でも手放す（#602・生成前なので会計なし）。
        let setup = self.gateway.stream(ctx, req);
        let Some(stream) = agent_core::await_unless_cancelled(setup, sink).await else {
            return Ok(());
        };
        let mut stream = stream.map_err(map_llm_error)?;
        let (text_acc, usage) = consume(&mut stream, sink).await?;

        drop(stream); // 会計（DB・Langfuse）を待つ間も上流を握り続けないよう、先に手放す。
        let context = agent_core::ContextUsage::new(
            effective_model.clone(),
            window_tokens,
            max_tokens,
            agent_core::context_usage::fit(context_estimate, usage.prompt_tokens),
        );
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
                    source: llm_gateway::UsageSource::Chat,
                    // この経路は 1 リクエスト 1 生成。
                    first_in_trace: true,
                    started_at: Some(started_at),
                },
            )
            .await;
        // 表示用の付加情報なので、会計を記録してから送る（送出の失敗で会計を落とさない）。
        sink.emit(AgentEvent::ContextUsage(context))
            .await
            .map_err(|e| ChatError::Internal(e.to_string()))?;
        Ok(())
    }
}

/// 応答ストリームを読み切り、本文・思考をイベントへ流す（停止要求で打ち切る・#602）。
/// 戻り値は本文と usage（打ち切ったときは報告が無く既定値）。
async fn consume(
    stream: &mut llm_gateway::DeltaStream,
    sink: &mut WorkerSink,
) -> Result<(String, llm_gateway::Usage), ChatError> {
    use agent_core::{AgentEvent, EventSink};

    let mut text_acc = String::new();
    let mut usage = llm_gateway::Usage::default();
    loop {
        // 黙っている間も停止を見張り、止まったらストリームを手放す（#602）。
        let delta = match agent_core::next_unless_cancelled(stream, sink).await {
            agent_core::NextDelta::Item(delta) => delta,
            agent_core::NextDelta::End | agent_core::NextDelta::Cancelled => break,
        };
        let event = match delta.map_err(map_llm_error)? {
            StreamDelta::TextDelta { text } => {
                text_acc.push_str(&text);
                AgentEvent::Text(text)
            }
            StreamDelta::ThinkingDelta { text } => AgentEvent::Thinking(text),
            StreamDelta::Done { usage: u, .. } => {
                usage = u;
                continue;
            }
            _ => continue, // 通常チャットはツールを使わない
        };
        sink.emit(event)
            .await
            .map_err(|e| ChatError::Internal(e.to_string()))?;
    }
    Ok((text_acc, usage))
}

#[cfg(test)]
mod tests {
    use super::*;
    use llm_gateway::LlmError;

    #[test]
    fn rate_limit_keeps_kind_and_gateway_line() {
        let line = "利用上限に達しました（Resets in 37min.）";
        let e = map_llm_error(LlmError::RateLimited(line.into()));
        assert!(matches!(&e, ChatError::RateLimited(m) if m == line));
        assert_eq!(e.user_message(), line);
    }

    #[test]
    fn provider_failure_is_not_shown_verbatim() {
        let e = map_llm_error(LlmError::Unavailable(
            "openai 502: {\"workspace\":\"wrk_01\"}".into(),
        ));
        assert!(matches!(e, ChatError::Unavailable(_)));
        assert_eq!(
            e.user_message(),
            crate::error::failure_message::PROVIDER_UNAVAILABLE
        );
    }
}

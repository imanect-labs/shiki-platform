//! 評価用の EventSink（集計＋窓の模擬）。

use agent_core::{AgentError, AgentEvent, Checkpoint, EventSink};

pub(crate) struct EvalSink {
    window: u32,
    pub(crate) peak_prompt: u64,
    pub(crate) overflowed: bool,
    pub(crate) compactions: usize,
    pub(crate) tool_calls: usize,
    pub(crate) tool_errors: usize,
    /// ツール呼び出しの無い本文だけの区切り（Text が来て、次の ToolCall 前に ContextUsage が来た数）。
    pub(crate) text_turns: usize,
    pub(crate) steps: usize,
    pub(crate) cost_micros_hint: i64,
    /// 各ステップの prompt の合計（エラーで終わった run の費用見積りに使う）。
    pub(crate) prompt_sum: u64,
    /// 解析用の要約ログ（本文は先頭だけ）。
    pub(crate) log: Vec<serde_json::Value>,
    text_buf: String,
    saw_call_this_step: bool,
}

impl EvalSink {
    pub(crate) fn new(window: u32) -> Self {
        EvalSink {
            window,
            peak_prompt: 0,
            overflowed: false,
            compactions: 0,
            tool_calls: 0,
            tool_errors: 0,
            text_turns: 0,
            steps: 0,
            cost_micros_hint: 0,
            prompt_sum: 0,
            log: Vec::new(),
            text_buf: String::new(),
            saw_call_this_step: false,
        }
    }

    fn flush_text(&mut self) {
        if !self.text_buf.trim().is_empty() {
            let t: String = self.text_buf.chars().take(600).collect();
            self.log.push(serde_json::json!({"text": t}));
        }
        self.text_buf.clear();
    }
}

#[async_trait::async_trait]
impl EventSink for EvalSink {
    async fn emit(&mut self, event: AgentEvent) -> Result<(), AgentError> {
        match event {
            AgentEvent::Text(t) => self.text_buf.push_str(&t),
            AgentEvent::ToolCall {
                name, input, step, ..
            } => {
                self.flush_text();
                self.tool_calls += 1;
                self.saw_call_this_step = true;
                let inp: String = input.to_string().chars().take(300).collect();
                self.log
                    .push(serde_json::json!({"call": name, "input": inp, "step": step}));
            }
            AgentEvent::ToolResult { ok, content, .. } => {
                if !ok {
                    self.tool_errors += 1;
                }
                let c: String = content.chars().take(300).collect();
                self.log
                    .push(serde_json::json!({"result_ok": ok, "content": c}));
            }
            AgentEvent::FailureRecovery { detail, action } => {
                if action.as_str() == "context_compacted" {
                    self.compactions += 1;
                }
                self.log
                    .push(serde_json::json!({"recovery": action.as_str(), "detail": detail}));
            }
            AgentEvent::ContextUsage(u) => {
                self.steps += 1;
                if !self.saw_call_this_step && !self.text_buf.trim().is_empty() {
                    self.text_turns += 1;
                }
                self.flush_text();
                self.saw_call_this_step = false;
                self.peak_prompt = self.peak_prompt.max(u.used_tokens);
                self.prompt_sum += u.used_tokens;
                self.log.push(
                    serde_json::json!({"prompt_tokens": u.used_tokens, "measured": u.measured}),
                );
                // 窓の模擬: 実測 prompt＋出力枠が窓を超えたら、実機では拒否されている。
                let reserved = u64::from(u.reserved_output_tokens.unwrap_or(0));
                if u.measured && u.used_tokens + reserved > u64::from(self.window) {
                    self.overflowed = true;
                    // 文言は vLLM の拒否に合わせる（ハーネスが窓あふれとして扱えるように）。
                    return Err(AgentError::Llm(format!(
                        "simulated: maximum context length exceeded: {} + {} > {}",
                        u.used_tokens, reserved, self.window
                    )));
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn is_cancelled(&self) -> bool {
        false
    }

    async fn save_checkpoint(&mut self, _checkpoint: &Checkpoint) -> Result<(), AgentError> {
        Ok(())
    }
}

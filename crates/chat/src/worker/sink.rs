//! `WorkerSink` — agent-core / 古典生成のイベントを永続化＋projection する [`EventSink`]。
//!
//! 各イベントを `generation_event` へ append（真実のソース・fencing 一致時のみ）＋Redis publish し、
//! 同時に `message.content` の projection（[`ContentBlock`] 列）を組み立てる。ツールや思考、
//! 引用は content block として順序保存し、確定時に message へ書き戻す。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use agent_core::{AgentError, AgentEvent, Citation as AgentCitation, EventSink};
use uuid::Uuid;

use crate::model::{Citation, ContentBlock, StreamEventKind};
use crate::store::ChatStore;

/// 生成イベントの受け口（1 run 分）。
pub(crate) struct WorkerSink {
    store: ChatStore,
    run_id: Uuid,
    fencing_token: i64,
    cancel: Arc<AtomicBool>,
    /// message.content の projection（イベント順に組み立て）。
    content: Vec<ContentBlock>,
    /// リース喪失（fencing 不一致）を検知したか。
    lost_lease: bool,
}

impl WorkerSink {
    pub(crate) fn new(
        store: ChatStore,
        run_id: Uuid,
        fencing_token: i64,
        cancel: Arc<AtomicBool>,
    ) -> Self {
        WorkerSink {
            store,
            run_id,
            fencing_token,
            cancel,
            content: Vec::new(),
            lost_lease: false,
        }
    }

    /// projection として確定した content を取り出す。
    pub(crate) fn content(&self) -> &[ContentBlock] {
        &self.content
    }

    /// リースを失ったか（ゾンビ化）。
    pub(crate) fn lost_lease(&self) -> bool {
        self.lost_lease
    }

    /// AgentEvent を content projection へ畳み込む（テキスト/思考は連続分を結合）。
    fn accumulate(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::Text(t) => match self.content.last_mut() {
                Some(ContentBlock::Text { text }) => text.push_str(t),
                _ => self.content.push(ContentBlock::Text { text: t.clone() }),
            },
            AgentEvent::Thinking(t) => match self.content.last_mut() {
                Some(ContentBlock::Thinking { text }) => text.push_str(t),
                _ => self
                    .content
                    .push(ContentBlock::Thinking { text: t.clone() }),
            },
            AgentEvent::ToolCall { id, name, input } => {
                self.content.push(ContentBlock::ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                });
            }
            AgentEvent::ToolResult {
                tool_call_id,
                content,
                ..
            } => {
                self.content.push(ContentBlock::ToolResult {
                    tool_call_id: tool_call_id.clone(),
                    content: content.clone(),
                });
            }
            AgentEvent::Citation(c) => {
                self.content.push(ContentBlock::Citation(to_citation(c)));
            }
        }
    }
}

/// AgentEvent → SSE イベント種別。
fn to_stream_kind(event: &AgentEvent) -> StreamEventKind {
    match event {
        AgentEvent::Text(t) => StreamEventKind::Token { text: t.clone() },
        AgentEvent::Thinking(t) => StreamEventKind::Thinking { text: t.clone() },
        AgentEvent::ToolCall { id, name, input } => StreamEventKind::ToolCall {
            id: id.clone(),
            name: name.clone(),
            input: input.clone(),
        },
        AgentEvent::ToolResult {
            tool_call_id,
            ok,
            content,
        } => StreamEventKind::ToolResult {
            tool_call_id: tool_call_id.clone(),
            ok: *ok,
            content: content.clone(),
        },
        AgentEvent::Citation(c) => StreamEventKind::Citation(to_citation(c)),
    }
}

/// agent-core の Citation → chat の Citation（同型フィールド）。
fn to_citation(c: &AgentCitation) -> Citation {
    Citation {
        node_id: c.node_id.clone(),
        chunk_id: c.chunk_id.clone(),
        snippet: c.snippet.clone(),
        page: c.page,
        heading_path: c.heading_path.clone(),
        score: c.score,
    }
}

#[async_trait::async_trait]
impl EventSink for WorkerSink {
    async fn emit(&mut self, event: AgentEvent) -> Result<(), AgentError> {
        let kind = to_stream_kind(&event);
        match self
            .store
            .append_stream_event(self.run_id, self.fencing_token, &kind)
            .await
        {
            Ok(Some(_)) => {}
            Ok(None) => {
                // fencing 不一致＝リース喪失（別ワーカーが takeover）。ゾンビ書込を止める。
                self.lost_lease = true;
                return Err(AgentError::Sink("lease lost (fencing mismatch)".into()));
            }
            Err(e) => return Err(AgentError::Sink(e.to_string())),
        }
        self.accumulate(&event);
        Ok(())
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

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
            AgentEvent::Artifact { artifact, .. } => {
                self.content.push(ContentBlock::FileRef {
                    node_id: artifact.node_id.clone(),
                    name: artifact.name.clone(),
                });
            }
            // 自律プロファイルの構造化イベント（計画/サブタスク/予算/承認/失敗回復）は
            // content block へは projection しない（進捗の可視化はライブ SSE 側で扱う・W4 で結線）。
            AgentEvent::PlanUpdated(_)
            | AgentEvent::SubtaskUpdated { .. }
            | AgentEvent::BudgetWarning { .. }
            | AgentEvent::ApprovalRequested { .. }
            | AgentEvent::ApprovalResolved { .. }
            | AgentEvent::FailureRecovery { .. } => {}
        }
    }
}

/// AgentEvent → SSE イベント種別。content/ストリームに写らないイベントは `None`。
///
/// Phase 5 自律の計画/予算/承認/失敗回復イベントの SSE 種別は W4（可観測化）で `StreamEventKind` に
/// 追加して結線する。Chat プロファイルはこれらを発火しないため、W1 時点では `None`（無視）で安全。
fn to_stream_kind(event: &AgentEvent) -> Option<StreamEventKind> {
    Some(match event {
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
        AgentEvent::Artifact { artifact, .. } => StreamEventKind::FileRef {
            node_id: artifact.node_id.clone(),
            name: artifact.name.clone(),
        },
        AgentEvent::PlanUpdated(_)
        | AgentEvent::SubtaskUpdated { .. }
        | AgentEvent::BudgetWarning { .. }
        | AgentEvent::ApprovalRequested { .. }
        | AgentEvent::ApprovalResolved { .. }
        | AgentEvent::FailureRecovery { .. } => return None,
    })
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
        // SSE/永続に写らないイベント（W1 時点の自律構造化イベント）は projection だけ更新して返す。
        let Some(kind) = to_stream_kind(&event) else {
            self.accumulate(&event);
            return Ok(());
        };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_event_maps_to_file_ref() {
        // AgentEvent::Artifact → SSE file_ref（node_id/name を保持）。
        let ev = AgentEvent::Artifact {
            tool_call_id: "call-1".into(),
            artifact: agent_core::ArtifactRef {
                node_id: "n1".into(),
                name: "result.csv".into(),
            },
        };
        match to_stream_kind(&ev) {
            Some(StreamEventKind::FileRef { node_id, name }) => {
                assert_eq!(node_id, "n1");
                assert_eq!(name, "result.csv");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn autonomous_events_do_not_stream() {
        // 計画/予算/失敗回復イベントは SSE 種別へは写らない（W4 で結線）。
        let ev = AgentEvent::BudgetWarning {
            kind: agent_core::BudgetKind::Tokens,
            used: 8,
            limit: 10,
        };
        assert!(to_stream_kind(&ev).is_none());
    }
}

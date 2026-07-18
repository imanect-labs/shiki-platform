//! `WorkerSink` — agent-core / 古典生成のイベントを永続化＋projection する [`EventSink`]。
//!
//! 各イベントを `generation_event` へ append（真実のソース・fencing 一致時のみ）＋Redis publish し、
//! 同時に `message.content` の projection（[`ContentBlock`] 列）を組み立てる。ツールや思考、
//! 引用は content block として順序保存し、確定時に message へ書き戻す。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use agent_core::{AgentError, AgentEvent, Citation as AgentCitation, EventSink};
use uuid::Uuid;

use crate::model::{Citation, ContentBlock, PlanSubtask, StreamEventKind};
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
            // 検証済みスペックのみが emit_ui から届く（Task 6.4・検証は gui 側の信頼境界）。
            AgentEvent::GenerativeUi { spec } => {
                self.content
                    .push(ContentBlock::GenerativeUi { spec: spec.clone() });
            }
            // 保存パイプライン通過済みの参照のみが emit_workflow から届く（Task 10.13）。
            AgentEvent::WorkflowRef { workflow } => {
                self.content.push(ContentBlock::WorkflowRef {
                    workflow: workflow.clone(),
                });
            }
            // StorageService へ作成済みのノート参照のみが save_note から届く（Task 11P.5）。
            AgentEvent::NoteRef { note } => {
                self.content
                    .push(ContentBlock::NoteRef { note: note.clone() });
            }
            // 未保存の下書きノート（save_note の下書き確定型・issue #282）。履歴からも下書きへ
            // 辿れるよう content block に残す（本文は client 下書きストアの真実源ではなく、
            // 開き直しの seed。確定は「ドライブに保存」）。
            AgentEvent::NoteDraft { draft } => {
                self.content.push(ContentBlock::NoteDraft {
                    draft: draft.clone(),
                });
            }
            // 未保存の下書きスライド（save_slide の下書き確定型・Task 11.3）。note_draft と同じく
            // 履歴からも下書きへ辿れるよう content block に残す（開き直しの seed・確定は UI 保存）。
            AgentEvent::SlideDraft { draft } => {
                self.content.push(ContentBlock::SlideDraft {
                    draft: draft.clone(),
                });
            }
            // 未保存の下書き CSV（save_csv の下書き確定型・Task 11.11）。同型。
            AgentEvent::CsvDraft { draft } => {
                self.content.push(ContentBlock::CsvDraft {
                    draft: draft.clone(),
                });
            }
            // 自律プロファイルの構造化イベント（計画/サブタスク/予算/承認/失敗回復）は
            // content block へは projection しない（進捗の可視化はライブ SSE 側で扱う・W4 で結線）。
            // Office ライブ編集も同様に projection しない（履歴再生で二重 paste しない・#328）。
            AgentEvent::PlanUpdated(_)
            | AgentEvent::SubtaskUpdated { .. }
            | AgentEvent::BudgetWarning { .. }
            | AgentEvent::ApprovalRequested { .. }
            | AgentEvent::ApprovalResolved { .. }
            | AgentEvent::OfficeLiveEdit { .. }
            | AgentEvent::FailureRecovery { .. } => {}
        }
    }
}

/// AgentEvent → SSE イベント種別。全 AgentEvent が SSE 種別へ写る（`generation_event` に append され
/// replay 可能）。message.content への projection 有無は [`WorkerSink::accumulate`] が別に決める。
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
        AgentEvent::Artifact { artifact, .. } => StreamEventKind::FileRef {
            node_id: artifact.node_id.clone(),
            name: artifact.name.clone(),
        },
        AgentEvent::GenerativeUi { spec } => StreamEventKind::GenerativeUi { spec: spec.clone() },
        AgentEvent::WorkflowRef { workflow } => StreamEventKind::WorkflowRef {
            workflow: workflow.clone(),
        },
        AgentEvent::NoteRef { note } => StreamEventKind::NoteRef { note: note.clone() },
        AgentEvent::NoteDraft { draft } => StreamEventKind::NoteDraft {
            draft: draft.clone(),
        },
        AgentEvent::SlideDraft { draft } => StreamEventKind::SlideDraft {
            draft: draft.clone(),
        },
        AgentEvent::CsvDraft { draft } => StreamEventKind::CsvDraft {
            draft: draft.clone(),
        },
        // 開いている Office セッションへのライブ編集（#328）。ライブ SSE のみ（content 非 projection）。
        AgentEvent::OfficeLiveEdit { node_id, html } => StreamEventKind::OfficeLiveEdit {
            node_id: node_id.clone(),
            html: html.clone(),
        },
        // 自律プロファイルの構造化イベント（Task 5.9 ライブ配信）。generation_event に append され
        // replay 可能（監査・5.10）だが message.content へは projection しない。
        AgentEvent::PlanUpdated(plan) => StreamEventKind::Plan {
            subtasks: plan
                .subtasks
                .iter()
                .map(|s| PlanSubtask {
                    id: s.id.clone(),
                    title: s.title.clone(),
                    status: subtask_status_str(s.status).to_string(),
                })
                .collect(),
        },
        AgentEvent::SubtaskUpdated { id, status } => StreamEventKind::Plan {
            // 単一サブタスク更新は最小の Plan イベントに畳む（UI は id で差し込む）。
            subtasks: vec![PlanSubtask {
                id: id.clone(),
                title: String::new(),
                status: subtask_status_str(*status).to_string(),
            }],
        },
        AgentEvent::BudgetWarning { kind, used, limit } => StreamEventKind::BudgetWarning {
            kind: kind.as_str().to_string(),
            used: *used,
            limit: *limit,
        },
        AgentEvent::ApprovalRequested {
            tool_call_id,
            name,
            input,
            reason,
        } => StreamEventKind::ApprovalRequested {
            tool_call_id: tool_call_id.clone(),
            name: name.clone(),
            input: input.clone(),
            reason: reason.clone(),
        },
        AgentEvent::ApprovalResolved {
            tool_call_id,
            approved,
        } => StreamEventKind::ApprovalResolved {
            tool_call_id: tool_call_id.clone(),
            approved: *approved,
        },
        AgentEvent::FailureRecovery { detail, action } => StreamEventKind::FailureRecovery {
            detail: detail.clone(),
            action: action.as_str().to_string(),
        },
    }
}

/// agent-core の `SubtaskStatus` を snake_case 文字列へ。
fn subtask_status_str(s: agent_core::SubtaskStatus) -> &'static str {
    use agent_core::SubtaskStatus;
    match s {
        SubtaskStatus::Todo => "todo",
        SubtaskStatus::Doing => "doing",
        SubtaskStatus::Done => "done",
        SubtaskStatus::Blocked => "blocked",
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
            StreamEventKind::FileRef { node_id, name } => {
                assert_eq!(node_id, "n1");
                assert_eq!(name, "result.csv");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn budget_warning_maps_to_sse() {
        // 予算警告は SSE `budget_warning` へ写る（ライブ配信・Task 5.9）。
        let ev = AgentEvent::BudgetWarning {
            kind: agent_core::BudgetKind::Tokens,
            used: 8,
            limit: 10,
        };
        match to_stream_kind(&ev) {
            StreamEventKind::BudgetWarning { kind, used, limit } => {
                assert_eq!(kind, "tokens");
                assert_eq!((used, limit), (8, 10));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn slide_draft_event_maps_to_sse_and_projection() {
        // AgentEvent::SlideDraft → SSE slide_draft（履歴 projection でも残る・Task 11.3）。
        let draft = serde_json::json!({ "name": "提案書", "content": "{\"version\":1}" });
        let ev = AgentEvent::SlideDraft {
            draft: draft.clone(),
        };
        match to_stream_kind(&ev) {
            StreamEventKind::SlideDraft { draft: d } => assert_eq!(d, draft),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn csv_draft_event_maps_to_sse_and_projection() {
        // AgentEvent::CsvDraft → SSE csv_draft（履歴 projection でも残る・Task 11.11）。
        let draft = serde_json::json!({ "name": "売上一覧", "csv": "a,b\n1,2\n" });
        let ev = AgentEvent::CsvDraft {
            draft: draft.clone(),
        };
        match to_stream_kind(&ev) {
            StreamEventKind::CsvDraft { draft: d } => assert_eq!(d, draft),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn office_live_edit_maps_to_sse() {
        // AgentEvent::OfficeLiveEdit → SSE office_live_edit（ライブ配信）。content への
        // projection は accumulate 側の no-op アームで抑止する（履歴再生で二重 paste しない・#328）。
        let ev = AgentEvent::OfficeLiveEdit {
            node_id: "file-1".into(),
            html: "<p>置換</p>".into(),
        };
        match to_stream_kind(&ev) {
            StreamEventKind::OfficeLiveEdit { node_id, html } => {
                assert_eq!(node_id, "file-1");
                assert_eq!(html, "<p>置換</p>");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn plan_updated_maps_to_sse_subtasks() {
        let mut plan = agent_core::Plan::default();
        plan.revise(vec![agent_core::plan::SubtaskInput {
            title: "調査".into(),
            status: Some("doing".into()),
        }]);
        match to_stream_kind(&AgentEvent::PlanUpdated(plan)) {
            StreamEventKind::Plan { subtasks } => {
                assert_eq!(subtasks.len(), 1);
                assert_eq!(subtasks[0].status, "doing");
                assert_eq!(subtasks[0].title, "調査");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}

//! `WorkerSink` — agent-core / 古典生成のイベントを永続化＋projection する [`EventSink`]。
//!
//! 各イベントを `generation_event` へ append（真実のソース・fencing 一致時のみ）＋Redis publish し、
//! 同時に `message.content` の projection（[`ContentBlock`] 列）を組み立てる。ツールや思考、
//! 引用は content block として順序保存し、確定時に message へ書き戻す。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use agent_core::{AgentError, AgentEvent, Checkpoint, Citation as AgentCitation, EventSink};
use uuid::Uuid;

use crate::model::{Citation, ContentBlock, PlanSubtask, StreamEventKind};
use crate::store::ChatStore;

/// durable 保存するチェックポイントの封筒（#351）。
///
/// agent-core の [`Checkpoint`]（ステップ境界の状態）に、**その境界までに追記済みの
/// `generation_event` の最大 seq** を添える。resume 時の projection 再構築（seed）はこの seq
/// までに限定する — 中断ステップの途中イベント（部分テキスト・実行済みツール呼出）は
/// チェックポイントに含まれず当該ステップごと再生成されるため、seed に混ぜると二重になる。
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct CheckpointEnvelope {
    /// projection seed の上限 seq（この境界までのイベントだけが確定済み）。
    pub(crate) event_seq: i64,
    pub(crate) checkpoint: Checkpoint,
}

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
    /// ステップ境界のチェックポイントを durable run 行へ永続化するか（自律 run のみ・#351）。
    persist_checkpoints: bool,
    /// この sink が追記した最新の `generation_event` seq（チェックポイント封筒の境界）。
    last_seq: i64,
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
            persist_checkpoints: false,
            last_seq: 0,
        }
    }

    /// チェックポイント永続化を有効化する（自律 run・#351）。
    #[must_use]
    pub(crate) fn with_checkpoints(mut self, enabled: bool) -> Self {
        self.persist_checkpoints = enabled;
        self
    }

    /// projection として確定した content を取り出す。
    pub(crate) fn content(&self) -> &[ContentBlock] {
        &self.content
    }

    /// リースを失ったか（ゾンビ化）。
    pub(crate) fn lost_lease(&self) -> bool {
        self.lost_lease
    }

    /// チェックポイント再開の前に、既存イベントログから content projection を再構築する（#351）。
    ///
    /// takeover した run はチェックポイント（ステップ境界）から**続きだけ**を生成するため、
    /// これ無しでは finalize の projection が takeover 前のテキスト/ツール結果を失う。
    /// `up_to_seq`＝チェックポイント境界までに限定する（中断ステップの途中イベントは当該ステップ
    /// ごと再生成されるため、混ぜると二重になる）。写像はライブ経路と同一の [`Self::accumulate`]。
    pub(crate) async fn seed_from_log(&mut self, up_to_seq: i64) -> Result<(), crate::ChatError> {
        for ev in self.store.replay_events(self.run_id, 0).await? {
            if ev.seq > up_to_seq {
                break; // replay は seq 昇順
            }
            self.accumulate(&ev.event);
        }
        // 境界 seq を引き継ぐ: 新規 append が 0 件のまま次の checkpoint 保存に至っても
        // event_seq が 0 に退行しない（seed 上限の後退防止・#351）。
        self.last_seq = self.last_seq.max(up_to_seq);
        Ok(())
    }

    /// SSE イベントを content projection へ畳み込む（テキスト/思考は連続分を結合）。
    ///
    /// ライブ生成（emit）と replay 再構築（seed_from_log）が同じ写像を通る（二重実装しない）。
    /// projection しない種別（plan/予算/承認/office ライブ編集/status 等）はここで落とす
    /// （進捗はライブ SSE 側・履歴再生で二重 paste しない・#328）。
    fn accumulate(&mut self, kind: &StreamEventKind) {
        match kind {
            StreamEventKind::Token { text } => match self.content.last_mut() {
                Some(ContentBlock::Text { text: acc }) => acc.push_str(text),
                _ => self.content.push(ContentBlock::Text { text: text.clone() }),
            },
            StreamEventKind::Thinking { text } => match self.content.last_mut() {
                Some(ContentBlock::Thinking { text: acc }) => acc.push_str(text),
                _ => self
                    .content
                    .push(ContentBlock::Thinking { text: text.clone() }),
            },
            StreamEventKind::ToolCall { id, name, input } => {
                self.content.push(ContentBlock::ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                });
            }
            StreamEventKind::ToolResult {
                tool_call_id,
                content,
                ..
            } => {
                self.content.push(ContentBlock::ToolResult {
                    tool_call_id: tool_call_id.clone(),
                    content: content.clone(),
                });
            }
            StreamEventKind::Citation(c) => {
                self.content.push(ContentBlock::Citation(c.clone()));
            }
            // code_interpreter の保存済み成果物（Task 4.11）。
            StreamEventKind::FileRef { node_id, name } => {
                self.content.push(ContentBlock::FileRef {
                    node_id: node_id.clone(),
                    name: name.clone(),
                });
            }
            // 検証済みスペックのみが emit_ui から届く（Task 6.4・検証は gui 側の信頼境界）。
            StreamEventKind::GenerativeUi { spec } => {
                self.content
                    .push(ContentBlock::GenerativeUi { spec: spec.clone() });
            }
            // 保存パイプライン通過済みの参照のみが emit_workflow から届く（Task 10.13）。
            StreamEventKind::WorkflowRef { workflow } => {
                self.content.push(ContentBlock::WorkflowRef {
                    workflow: workflow.clone(),
                });
            }
            // StorageService へ作成済みのノート参照のみが save_note から届く（Task 11P.5）。
            StreamEventKind::NoteRef { note } => {
                self.content
                    .push(ContentBlock::NoteRef { note: note.clone() });
            }
            // 未保存の下書き（ノート/スライド/CSV/Word・下書き確定型）。履歴からも下書きへ
            // 辿れるよう content block に残す（開き直しの seed・確定は UI 保存・#282/#332）。
            StreamEventKind::NoteDraft { draft } => {
                self.content.push(ContentBlock::NoteDraft {
                    draft: draft.clone(),
                });
            }
            StreamEventKind::SlideDraft { draft } => {
                self.content.push(ContentBlock::SlideDraft {
                    draft: draft.clone(),
                });
            }
            StreamEventKind::CsvDraft { draft } => {
                self.content.push(ContentBlock::CsvDraft {
                    draft: draft.clone(),
                });
            }
            StreamEventKind::DocumentDraft { draft } => {
                self.content.push(ContentBlock::DocumentDraft {
                    draft: draft.clone(),
                });
            }
            // ライブ専用/進捗/端末イベントは projection しない。
            StreamEventKind::OfficeLiveEdit { .. }
            | StreamEventKind::Plan { .. }
            | StreamEventKind::BudgetWarning { .. }
            | StreamEventKind::ApprovalRequested { .. }
            | StreamEventKind::ApprovalResolved { .. }
            | StreamEventKind::FailureRecovery { .. }
            | StreamEventKind::Status { .. }
            | StreamEventKind::Error { .. }
            | StreamEventKind::Done { .. } => {}
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
        AgentEvent::DocumentDraft { draft } => StreamEventKind::DocumentDraft {
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
            Ok(Some(seq)) => self.last_seq = seq,
            Ok(None) => {
                // fencing 不一致＝リース喪失（別ワーカーが takeover）。ゾンビ書込を止める。
                self.lost_lease = true;
                return Err(AgentError::Sink("lease lost (fencing mismatch)".into()));
            }
            Err(e) => return Err(AgentError::Sink(e.to_string())),
        }
        self.accumulate(&kind);
        Ok(())
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// ステップ境界のチェックポイントを durable run 行へ保存する（自律 run のみ・#351）。
    ///
    /// fencing 不一致はリース喪失としてループを止める（ゾンビ書込防止）。一時的な DB エラーは
    /// warn のみで続行する（best-effort: 次の境界で再保存され、resume は一つ前の境界へ戻るだけ。
    /// 副作用の収束は版管理と冪等キーが担う）。
    async fn save_checkpoint(&mut self, checkpoint: &Checkpoint) -> Result<(), AgentError> {
        if !self.persist_checkpoints {
            return Ok(());
        }
        // 封筒に「この境界までの event seq」を添える（resume 時の projection seed の上限・#351）。
        let value = serde_json::json!({
            "event_seq": self.last_seq,
            "checkpoint": checkpoint,
        });
        match self
            .store
            .save_checkpoint(self.run_id, self.fencing_token, &value)
            .await
        {
            Ok(true) => Ok(()),
            Ok(false) => {
                self.lost_lease = true;
                Err(AgentError::Sink("lease lost (fencing mismatch)".into()))
            }
            Err(e) => {
                tracing::warn!(run_id = %self.run_id, error = %e, "checkpoint 保存に失敗（次の境界で再試行）");
                Ok(())
            }
        }
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

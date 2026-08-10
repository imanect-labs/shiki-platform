//! `AgentEvent` → SSE イベント種別（[`StreamEventKind`]）への写像。
//!
//! [`super::sink`] から切り出した（1 ファイル行数ゲート）。ここは**ワイヤ形式への変換だけ**を
//! 担う純関数の集まりで、DB も状態も触らない。projection（message.content へ残すか）は
//! [`super::sink::WorkerSink::accumulate`] が別に決める。

use agent_core::{AgentEvent, Citation as AgentCitation};

use crate::model::{Citation, PlanSubtask, StreamEventKind};

/// サブエージェントが中継したイベント → SSE イベント種別（#391）。
///
/// 子は**自分のループ番号**で step を数えるので、そのまま流すと親の step 空間と衝突する
/// （実測: 親が step 4 で 3 体を起動している最中に、子の `web_fetch` が step 5 として届き、
/// 親の step 5 のツール群と同じ「並行して N 件」に混ざった）。step を落として、
/// 「子の実行」であることだけを印として残す。UI はこの印で束ね方と見せ方を変える。
pub(super) fn to_child_stream_kind(event: &AgentEvent) -> StreamEventKind {
    match to_stream_kind(event) {
        StreamEventKind::ToolCall {
            id, name, input, ..
        } => StreamEventKind::ToolCall {
            id,
            name,
            input,
            step: None,
            via_subagent: true,
        },
        other => other,
    }
}

/// AgentEvent → SSE イベント種別。全 AgentEvent が SSE 種別へ写る（`generation_event` に append され
/// replay 可能）。message.content への projection 有無は [`WorkerSink::accumulate`] が別に決める。
pub(super) fn to_stream_kind(event: &AgentEvent) -> StreamEventKind {
    match event {
        AgentEvent::Text(t) => StreamEventKind::Token { text: t.clone() },
        AgentEvent::Thinking(t) => StreamEventKind::Thinking { text: t.clone() },
        AgentEvent::ToolCall {
            id,
            name,
            input,
            step,
        } => StreamEventKind::ToolCall {
            id: id.clone(),
            name: name.clone(),
            input: input.clone(),
            // ライブ生成では常に既知（None は過去 run の replay だけ）。
            step: Some(*step),
            via_subagent: false,
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
        AgentEvent::DocumentRef { document } => StreamEventKind::DocumentRef {
            document: document.clone(),
        },
        // skill 発動記録（#344）。generation_event に残り replay 可能（UI はチップ表示）。
        AgentEvent::SkillInvoked { skill } => StreamEventKind::SkillInvoked {
            skill: skill.clone(),
        },
        // サブエージェント委譲の記録（#391）。子の生イベントは親へ流れない（要約のみ）。
        AgentEvent::SubagentRun { subagent } => StreamEventKind::SubagentRun {
            subagent: subagent.clone(),
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

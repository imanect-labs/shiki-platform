//! 承認ゲート＋ツール実行（Task 5.6）。[`crate::agent`] のループから切り出した補助。
//!
//! `authorize` が実行前ゲート（安全ツール/事前許可は素通し・破壊系/egress は承認待ち）を担い、
//! `execute_tool` が dispatch、`emit_tool_events` が結果/引用/成果物イベントの外部化を担う。

use std::collections::HashMap;
use std::sync::Arc;

use authz::AuthContext;

use crate::agent::PendingCall;
use crate::approval::{ApprovalDecision, Approver};
use crate::event::{AgentError, AgentEvent, EventSink};
use crate::profile::AgentOptions;
use crate::tool::{Tool, ToolOutcome};

/// 承認ゲートの判定結果。
pub(crate) enum Authz {
    /// 実行してよい（安全ツール・事前許可・承認済み）。
    Proceed,
    /// 実行しない。観測テキストをモデルへ戻す（未許可・却下）。
    Reject(String),
    /// 承認待ち中にキャンセルされた（run を停止する）。
    Cancel,
}

/// ツール実行前の承認ゲート（Task 5.6）。
///
/// 安全ツール（`requires_confirmation()==false`）と事前許可済みは即 `Proceed`。破壊系は `approver`
/// があれば承認要求を発火して決定を待ち（承認→Proceed／却下→Reject／キャンセル→Cancel）、
/// approver が無ければ「確認が必要」を観測へ戻す（Chat プロファイル互換）。
pub(crate) async fn authorize(
    tool_map: &HashMap<&str, &Arc<dyn Tool>>,
    call: &PendingCall,
    opts: &AgentOptions,
    approver: Option<&dyn Approver>,
    sink: &mut dyn EventSink,
) -> Result<Authz, AgentError> {
    // 未知ツールは execute_tool 側で unknown エラーにするため素通し。
    // egress（ネットワーク）ツールは requires_confirmation=false だが、**自律版では承認ゲート対象**
    // にする（Task 5.6「egress は承認ゲート」）。Chat 版は従来どおり素通し（承認者が無いため）。
    let is_egress = matches!(
        crate::vocab::ToolName::parse(&call.name),
        Some(crate::vocab::ToolName::WebFetch | crate::vocab::ToolName::WebSearch)
    );
    let needs_confirm = tool_map
        .get(call.name.as_str())
        .is_some_and(|t| t.requires_confirmation())
        || (opts.profile.is_autonomous() && is_egress);
    // 実行中モードトグル（#350）: approver が現在ポリシを返すなら、run 開始時のスナップショット
    // （opts.approval）ではなくそれで判定する（各呼び出し直前に問い直す＝緩和も厳格化も即時反映）。
    let refreshed = match approver {
        Some(a) if needs_confirm => a.current_policy().await,
        _ => None,
    };
    let policy = refreshed.as_ref().unwrap_or(&opts.approval);
    if !needs_confirm || policy.is_pre_authorized(&call.name) {
        return Ok(Authz::Proceed);
    }
    let Some(approver) = approver else {
        return Ok(Authz::Reject(format!(
            "tool '{}' requires explicit confirmation and was not executed",
            call.name
        )));
    };

    sink.emit(AgentEvent::ApprovalRequested {
        tool_call_id: call.id.clone(),
        name: call.name.clone(),
        input: call.input.clone(),
        reason: "破壊的/権限/高コストな操作のため承認が必要です".to_string(),
    })
    .await?;
    let decision = approver.decide(&call.id, &call.name, &call.input).await;
    sink.emit(AgentEvent::ApprovalResolved {
        tool_call_id: call.id.clone(),
        approved: decision == ApprovalDecision::Approved,
    })
    .await?;
    Ok(match decision {
        ApprovalDecision::Approved => Authz::Proceed,
        ApprovalDecision::Rejected => Authz::Reject(format!(
            "操作 '{}' はユーザーに却下されました（実行していません）。",
            call.name
        )),
        ApprovalDecision::Cancelled => Authz::Cancel,
    })
}

/// 1 ツール呼び出しを実行する（未知は観測エラーへ・確認は [`authorize`] 済み前提）。
pub(crate) async fn execute_tool(
    tool_map: &HashMap<&str, &Arc<dyn Tool>>,
    ctx: &AuthContext,
    call: &PendingCall,
    trace_id: Option<&str>,
) -> ToolOutcome {
    let Some(tool) = tool_map.get(call.name.as_str()) else {
        return ToolOutcome::error(format!("unknown tool: {}", call.name));
    };
    match tool.call(ctx, call.input.clone(), trace_id).await {
        Ok(o) => o,
        Err(e) => ToolOutcome::error(format!("tool '{}' failed: {e}", call.name)),
    }
}

/// ツール結果イベント（結果・引用・成果物）を外部化する。
pub(crate) async fn emit_tool_events(
    sink: &mut dyn EventSink,
    call: &PendingCall,
    outcome: &ToolOutcome,
) -> Result<(), AgentError> {
    sink.emit(AgentEvent::ToolResult {
        tool_call_id: call.id.clone(),
        ok: !outcome.is_error,
        content: outcome.content.clone(),
    })
    .await?;
    for cite in &outcome.citations {
        sink.emit(AgentEvent::Citation(cite.clone())).await?;
    }
    for artifact in &outcome.artifacts {
        sink.emit(AgentEvent::Artifact {
            tool_call_id: call.id.clone(),
            artifact: artifact.clone(),
        })
        .await?;
    }
    for spec in &outcome.ui_specs {
        sink.emit(AgentEvent::GenerativeUi { spec: spec.clone() })
            .await?;
    }
    for workflow in &outcome.workflow_refs {
        sink.emit(AgentEvent::WorkflowRef {
            workflow: workflow.clone(),
        })
        .await?;
    }
    for note in &outcome.note_refs {
        sink.emit(AgentEvent::NoteRef { note: note.clone() })
            .await?;
    }
    for draft in &outcome.note_drafts {
        sink.emit(AgentEvent::NoteDraft {
            draft: draft.clone(),
        })
        .await?;
    }
    for draft in &outcome.slide_drafts {
        sink.emit(AgentEvent::SlideDraft {
            draft: serde_json::json!({ "name": draft.name, "content": draft.content }),
        })
        .await?;
    }
    for draft in &outcome.csv_drafts {
        sink.emit(AgentEvent::CsvDraft {
            draft: serde_json::json!({ "name": draft.name, "csv": draft.csv }),
        })
        .await?;
    }
    for draft in &outcome.document_drafts {
        sink.emit(AgentEvent::DocumentDraft {
            draft: draft.clone(),
        })
        .await?;
    }
    for edit in &outcome.office_live_edits {
        sink.emit(AgentEvent::OfficeLiveEdit {
            node_id: edit.node_id.clone(),
            html: edit.html.clone(),
        })
        .await?;
    }
    Ok(())
}

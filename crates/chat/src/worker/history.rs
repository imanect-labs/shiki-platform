//! LLM 履歴の組み立てヘルパ（content block → テキスト・プレビュー）。
//!
//! `generate` から切り出した純関数群（責務分離・generate.rs の肥大回避）。追編集に必要な参照
//! （ワークフロー/保存済みノート/下書きの id・name）を観測テキストへ載せ、モデルが「さっきの
//! を直して」に正しく追従できるようにする。

use llm_gateway::Message as LlmMessage;

use crate::model::ContentBlock;

/// content block 列からテキスト（＋添付名・参照メモ）を抽出する（LLM 履歴用）。
pub(super) fn message_text(blocks: &[ContentBlock]) -> String {
    let mut parts = Vec::new();
    for b in blocks {
        match b {
            ContentBlock::Text { text } => parts.push(text.clone()),
            ContentBlock::FileRef { name, .. } => parts.push(format!("[添付: {name}]")),
            // 「さっきのワークフローを直して」等の追編集に id/version が要る（Task 10.13）。
            ContentBlock::WorkflowRef { workflow } => {
                let id = workflow.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let name = workflow.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let version = workflow.get("version").and_then(serde_json::Value::as_i64);
                parts.push(format!(
                    "[保存済みワークフロー: {name}（workflow_id: {id}, v{}）]",
                    version.unwrap_or(0)
                ));
            }
            // 「さっき作ったノートに追記して」等の追編集に node_id が要る（Task 11P.5）。
            ContentBlock::NoteRef { note } => {
                let id = note.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let name = note.get("name").and_then(|v| v.as_str()).unwrap_or("");
                parts.push(format!("[保存済みノート: {name}（node_id: {id}）]"));
            }
            // 「さっきの下書きを直して」等の refine で**同じ name を再利用**させる（下書きキー=名前・#282）。
            // 同名 save_note で同じ下書きが更新され、別名なら別の下書きになる。
            ContentBlock::NoteDraft { draft } => {
                let name = draft.get("name").and_then(|v| v.as_str()).unwrap_or("");
                parts.push(format!(
                    "[作成中の下書きノート: {name}（未保存。直すには同じ name「{name}」で save_note）]"
                ));
            }
            _ => {}
        }
    }
    parts.join("\n")
}

/// LLM メッセージのテキストプレビュー（Langfuse/検索クエリ用）。
pub(super) fn message_preview(m: &LlmMessage) -> String {
    m.content
        .iter()
        .filter_map(|b| match b {
            llm_gateway::Block::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::message_text;
    use crate::model::ContentBlock;

    /// 追編集に要る参照（添付/ワークフロー/保存済みノート/下書き）が観測テキストへ載る。
    #[test]
    fn message_text_surfaces_refs_and_draft_name_for_refine() {
        let blocks = vec![
            ContentBlock::Text {
                text: "本文".into(),
            },
            ContentBlock::FileRef {
                node_id: "n".into(),
                name: "a.pdf".into(),
            },
            ContentBlock::WorkflowRef {
                workflow: serde_json::json!({ "id": "w1", "name": "flow", "version": 2 }),
            },
            ContentBlock::NoteRef {
                note: serde_json::json!({ "id": "no1", "name": "議事録" }),
            },
            ContentBlock::NoteDraft {
                draft: serde_json::json!({ "name": "予算計画", "markdown": "# 予算" }),
            },
        ];
        let out = message_text(&blocks);
        assert!(out.contains("本文"));
        assert!(out.contains("[添付: a.pdf]"));
        assert!(out.contains("workflow_id: w1") && out.contains("v2"));
        assert!(out.contains("node_id: no1"));
        // refine で同名再利用を誘導するため、下書き名が観測テキストに載ること（#282）。
        assert!(
            out.contains("予算計画") && out.contains("save_note"),
            "下書き名と誘導が載る: {out}"
        );
    }
}

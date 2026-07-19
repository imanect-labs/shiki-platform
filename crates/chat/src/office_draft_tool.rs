//! AI 生成 Markdown を Word 文書（.docx）の**下書き**として用意するツール（save_document・#332）。
//!
//! save_note と同型の下書き確定型: この時点では .docx 化も StorageService 書込もしない。
//! フロントは /office/draft を開き、ユーザーが内容を詰めてから「ドライブに保存」で
//! blank.docx テンプレ＋append_markdown（office.edit と同経路）により .docx 化・確定する。

use agent_core::{Tool, ToolError, ToolName, ToolOutcome};
use authz::AuthContext;
use serde::Deserialize;

/// AI 生成 md を**下書き Word 文書**として用意するツール（document_draft カード化・#332）。
///
/// 下書きは**会話内で name をキーに識別**する: 同じ name で呼び直すと同じ下書きが更新され、
/// 別 name なら別の下書きになる（note/slide/csv と同じ状態機械）。ストレージ書込を伴わない
/// ため確認ゲートは不要（確定は UI の保存ボタンが担う・fail-closed はそちら）。
pub struct SaveDocumentTool;

impl SaveDocumentTool {
    pub fn new() -> Self {
        SaveDocumentTool
    }
}

impl Default for SaveDocumentTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct SaveDocumentInput {
    /// 文書名（`.docx` は自動付与）。下書きの識別キーも兼ねる。
    name: String,
    /// 本文 md（.docx 化は確定保存時にサーバ側で行う）。
    markdown: String,
}

#[async_trait::async_trait]
impl Tool for SaveDocumentTool {
    fn name(&self) -> &str {
        ToolName::SaveDocument.as_str()
    }
    fn description(&self) -> &'static str {
        "会話で生成した内容を新しい Word 文書（.docx）の下書きとして用意する。ユーザーが\
         「Word で〜を作って」「docx にして」等と依頼したときに使う。呼ぶと下書き文書画面が\
         開き、ユーザーはそこで内容を確認・編集してから自分で「ドライブに保存」して確定する\
         （このツールは保存しない）。内容を直す場合は**同じ name で呼び直す**と同じ下書きが\
         更新される。別の文書を同時に作る場合は別の name で呼ぶ。"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "文書名（.docx は自動付与）。同じ name で呼び直すと同じ下書きを更新する" },
                "markdown": { "type": "string", "description": "文書本文の Markdown（見出し・箇条書き・段落）" }
            },
            "required": ["name", "markdown"]
        })
    }
    /// 下書きは StorageService へ書かない（確定は UI の保存ボタン）。承認ゲートは不要。
    fn requires_confirmation(&self) -> bool {
        false
    }

    async fn call(
        &self,
        _ctx: &AuthContext,
        input: serde_json::Value,
        _trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let input: SaveDocumentInput = serde_json::from_value(input)
            .map_err(|e| ToolError::Invalid(format!("入力が不正です: {e}")))?;
        let name = input.name.trim();
        if name.is_empty() {
            return Err(ToolError::Invalid("文書名を指定してください".into()));
        }
        // 表示名は .docx（大文字小文字問わず）を落として持つ（下書きカード/画面のタイトル用）。
        // 拡張子だけの名前（".docx" 等）は空表示名になるためここで弾く（イベント/URL 破綻を防ぐ）。
        let display_name = name
            .strip_suffix(".docx")
            .or_else(|| name.strip_suffix(".DOCX"))
            .unwrap_or(name)
            .trim();
        if display_name.is_empty() {
            return Err(ToolError::Invalid("文書名を指定してください".into()));
        }
        // 下書き本文も正規化する（生 HTML はコードブロックへ縮退＝XSS 遮断・Task 11P.6）。
        let markdown = collab::note::normalize_markdown(&input.markdown);
        let mut outcome = ToolOutcome::ok(format!(
            "下書き Word 文書「{display_name}」を用意しました。画面で内容を確認・編集し、\
             「ドライブに保存」で確定してください。"
        ));
        outcome.document_drafts.push(serde_json::json!({
            "name": display_name,
            "markdown": markdown,
        }));
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use agent_core::Tool;

    fn ctx() -> authz::AuthContext {
        authz::AuthContext::new(
            authz::Principal {
                kind: authz::PrincipalKind::User,
                id: "alice".into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: None,
            },
            "acme".into(),
            "default".into(),
        )
    }

    /// save_document は保存せず下書き（document_draft）を出す・確認ゲートは不要（#332）。
    #[tokio::test]
    async fn save_document_emits_draft_without_writing() {
        let tool = super::SaveDocumentTool::new();
        assert!(!tool.requires_confirmation(), "下書きは確認ゲート不要");
        let out = tool
            .call(
                &ctx(),
                serde_json::json!({ "name": "提案書", "markdown": "# 提案\n\n本文" }),
                None,
            )
            .await
            .expect("call");
        assert!(!out.is_error);
        assert!(out.note_drafts.is_empty(), "note_draft ではない");
        assert_eq!(out.document_drafts.len(), 1, "下書きを 1 件出す");
        assert_eq!(out.document_drafts[0]["name"], "提案書");
        assert!(out.document_drafts[0]["markdown"]
            .as_str()
            .unwrap()
            .contains("# 提案"));
    }

    /// 下書き名は .docx を落として持つ（画面/カードのタイトル用）。
    #[tokio::test]
    async fn save_document_strips_docx_suffix_from_name() {
        let out = super::SaveDocumentTool::new()
            .call(
                &ctx(),
                serde_json::json!({ "name": "議事録.docx", "markdown": "本文" }),
                None,
            )
            .await
            .expect("call");
        assert_eq!(out.document_drafts[0]["name"], "議事録");
    }

    /// 空名はエラー（モデルに名前指定を促す）。
    #[tokio::test]
    async fn save_document_rejects_empty_name() {
        let err = super::SaveDocumentTool::new()
            .call(
                &ctx(),
                serde_json::json!({ "name": "  ", "markdown": "本文" }),
                None,
            )
            .await;
        assert!(matches!(err, Err(agent_core::ToolError::Invalid(_))));
    }

    /// 拡張子だけの名前（.docx / .DOCX）は空表示名になるため弾く。
    #[tokio::test]
    async fn save_document_rejects_extension_only_name() {
        for name in [".docx", ".DOCX", "  .docx  "] {
            let err = super::SaveDocumentTool::new()
                .call(
                    &ctx(),
                    serde_json::json!({ "name": name, "markdown": "本文" }),
                    None,
                )
                .await;
            assert!(
                matches!(err, Err(agent_core::ToolError::Invalid(_))),
                "{name:?} は拒否されること"
            );
        }
    }

    /// 生 HTML はコードブロックへ縮退する（XSS 遮断・note と同じ正規化）。
    #[tokio::test]
    async fn save_document_normalizes_raw_html() {
        let out = super::SaveDocumentTool::new()
            .call(
                &ctx(),
                serde_json::json!({ "name": "x", "markdown": "<script>alert(1)</script>" }),
                None,
            )
            .await
            .expect("call");
        let md = out.document_drafts[0]["markdown"].as_str().unwrap();
        assert!(!md.starts_with("<script>"), "生 HTML をそのまま残さない");
    }
}

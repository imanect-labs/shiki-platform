//! 開いている Office 文書セッションへの AI ライブ編集ツール（office.live_edit・#328）。
//!
//! `office.edit`（ファイルを worker で編集し新/提案バージョン保存）と違い、**ファイルは
//! 書き換えない**。開いている Collabora 編集セッションの**現在の選択範囲**を指定 HTML で
//! 置き換えるようフロントへ指示するだけ（フロントが Collabora の `Action_Paste` を実行）。
//! セッション内へ注入するため CoolWSD 協調プロトコル経由で全参加者へ即反映し、ファイルレベル
//! 編集で起きる版競合（開いているセッションが閉じる際に編集前内容を保存し上書きする問題）を
//! 回避する。
//!
//! 認可: 発話ユーザーの `editor@file` を毎回 OpenFGA（HigherConsistency）で判定する
//! （confused-deputy 回避・昇格しない・共有解除即時＝PIT-11）。権限なし/未検出は同一メッセージに
//! 畳む（存在秘匿・#326）。HTML は emit 前にサニタイズする（PIT-40・ammonia 許可リスト）。

use std::sync::Arc;

use agent_core::{OfficeLiveEdit as OfficeLiveEditOut, Tool, ToolError, ToolName, ToolOutcome};
use authz::{AuthContext, AuthzClient, Consistency, Relation};
use serde::Deserialize;
use uuid::Uuid;

/// 開いている Office セッションへライブ編集を注入するツール。
pub struct OfficeLiveEditTool {
    authz: Arc<dyn AuthzClient>,
}

impl OfficeLiveEditTool {
    pub fn new(authz: Arc<dyn AuthzClient>) -> Self {
        OfficeLiveEditTool { authz }
    }
}

#[derive(Debug, Deserialize)]
struct LiveEditInput {
    /// 対象 Office ファイル（.docx/.xlsx/.pptx）のノード ID。
    node_id: Uuid,
    /// 現在の選択範囲を置き換える HTML（段落・箇条書き・見出し・強調などの最小サブセット）。
    html: String,
}

#[async_trait::async_trait]
impl Tool for OfficeLiveEditTool {
    fn name(&self) -> &str {
        ToolName::OfficeLiveEdit.as_str()
    }

    fn description(&self) -> &'static str {
        "開いている Office 文書（Word/Excel/PowerPoint を Collabora で編集中）の**現在の選択範囲**を、\
         指定した HTML で置き換える。全参加者の編集画面へ即座に反映される（セッション内ライブ編集）。\
         ユーザーが選択した箇所を書き換える依頼に使う。文書を開いていない編集や、構造的なファイル\
         編集（セルの一括設定・シート/スライド追加など）は office.edit を使うこと。html は段落・\
         箇条書き・見出し・強調など最小の書式のみ対応（script/style 等は自動で除去される）。"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "node_id": { "type": "string", "format": "uuid", "description": "対象 Office ファイルのノード ID" },
                "html": { "type": "string", "description": "現在の選択範囲を置き換える HTML（最小書式）" }
            },
            "required": ["node_id", "html"]
        })
    }

    /// 文書を書き換える破壊的操作のため確認対象（承認ゲート・human-in-the-loop）。
    fn requires_confirmation(&self) -> bool {
        true
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        _trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let input: LiveEditInput = serde_json::from_value(input)
            .map_err(|e| ToolError::Invalid(format!("入力が不正です: {e}")))?;

        // 実行主体の editor@file を毎回再判定する（confused-deputy 回避・共有解除即時・PIT-11）。
        let object = ctx.ns().file(&input.node_id.to_string());
        let allowed = self
            .authz
            .check(
                &ctx.subject(),
                Relation::Editor,
                &object,
                Consistency::HigherConsistency,
            )
            .await
            .map_err(|e| ToolError::Internal(format!("認可判定に失敗しました: {e}")))?;
        if !allowed {
            // 読める/存在するかも明かさない（存在秘匿・#326・API 層の 404 統一と同契約）。
            return Ok(ToolOutcome::error(
                "指定された文書にアクセスできません（存在しないか、権限がありません）。",
            ));
        }

        // 描画面（Collabora）へ注入する前にサニタイズする（PIT-40・ammonia 許可リスト）。
        let sanitized = collab::slide::sanitize::sanitize_html(&input.html);
        if sanitized.trim().is_empty() {
            return Ok(ToolOutcome::error(
                "置き換える内容が空でした（HTML がサニタイズで全て除去されました）。",
            ));
        }

        let mut outcome = ToolOutcome::ok(
            "開いている文書の選択範囲をライブで置き換えました（全参加者の編集画面に反映されます）。",
        );
        outcome.office_live_edits.push(OfficeLiveEditOut {
            node_id: input.node_id.to_string(),
            html: sanitized,
        });
        Ok(outcome)
    }
}

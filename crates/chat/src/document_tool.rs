//! AI ノート共同編集ツール（document.edit / document.read・Task 11P.4）。
//!
//! エージェントは**共同編集参加者**としてノート（Yjs）を編集する。人間と同じ
//! `editor@file` 権限で判定し（confused-deputy 回避・昇格しない）、編集は共有 Yjs
//! ドキュメントへ適用されて人間の並行編集と収束する。ファイル直接上書きの経路は作らない
//! （collab ハブ経由のみ）。既定は直接適用、`mode="suggest"` で提案マーク付与に切替。

use std::sync::Arc;

use agent_core::{Tool, ToolError, ToolName, ToolOutcome};
use authz::AuthContext;
use collab::CollabHub;
use serde::Deserialize;
use storage::StorageService;
use uuid::Uuid;

/// collab のエラーを**モデルが読む error 観測**へ写す（fail-closed・存在秘匿）。
///
/// 権限不足・存在しないは同じ「編集できない」メッセージに畳み、実行主体の権限を
/// 越える編集ができないことをモデルに伝える（confused-deputy 回避・情報を漏らさない）。
fn denied_outcome(err: &collab::CollabError) -> ToolOutcome {
    use collab::CollabError as CE;
    let msg = match err {
        CE::Forbidden(_) | CE::Authz(_) | CE::Storage(storage::StorageError::Forbidden) => {
            "このノートを編集する権限がありません（editor 権限が必要です）。"
        }
        CE::NotFound(_) | CE::Storage(storage::StorageError::NotFound) => {
            "指定されたノートが見つかりません。"
        }
        _ => "ノート編集に失敗しました。",
    };
    ToolOutcome::error(msg)
}

/// ノートの現在の md を読むツール（編集前の把握に使う）。
pub struct DocumentReadTool {
    collab: Arc<CollabHub>,
    storage: Arc<StorageService>,
}

impl DocumentReadTool {
    pub fn new(collab: Arc<CollabHub>, storage: Arc<StorageService>) -> Self {
        DocumentReadTool { collab, storage }
    }
}

#[derive(Debug, Deserialize)]
struct ReadInput {
    /// 対象ノート（.md ファイル）の ID。
    node_id: Uuid,
}

#[async_trait::async_trait]
impl Tool for DocumentReadTool {
    fn name(&self) -> &str {
        ToolName::DocumentRead.as_str()
    }
    fn description(&self) -> &'static str {
        "ノート（.md ファイル）の現在の内容を正規化 Markdown で読み取る。document.edit で\
         編集する前に、見出し構成や既存内容を把握するために使う。"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "node_id": { "type": "string", "format": "uuid", "description": "ノートのノード ID" }
            },
            "required": ["node_id"]
        })
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let input: ReadInput = serde_json::from_value(input)
            .map_err(|e| ToolError::Invalid(format!("入力が不正です: {e}")))?;
        let node = match self
            .storage
            .get_metadata(ctx, input.node_id, trace_id)
            .await
        {
            Ok(node) => node,
            Err(e) => return Ok(denied_outcome(&collab::CollabError::Storage(e))),
        };
        match self.collab.read_note_markdown(ctx, &node).await {
            Ok(markdown) => Ok(ToolOutcome::ok(format!(
                "# ノート「{}」の現在の内容\n\n{markdown}",
                node.name
            ))),
            Err(e) => Ok(denied_outcome(&e)),
        }
    }
}

/// AI 生成 md を**下書きノート**として用意するツール（note_draft カード化・issue #282）。
///
/// 「ドキュメントを作って」等の依頼に対し、本文 md を**下書き**として返す（この時点では
/// StorageService へは作らない）。フロントは下書きノート画面を開き、ユーザーがそこで AI と
/// 内容を詰めてから、画面右上「ドライブに保存」を押して初めてノートを実体化する（下書き→確定
/// の状態機械は**新規作成パスのみ**・既存ノートの編集は document.edit のライブ編集）。
///
/// 下書きは**会話内で name をキーに識別**する: 同じ name で呼び直すと同じ下書きが更新され、
/// 別 name なら別の下書きになる（1 会話で複数の下書きを並行して詰められる）。ストレージ書込を
/// 伴わないため確認ゲートは不要（確定は UI の保存ボタンが担う・fail-closed はそちら）。
pub struct SaveNoteTool;

impl SaveNoteTool {
    pub fn new() -> Self {
        SaveNoteTool
    }
}

impl Default for SaveNoteTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct SaveNoteInput {
    /// ノート名（`.md` は自動付与）。下書きの識別キーも兼ねる。
    name: String,
    /// 本文 md。
    markdown: String,
}

#[async_trait::async_trait]
impl Tool for SaveNoteTool {
    fn name(&self) -> &str {
        ToolName::SaveNote.as_str()
    }
    fn description(&self) -> &'static str {
        "会話で生成した内容を新しいノートの下書きとして用意する。ユーザーが「〜のドキュメント\
         を作って」「ノートにして」等と依頼したときに使う。呼ぶと下書きノート画面が開き、ユーザー\
         はそこで内容を確認・編集してから自分で「ドライブに保存」して確定する（このツールは保存\
         しない）。内容を直す場合は**同じ name で呼び直す**と同じ下書きが更新される。別の文書を\
         同時に作る場合は別の name で呼ぶ。"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "ノート名（.md は自動付与）。同じ name で呼び直すと同じ下書きを更新する" },
                "markdown": { "type": "string", "description": "ノート本文の Markdown" }
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
        let input: SaveNoteInput = serde_json::from_value(input)
            .map_err(|e| ToolError::Invalid(format!("入力が不正です: {e}")))?;
        let name = input.name.trim();
        if name.is_empty() {
            return Err(ToolError::Invalid("ノート名を指定してください".into()));
        }
        // 表示名は .md を落として持つ（下書きカード/画面のタイトル用）。保存時に付与する。
        let display_name = name.strip_suffix(".md").unwrap_or(name);
        // 下書き本文も正規化する（生 HTML はコードブロックへ縮退＝XSS 遮断・Task 11P.6）。
        let markdown = collab::note::normalize_markdown(&input.markdown);
        let mut outcome = ToolOutcome::ok(format!(
            "下書きノート「{display_name}」を用意しました。画面で内容を確認・編集し、\
             「ドライブに保存」で確定してください。"
        ));
        outcome.note_drafts.push(serde_json::json!({
            "name": display_name,
            "markdown": markdown,
        }));
        Ok(outcome)
    }
}

/// ノートを編集するツール（共同編集参加者として Yjs へ適用）。
pub struct DocumentEditTool {
    collab: Arc<CollabHub>,
    storage: Arc<StorageService>,
}

impl DocumentEditTool {
    pub fn new(collab: Arc<CollabHub>, storage: Arc<StorageService>) -> Self {
        DocumentEditTool { collab, storage }
    }
}

#[derive(Debug, Deserialize)]
struct EditInput {
    /// 対象ノート（.md ファイル）の ID。
    node_id: Uuid,
    /// 適用モード（既定 direct・suggest で提案マーク付与）。
    #[serde(default)]
    mode: collab::note::EditMode,
    /// 編集操作列（順に適用）。
    ops: Vec<collab::note::EditOp>,
}

#[async_trait::async_trait]
impl Tool for DocumentEditTool {
    fn name(&self) -> &str {
        ToolName::DocumentEdit.as_str()
    }
    fn description(&self) -> &'static str {
        "ノート（.md ファイル）を共同編集参加者として編集する。操作は Markdown で内容を\
         指定する: append（末尾追記）/ replace_section（見出し名で節本文を置換）/ \
         insert_after_heading（見出し直後に挿入）/ replace_all（全置換）/ set_meta（\
         タイトル・タグ等のプロパティ設定）。既定は直接適用（あなたの名義で反映）。\
         mode=\"suggest\" で提案として挿入し、人間が承認/棄却できる。グラフ等の genui\
         コンポーネントを本文に入れたいときは document.embed を使う。"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "node_id": { "type": "string", "format": "uuid", "description": "ノートのノード ID" },
                "mode": {
                    "type": "string", "enum": ["direct", "suggest"], "default": "direct",
                    "description": "direct=直接適用（既定）/ suggest=提案マーク付き"
                },
                "ops": {
                    "type": "array",
                    "description": "編集操作列（順に適用）",
                    "items": {
                        "type": "object",
                        "properties": {
                            "op": {
                                "type": "string",
                                "enum": ["append", "replace_section", "insert_after_heading", "replace_all", "set_meta"]
                            },
                            "markdown": { "type": "string", "description": "挿入/置換する Markdown（set_meta 以外）" },
                            "heading": { "type": "string", "description": "対象見出しテキスト（replace_section / insert_after_heading）" },
                            "key": { "type": "string", "description": "プロパティ名（set_meta・title/icon/tags/任意）" },
                            "value": { "type": "string", "description": "プロパティ値（set_meta・tags は , 区切り）" }
                        },
                        "required": ["op"]
                    }
                }
            },
            "required": ["node_id", "ops"]
        })
    }

    /// 破壊的（既存内容を書き換え得る）ため確認対象。承認ゲートの対象になる。
    fn requires_confirmation(&self) -> bool {
        true
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let input: EditInput = serde_json::from_value(input)
            .map_err(|e| ToolError::Invalid(format!("入力が不正です: {e}")))?;
        if input.ops.is_empty() {
            return Err(ToolError::Invalid("ops が空です".into()));
        }
        let node = match self
            .storage
            .get_metadata(ctx, input.node_id, trace_id)
            .await
        {
            Ok(node) => node,
            Err(e) => return Ok(denied_outcome(&collab::CollabError::Storage(e))),
        };
        let report = match self
            .collab
            .apply_ai_edit(ctx, &node, &input.ops, input.mode)
            .await
        {
            Ok(report) => report,
            Err(e) => return Ok(denied_outcome(&e)),
        };

        let mode_label = match input.mode {
            collab::note::EditMode::Direct => "直接適用",
            collab::note::EditMode::Suggest => "提案",
        };
        use std::fmt::Write as _;
        let mut content = format!(
            "ノート「{}」を編集しました（{mode_label}・{} 件適用）。",
            node.name, report.applied
        );
        if !report.skipped.is_empty() {
            let _ = write!(
                content,
                "\n次の操作は対象が見つからずスキップしました: {}",
                report.skipped.join(", ")
            );
        }
        let outcome = if report.applied == 0 {
            ToolOutcome::error(content)
        } else {
            ToolOutcome::ok(content)
        };
        Ok(outcome)
    }
}

/// ノート本文にグラフ等の genui コンポーネントを埋め込むツール（issue #282）。
///
/// document.edit と同じ共同編集経路（editor@file・昇格しない）で、本文末尾に検証済み genui
/// スペックの埋め込みブロックを **append**（非破壊）する。追記のみ・spec は描画時に fail-closed
/// 検証されるため確認ゲートは不要（＝「AI が裁量で自動挿入」を確認カード無しで実現）。
pub struct DocumentEmbedTool {
    collab: Arc<CollabHub>,
    storage: Arc<StorageService>,
}

impl DocumentEmbedTool {
    pub fn new(collab: Arc<CollabHub>, storage: Arc<StorageService>) -> Self {
        DocumentEmbedTool { collab, storage }
    }
}

#[derive(Debug, Deserialize)]
struct EmbedInput {
    /// 対象ノート（.md ファイル）の ID。
    node_id: Uuid,
    /// genui スペック（emit_ui と同じ検証済みスペック）。
    spec: serde_json::Value,
}

#[async_trait::async_trait]
impl Tool for DocumentEmbedTool {
    fn name(&self) -> &str {
        ToolName::DocumentEmbed.as_str()
    }
    fn description(&self) -> &'static str {
        "ノート（.md ファイル）の本文末尾にグラフ等の genui コンポーネントを埋め込む。ユーザーが\
         「このグラフをノートに入れて」「図をノートに追加して」等と依頼したときに使う。spec は\
         emit_ui と同じ検証済み genui スペック。追記のみで既存内容は消さない。"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "node_id": { "type": "string", "format": "uuid", "description": "ノートのノード ID" },
                "spec": { "type": "object", "description": "genui スペック（emit_ui と同じ検証済みスペック）" }
            },
            "required": ["node_id", "spec"]
        })
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let input: EmbedInput = serde_json::from_value(input)
            .map_err(|e| ToolError::Invalid(format!("入力が不正です: {e}")))?;
        let node = match self
            .storage
            .get_metadata(ctx, input.node_id, trace_id)
            .await
        {
            Ok(node) => node,
            Err(e) => return Ok(denied_outcome(&collab::CollabError::Storage(e))),
        };
        let ops = [collab::note::EditOp::InsertEmbed { spec: input.spec }];
        let report = match self
            .collab
            .apply_ai_edit(ctx, &node, &ops, collab::note::EditMode::Direct)
            .await
        {
            Ok(report) => report,
            Err(e) => return Ok(denied_outcome(&e)),
        };
        if report.applied == 0 {
            return Ok(ToolOutcome::error(
                "埋め込みを挿入できませんでした（スペックが不正な可能性があります）。",
            ));
        }
        Ok(ToolOutcome::ok(format!(
            "ノート「{}」にグラフを埋め込みました。",
            node.name
        )))
    }
}

#[cfg(test)]
mod tests {
    //! collab error → モデル観測メッセージの写像（`denied_outcome`）を検証する。
    use super::denied_outcome;
    use collab::CollabError as CE;

    #[test]
    fn forbidden_family_maps_to_permission_message() {
        for e in [
            CE::Forbidden("x".into()),
            CE::Authz(authz::AuthzError::InvalidModel("m".into())),
            CE::Storage(storage::StorageError::Forbidden),
        ] {
            let o = denied_outcome(&e);
            assert!(o.is_error);
            assert!(o.content.contains("権限がありません"), "got: {}", o.content);
        }
    }

    #[test]
    fn not_found_family_maps_to_not_found_message() {
        for e in [
            CE::NotFound("x".into()),
            CE::Storage(storage::StorageError::NotFound),
        ] {
            assert!(denied_outcome(&e).content.contains("見つかりません"));
        }
    }

    #[test]
    fn other_errors_fall_back_to_generic_message() {
        let o = denied_outcome(&CE::InvalidUpdate("bad yjs".into()));
        assert!(o.is_error);
        assert_eq!(o.content, "ノート編集に失敗しました。");
    }

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

    /// save_note は保存せず下書き（note_draft）を出す・確認ゲートは不要（issue #282）。
    #[tokio::test]
    async fn save_note_emits_draft_without_writing() {
        use agent_core::Tool;
        let tool = super::SaveNoteTool::new();
        assert!(!tool.requires_confirmation(), "下書きは確認ゲート不要");
        let out = tool
            .call(
                &ctx(),
                serde_json::json!({ "name": "予算計画", "markdown": "# 予算\n\n本文" }),
                None,
            )
            .await
            .expect("call");
        assert!(!out.is_error);
        assert!(
            out.note_refs.is_empty(),
            "保存はしない（note_ref を出さない）"
        );
        assert_eq!(out.note_drafts.len(), 1, "下書きを 1 件出す");
        assert_eq!(out.note_drafts[0]["name"], "予算計画");
        assert!(out.note_drafts[0]["markdown"]
            .as_str()
            .unwrap()
            .contains("# 予算"));
    }

    /// 下書き名は .md を落として持つ（画面/カードのタイトル用）。
    #[tokio::test]
    async fn save_note_strips_md_suffix_from_name() {
        use agent_core::Tool;
        let out = super::SaveNoteTool::new()
            .call(
                &ctx(),
                serde_json::json!({ "name": "議事録.md", "markdown": "本文" }),
                None,
            )
            .await
            .expect("call");
        assert_eq!(out.note_drafts[0]["name"], "議事録");
    }

    /// 空名はエラー（モデルに名前指定を促す）。
    #[tokio::test]
    async fn save_note_rejects_empty_name() {
        use agent_core::Tool;
        let err = super::SaveNoteTool::new()
            .call(
                &ctx(),
                serde_json::json!({ "name": "  ", "markdown": "本文" }),
                None,
            )
            .await;
        assert!(matches!(err, Err(agent_core::ToolError::Invalid(_))));
    }
}

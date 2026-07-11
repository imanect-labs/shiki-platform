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

/// AI 生成 md を新規ノートとして保存するツール（note_ref カード化・Task 11P.5）。
///
/// チャットで生成した内容を「ノートとして保存」する経路。作成は StorageService の内部書込
/// （発話ユーザーの AuthContext・認可・監査・書込イベント→RAG 再索引）を通り、初期 md は
/// 正規化して保存する（生 HTML は縮退・XSS 遮断）。返り値は note_ref（chat がカード化）。
pub struct SaveNoteTool {
    storage: Arc<StorageService>,
}

impl SaveNoteTool {
    pub fn new(storage: Arc<StorageService>) -> Self {
        SaveNoteTool { storage }
    }
}

#[derive(Debug, Deserialize)]
struct SaveNoteInput {
    /// ノート名（`.md` は自動付与）。
    name: String,
    /// 本文 md。
    markdown: String,
    /// 配置先フォルダ（省略時は org ルート直下）。
    #[serde(default)]
    parent_id: Option<Uuid>,
}

#[async_trait::async_trait]
impl Tool for SaveNoteTool {
    fn name(&self) -> &str {
        ToolName::SaveNote.as_str()
    }
    fn description(&self) -> &'static str {
        "会話で生成した内容を新しいノート（.md ファイル）として保存する。ユーザーが\
         「ノートとして保存して」等と依頼したときに使う。保存後は参照カードが表示され、\
         ユーザーはノートページを開いて共同編集できる。"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "ノート名（.md は自動付与）" },
                "markdown": { "type": "string", "description": "ノート本文の Markdown" },
                "parent_id": { "type": "string", "format": "uuid", "description": "配置先フォルダ ID（省略で直下）" }
            },
            "required": ["name", "markdown"]
        })
    }
    fn requires_confirmation(&self) -> bool {
        true
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let input: SaveNoteInput = serde_json::from_value(input)
            .map_err(|e| ToolError::Invalid(format!("入力が不正です: {e}")))?;
        let name = input.name.trim();
        if name.is_empty() {
            return Err(ToolError::Invalid("ノート名を指定してください".into()));
        }
        let file_name = if collab::note::is_note_file(name) {
            name.to_string()
        } else {
            format!("{name}.md")
        };
        // 初期 md は正規化して保存する（生 HTML はコードブロックへ縮退＝XSS 遮断・Task 11P.6）。
        let markdown = collab::note::normalize_markdown(&input.markdown);
        let node = match self
            .storage
            .write_file_internal(
                ctx,
                input.parent_id,
                &file_name,
                markdown.as_bytes(),
                "text/markdown",
                trace_id,
            )
            .await
        {
            Ok(node) => node,
            Err(storage::StorageError::Forbidden) => {
                return Ok(ToolOutcome::error("ノートを作成する権限がありません。"))
            }
            Err(e) => return Err(ToolError::Internal(format!("ノート作成に失敗: {e}"))),
        };
        let mut outcome = ToolOutcome::ok(format!(
            "ノート「{}」を保存しました。参照カードから開いて共同編集できます。",
            node.name
        ));
        outcome.note_refs.push(serde_json::json!({
            "id": node.id.to_string(),
            "name": node.name,
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
         mode=\"suggest\" で提案として挿入し、人間が承認/棄却できる。"
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

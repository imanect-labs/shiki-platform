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
        // 権限なしと未検出は同一メッセージに畳む（存在秘匿・API 層の 404 統一と同じ契約）。
        CE::Forbidden(_)
        | CE::Authz(_)
        | CE::NotFound(_)
        | CE::Storage(storage::StorageError::Forbidden | storage::StorageError::NotFound) => {
            "指定されたノートにアクセスできません（存在しないか、権限がありません）。"
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
        "ノート（.md ファイル）を共同編集参加者として編集する。人が同時に書いている箇所を\
         壊さないよう、**変える所だけを変える一番小さい操作**を選ぶ。\
         語句の訂正・言い換え・見出しの改名・文や項目の削除は replace_text（find の語句を replace に。\
         空にすると段落・項目ごと消える）。段落・リスト・表を書式付きで書き直すなら replace_block\
         （find を含む最上位のブロックを置換。リストの項目の中で同じ種類のリストを渡すとその項目だけ\
         を置換＝チェックの付け外しもこれ）。段落・項目の後ろに足すなら insert_after（リストの項目の\
         後ろに同じ種類のリストを渡すと同じリストに続く）。find は本文の文字どおりで、文書全体で一意\
         にする（重なるなら heading で節を絞る）。1 つの段落・項目・表のセルの中の語句に限る（表は\
         行ではなくセルの文字を渡す）。節単位は replace_section（見出しを残して本文を置換）・\
         insert_after_heading・delete_section（見出しごと消す）・move_section（before_heading の前へ。\
         省略で末尾）。append は末尾に追記、replace_all は全文の作り直し（最終手段）、set_meta は\
         タイトル・タグ等。既定は直接適用（あなたの名義で反映）。mode=\"suggest\" で提案として入れ、\
         人間が承認/棄却できる。ops は全部適用されるか何も適用されないかのどちらかで、適用できない\
         ときは理由が返るので、直して ops 全体を送り直す。\
         グラフ等の図は markdown に ```ui ブロックとして書く（末尾に図だけ足すなら document.embed）。"
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
                                "enum": [
                                    "replace_text", "replace_block", "insert_after",
                                    "replace_section", "insert_after_heading", "delete_section",
                                    "move_section", "append", "replace_all", "set_meta"
                                ]
                            },
                            "find": { "type": "string", "description": "場所を決める本文の語句（replace_text / replace_block / insert_after）。文書全体で一意に" },
                            "replace": { "type": "string", "description": "差し替える文字（replace_text・書式記号は使えない・空で削除）" },
                            "markdown": { "type": "string", "description": "入れる/置換する Markdown（replace_block / insert_after / replace_section / insert_after_heading / append / replace_all）" },
                            "heading": { "type": "string", "description": "対象の見出しの文字（節単位の操作）。find を使う操作では、探す範囲をその節に絞る。同じ見出しが複数あるときは「親の見出し > 見出し」" },
                            "before_heading": { "type": "string", "description": "move_section の移動先（この見出しの前へ。省略で末尾）" },
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
        // 適用は全部か無しか（#886）。1 つでも適用できなければ何も変えずに理由を返す
        // （成功と言い切らせない・直した ops 全体を送り直せば二重にならない）。
        if !report.skipped.is_empty() || report.applied == 0 {
            return Ok(ToolOutcome::error(format!(
                "ノート「{}」は変更していません（操作は全部適用するか、何も適用しないかのどちらか）。\
                 次の理由を直して、ops 全体を送り直す:\n- {}",
                node.name,
                report.skipped.join("\n- ")
            )));
        }
        // 編集結果をチャットに残す（#381）。Yjs へ適用した時点で共有ドキュメントは変わって
        // いるが、版はセッションの保存契機で進むためここでは載せない（嘘の版を出さない）。
        let mut outcome = ToolOutcome::ok(format!(
            "ノート「{}」を編集しました（{mode_label}・{} 件適用）。",
            node.name, report.applied
        ));
        outcome
            .document_refs
            .push(crate::document_ref::payload(node.id, &node.name, None));
        Ok(outcome)
    }
}

/// ノート本文に Yui の図（```ui）を入れるツール（issue #282・#604）。
///
/// document.edit と同じ共同編集経路（editor@file・昇格しない）で、本文末尾に ```ui の Markdown を
/// **append**（非破壊）する。追記のみ・図は描画時に fail-closed で読まれるため確認ゲートは不要
/// （＝「AI が裁量で自動挿入」を確認カード無しで実現）。ツール名はスキルの `allowed_tools` に
/// 保存されているので、旧 genui スペックからの置き換えでも名前は変えない（#604）。
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
    /// 図の記述（```ui ブロックの中身。フェンスごと渡されても外す）。
    source: String,
}

/// 図の記述の上限（ノート 1 つの図としては十分に大きい）。
const MAX_FIGURE_BYTES: usize = 16 * 1024;

/// 渡されたフェンスの情報文字列を、追記するフェンスの言語にする（`ui bar`・`UI:Bar`・`yui bar`
/// → `ui:bar`。種類の無い `ui`・`yui`・空は `ui`）。ノートの画面（`normalizeYuiInfo`）と同じ形を
/// 受け、種類の名前の正規化（別名 → 正規名）は画面側に任せる。
fn fence_info(rest: &str) -> Result<String, &'static str> {
    let rest = rest.trim();
    if rest.is_empty() {
        return Ok("ui".into());
    }
    let lower = rest.to_lowercase();
    let tail = ["yui", "ui"]
        .iter()
        .find_map(|p| lower.strip_prefix(p))
        .ok_or("```ui の図の記述を渡してください。")?;
    let kind = match tail.strip_prefix(':') {
        Some(k) => k,
        None if tail.is_empty() => return Ok("ui".into()),
        None if tail.starts_with(char::is_whitespace) => tail.trim_start(),
        None => return Err("```ui の図の記述を渡してください。"),
    };
    let kind = kind.split_whitespace().next().unwrap_or_default();
    if kind.is_empty() {
        return Ok("ui".into());
    }
    if kind.contains(['`', '~']) {
        return Err("図の種類の指定が不正です。");
    }
    Ok(format!("ui:{kind}"))
}

/// 図の記述を、ノートへ追記する ```ui の Markdown にする（不正なら理由を返す）。
///
/// ノートの図は表示だけ（ボタンを押す先が無い）なので、操作を持つ種類（app・ask・plan）は拒む。
/// 記述の中のフェンスはブロックを閉じて本文へ漏れるので拒む。
fn figure_markdown(source: &str) -> Result<String, &'static str> {
    let mut lines: Vec<&str> = source.trim().lines().collect();
    let mut info = "ui".to_string();
    if let Some(first) = lines.first() {
        let open = first.trim();
        if let Some(rest) = open
            .strip_prefix("```")
            .or_else(|| open.strip_prefix("~~~"))
        {
            info = fence_info(rest)?;
            lines.remove(0);
            if lines.last().is_some_and(|l| {
                let l = l.trim();
                l.starts_with("```") || l.starts_with("~~~")
            }) {
                lines.pop();
            }
        }
    }
    let body = lines.join("\n");
    let body = body.trim_matches('\n');
    if body.trim().is_empty() {
        return Err("図の記述が空です。");
    }
    if body.len() > MAX_FIGURE_BYTES {
        return Err("図の記述が大きすぎます（16KB まで）。");
    }
    if body.lines().any(|l| {
        let l = l.trim_start();
        l.starts_with("```") || l.starts_with("~~~")
    }) {
        return Err("図の記述にフェンス（``` や ~~~）を含めないでください。");
    }
    let hint = info.strip_prefix("ui:").and_then(yui::resolve_kind);
    if hint.is_some() || yui::parse_block(body, None).figure.is_some() {
        return Err("ノートの図は表示だけです。ボタンを持つ app・ask・plan は入れられません。");
    }
    Ok(format!("```{info}\n{body}\n```\n"))
}

#[async_trait::async_trait]
impl Tool for DocumentEmbedTool {
    fn name(&self) -> &str {
        ToolName::DocumentEmbed.as_str()
    }
    fn description(&self) -> &'static str {
        "ノート（.md ファイル）の本文末尾にグラフ・表・構成図などの図を入れる。ユーザーが\
         「このグラフをノートに入れて」「図をノートに追加して」等と依頼したときに使う。source は\
         回答の本文に書く ```ui ブロックの中身（Yui の記述・フェンスは不要）。表示だけの種類に限る\
         （app・ask・plan は入れられない）。追記のみで既存内容は消さない。"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "node_id": { "type": "string", "format": "uuid", "description": "ノートのノード ID" },
                "source": {
                    "type": "string",
                    "description": "```ui ブロックの中身（例: \"bar 月別の売上\\n4月 | 120\\n5月 | 150\"）"
                }
            },
            "required": ["node_id", "source"]
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
        let markdown = match figure_markdown(&input.source) {
            Ok(markdown) => markdown,
            Err(msg) => return Ok(ToolOutcome::error(msg)),
        };
        let node = match self
            .storage
            .get_metadata(ctx, input.node_id, trace_id)
            .await
        {
            Ok(node) => node,
            Err(e) => return Ok(denied_outcome(&collab::CollabError::Storage(e))),
        };
        let ops = [collab::note::EditOp::Append { markdown }];
        let report = match self
            .collab
            .apply_ai_edit(ctx, &node, &ops, collab::note::EditMode::Direct)
            .await
        {
            Ok(report) => report,
            Err(e) => return Ok(denied_outcome(&e)),
        };
        if report.applied == 0 {
            return Ok(ToolOutcome::error("図を入れられませんでした。"));
        }
        Ok(ToolOutcome::ok(format!(
            "ノート「{}」に図を入れました。",
            node.name
        )))
    }
}

#[cfg(test)]
mod tests {
    //! collab error → モデル観測メッセージの写像（`denied_outcome`）を検証する。
    use super::denied_outcome;
    use collab::CollabError as CE;

    /// 権限なしと未検出が**同一メッセージ**であること（存在秘匿・API 層の 404 統一と同じ契約）。
    #[test]
    fn forbidden_and_not_found_are_indistinguishable() {
        let expected = denied_outcome(&CE::Forbidden("x".into()));
        assert!(expected.is_error);
        assert!(expected.content.contains("アクセスできません"));
        for e in [
            CE::Authz(authz::AuthzError::InvalidModel("m".into())),
            CE::Storage(storage::StorageError::Forbidden),
            CE::NotFound("x".into()),
            CE::Storage(storage::StorageError::NotFound),
        ] {
            assert_eq!(denied_outcome(&e).content, expected.content);
        }
    }

    #[test]
    fn figure_markdown_wraps_display_figures() {
        use super::figure_markdown;
        assert_eq!(
            figure_markdown("bar 売上\n4月 | 120").unwrap(),
            "```ui\nbar 売上\n4月 | 120\n```\n"
        );
        // フェンスごと渡されても外し、種類付きの情報文字列は保つ。
        assert_eq!(
            figure_markdown("```ui:bar\n売上\n4月 | 120\n```").unwrap(),
            "```ui:bar\n売上\n4月 | 120\n```\n"
        );
        // 種類をフェンスの空白区切り・大文字・yui で指定しても落とさない。
        assert_eq!(
            figure_markdown("```ui bar\n月別売上\n1月 | 120\n```").unwrap(),
            "```ui:bar\n月別売上\n1月 | 120\n```\n"
        );
        assert!(figure_markdown("```YUI:Bar\n売上\n1月 | 1\n```")
            .unwrap()
            .starts_with("```ui:bar\n"));
        assert!(figure_markdown("```\nbar 売上\n1月 | 1\n```")
            .unwrap()
            .starts_with("```ui\n"));
        assert!(figure_markdown("```json\n{}\n```").is_err());
        assert!(figure_markdown("```uibar\n売上\n```").is_err());
        assert!(figure_markdown("  \n").is_err());
        assert!(figure_markdown("bar 売上\n```\n# 漏れた見出し").is_err());
        assert!(figure_markdown(&"a".repeat(super::MAX_FIGURE_BYTES + 1)).is_err());
        // 操作を持つ種類はノートに入れない。
        assert!(figure_markdown("app 申請\n金額 = number\nbutton 送る -> chat").is_err());
        assert!(figure_markdown("```ui:ask\n? 対象は\n- 国内\n```").is_err());
        assert!(figure_markdown("plan 計画\n1. 調べる").is_err());
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

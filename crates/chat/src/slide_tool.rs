//! AI スライド共同編集ツール（slide.read / slide.edit・Task 11.3・design §4.8.3）。
//!
//! エージェントは**共同編集参加者**としてスライド（Yjs）を編集する。人間と同じ
//! `editor@file` 権限で判定し（confused-deputy 回避・昇格しない）、編集は共有 Yjs
//! ドキュメントへ適用されて人間の並行編集と収束する（排他なし）。HTML 入力は
//! collab 側適用時に必ずサニタイズされる（PIT-40 第1層）。

use std::sync::{Arc, LazyLock};

use agent_core::{SlideDraft, Tool, ToolError, ToolName, ToolOutcome};
use authz::AuthContext;
use collab::CollabHub;
use serde::Deserialize;
use storage::StorageService;
use uuid::Uuid;

use crate::slide_templates::{design_guidance, is_known_theme, THEMES};

/// collab のエラーを**モデルが読む error 観測**へ写す（fail-closed・存在秘匿）。
fn denied_outcome(err: &collab::CollabError) -> ToolOutcome {
    use collab::CollabError as CE;
    let msg = match err {
        CE::Forbidden(_) | CE::Authz(_) | CE::Storage(storage::StorageError::Forbidden) => {
            "このスライドを編集する権限がありません（editor 権限が必要です）。"
        }
        CE::NotFound(_) | CE::Storage(storage::StorageError::NotFound) => {
            "指定されたスライドが見つかりません。"
        }
        _ => "スライド編集に失敗しました。",
    };
    ToolOutcome::error(msg)
}

/// スライドの現在内容（正規化 JSON）を読むツール（編集前の把握に使う）。
pub struct SlideReadTool {
    collab: Arc<CollabHub>,
    storage: Arc<StorageService>,
}

impl SlideReadTool {
    pub fn new(collab: Arc<CollabHub>, storage: Arc<StorageService>) -> Self {
        SlideReadTool { collab, storage }
    }
}

#[derive(Debug, Deserialize)]
struct ReadInput {
    /// 対象スライド（.slide ファイル）の ID。
    node_id: Uuid,
}

#[async_trait::async_trait]
impl Tool for SlideReadTool {
    fn name(&self) -> &str {
        ToolName::SlideRead.as_str()
    }
    fn description(&self) -> &'static str {
        "スライド（.slide ファイル）の現在の内容を正規化 JSON で読み取る（各スライドの\
         id・本文 HTML・スピーカーノート・メタデータ）。slide.edit で編集する前に、\
         スライド構成と id を把握するために使う。"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "node_id": { "type": "string", "format": "uuid", "description": "スライドのノード ID" }
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
        match self.collab.read_slide_json(ctx, &node).await {
            Ok(json) => Ok(ToolOutcome::ok(format!(
                "# スライド「{}」の現在の内容（正規化 JSON）\n\n```json\n{json}```",
                node.name
            ))),
            Err(e) => Ok(denied_outcome(&e)),
        }
    }
}

/// スライドを共同編集参加者として編集するツール。
pub struct SlideEditTool {
    collab: Arc<CollabHub>,
    storage: Arc<StorageService>,
}

impl SlideEditTool {
    pub fn new(collab: Arc<CollabHub>, storage: Arc<StorageService>) -> Self {
        SlideEditTool { collab, storage }
    }
}

#[derive(Debug, Deserialize)]
struct EditInput {
    /// 対象スライド（.slide ファイル）の ID。
    node_id: Uuid,
    /// 編集操作列（順に適用）。
    ops: Vec<collab::slide::SlideEditOp>,
}

#[async_trait::async_trait]
impl Tool for SlideEditTool {
    fn name(&self) -> &str {
        ToolName::SlideEdit.as_str()
    }
    fn description(&self) -> &'static str {
        // テーマカタログ＋デザイン指針を焼き込む（閉集合の語彙・design §4.8.3）。
        static DESC: LazyLock<String> = LazyLock::new(|| {
            format!(
                "スライド（.slide ファイル）を共同編集参加者として編集する。人間が編集中でも安全に\
                 同時編集できる（CRDT で収束）。操作: append_slide（末尾に追加）/ insert_slide_after\
                 （指定 id の直後に挿入）/ replace_slide（本文 HTML 置換）/ remove_slide / set_notes\
                 （スピーカーノート）/ set_background（{{\"color\":\"#rrggbb\"}}）/ set_meta（title・\
                 theme_id 等）。編集前に slide.read で構成と id を確認すること。{}",
                design_guidance()
            )
        });
        DESC.as_str()
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "node_id": { "type": "string", "format": "uuid", "description": "スライドのノード ID" },
                "ops": {
                    "type": "array",
                    "description": "編集操作列（順に適用）",
                    "items": {
                        "type": "object",
                        "properties": {
                            "op": {
                                "type": "string",
                                "enum": ["append_slide", "insert_slide_after", "replace_slide",
                                          "remove_slide", "set_notes", "set_background", "set_meta"]
                            },
                            "slide_id": { "type": "string", "description": "対象スライド id（append_slide/set_meta 以外で必須）" },
                            "html": { "type": "string", "description": "スライド本文 HTML（append_slide/insert_slide_after/replace_slide）" },
                            "notes": { "type": "string", "description": "スピーカーノート（set_notes・追加系では任意）" },
                            "bg": { "type": "object", "description": "背景指定（set_background・例 {\"color\":\"#ffffff\"}）" },
                            "key": { "type": "string", "description": "プロパティ名（set_meta・title/theme_id/tags/任意）" },
                            "value": { "type": "string", "description": "プロパティ値（set_meta）" }
                        },
                        "required": ["op"]
                    }
                }
            },
            "required": ["node_id", "ops"]
        })
    }

    /// 破壊的（既存スライドを書き換え得る）ため確認対象。承認ゲートの対象になる。
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
            .apply_ai_slide_edit(ctx, &node, &input.ops)
            .await
        {
            Ok(report) => report,
            Err(e) => return Ok(denied_outcome(&e)),
        };

        use std::fmt::Write as _;
        let mut content = format!(
            "スライド「{}」を編集しました（{} 件適用）。",
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

/// AI 生成スライドを**下書き**として用意するツール（slide_draft カード化・Task 11.3）。
///
/// 「パワポ/スライドを作って」等の依頼に対し、スライド一式を**下書き**として返す（この時点では
/// StorageService へは作らない）。フロントは下書きスライド画面を開き、ユーザーがそこで AI と
/// 内容を詰めてから「ドライブに保存」を押して初めてスライドを実体化する（save_note と同型の
/// 下書き確定型・issue #282 の状態機械をスライドへ展開）。
///
/// 下書きは**会話内で name をキーに識別**する: 同じ name で呼び直すと同じ下書きが更新され、
/// 別 name なら別の下書きになる。ストレージ書込を伴わないため確認ゲートは不要（確定は UI の
/// 保存ボタンが担う・fail-closed はそちら）。HTML はこの時点でサニタイズし、「サニタイズ済みが
/// 正規形」を下書きにも保証する（PIT-40 第1層）。
pub struct SaveSlideTool;

impl SaveSlideTool {
    pub fn new() -> Self {
        SaveSlideTool
    }
}

impl Default for SaveSlideTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct SaveSlideInput {
    /// スライド名（`.slide` は自動付与）。下書きの識別キーも兼ねる。
    name: String,
    /// スライド列（先頭が表紙）。
    slides: Vec<SaveSlideItem>,
    /// テーマ（カタログの閉集合・省略可）。
    #[serde(default)]
    theme_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SaveSlideItem {
    /// スライド本文 HTML（基本要素サブセット・サーバ側でサニタイズされる）。
    html: String,
    /// スピーカーノート（任意）。
    #[serde(default)]
    notes: Option<String>,
}

#[async_trait::async_trait]
impl Tool for SaveSlideTool {
    fn name(&self) -> &str {
        ToolName::SaveSlide.as_str()
    }
    fn description(&self) -> &'static str {
        static DESC: LazyLock<String> = LazyLock::new(|| {
            format!(
                "会話で生成したスライド一式を新しいスライドの下書きとして用意する。ユーザーが\
                 「パワポを作って」「スライドにして」等と依頼したときに使う。呼ぶと下書きスライド\
                 画面が開き、ユーザーはそこで内容を確認・編集してから自分で「ドライブに保存」して\
                 確定する（このツールは保存しない）。内容を直す場合は**同じ name で呼び直す**と\
                 同じ下書きが更新される。別のスライドを同時に作る場合は別の name で呼ぶ。{}",
                design_guidance()
            )
        });
        DESC.as_str()
    }
    fn input_schema(&self) -> serde_json::Value {
        let theme_ids: Vec<&str> = THEMES.iter().map(|t| t.id).collect();
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "スライド名（.slide は自動付与）。同じ name で呼び直すと同じ下書きを更新する" },
                "slides": {
                    "type": "array",
                    "description": "スライド列（先頭が表紙）",
                    "items": {
                        "type": "object",
                        "properties": {
                            "html": { "type": "string", "description": "スライド本文 HTML（1280×720 前提・基本要素サブセット）" },
                            "notes": { "type": "string", "description": "スピーカーノート（任意）" }
                        },
                        "required": ["html"]
                    }
                },
                "theme_id": { "type": "string", "enum": theme_ids, "description": "テーマ（省略可・カタログの閉集合）" }
            },
            "required": ["name", "slides"]
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
        let input: SaveSlideInput = serde_json::from_value(input)
            .map_err(|e| ToolError::Invalid(format!("入力が不正です: {e}")))?;
        let name = input.name.trim();
        if name.is_empty() {
            return Err(ToolError::Invalid("スライド名を指定してください".into()));
        }
        if input.slides.is_empty() {
            return Err(ToolError::Invalid(
                "slides が空です（1 枚以上指定してください）".into(),
            ));
        }
        // テーマは閉集合で照合する（未知はモデルへ差し戻して自己修正させる・fail-closed）。
        if let Some(theme) = input.theme_id.as_deref() {
            if !is_known_theme(theme) {
                let ids: Vec<&str> = THEMES.iter().map(|t| t.id).collect();
                return Err(ToolError::Invalid(format!(
                    "未知の theme_id です: {theme}（利用可能: {}）",
                    ids.join(", ")
                )));
            }
        }
        // 表示名は .slide を落として持つ（下書きカード/画面のタイトル用）。保存時に付与する。
        let display_name = name.strip_suffix(".slide").unwrap_or(name);
        // 下書き本文もサニタイズする（「サニタイズ済みが正規形」を下書きにも保証・PIT-40）。
        let mut meta = collab::note::NoteMeta {
            title: Some(display_name.to_string()),
            ..Default::default()
        };
        if let Some(theme) = input.theme_id {
            meta.extra.insert("theme_id".into(), theme);
        }
        let slides: Vec<collab::slide::Slide> = input
            .slides
            .into_iter()
            .map(|s| collab::slide::Slide {
                id: Uuid::new_v4().to_string(),
                html: collab::slide::sanitize_html(&s.html),
                notes: s.notes.unwrap_or_default(),
                bg: None,
            })
            .collect();
        let count = slides.len();
        let content = collab::slide::SlideDoc { meta, slides }.to_json();
        let mut outcome = ToolOutcome::ok(format!(
            "下書きスライド「{display_name}」を用意しました（{count} 枚）。画面で内容を確認・\
             編集し、「ドライブに保存」で確定してください。"
        ));
        outcome.slide_drafts.push(SlideDraft {
            name: display_name.to_string(),
            content,
        });
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    //! error 写像（`denied_outcome`）と入力デシリアライズの純関数部分を検証する。
    //! hub 経由の適用・認可は crates/collab/tests/slide_ai_edit_it.rs が担う。
    use super::*;

    #[test]
    fn denied_outcomeは権限と存在を秘匿して畳む() {
        use collab::CollabError as CE;
        assert!(denied_outcome(&CE::Forbidden("x".into()))
            .content
            .contains("権限がありません"));
        assert!(
            denied_outcome(&CE::Storage(storage::StorageError::Forbidden))
                .content
                .contains("権限がありません")
        );
        assert!(denied_outcome(&CE::NotFound("x".into()))
            .content
            .contains("見つかりません"));
        assert!(denied_outcome(&CE::InvalidUpdate("x".into()))
            .content
            .contains("失敗しました"));
        assert!(denied_outcome(&CE::Forbidden("x".into())).is_error);
    }

    #[test]
    fn edit入力はopsのタグ付きenumでデシリアライズできる() {
        let input: EditInput = serde_json::from_value(serde_json::json!({
            "node_id": "0190f9a0-0000-7000-8000-000000000000",
            "ops": [
                { "op": "append_slide", "html": "<h2>追加</h2>", "notes": "メモ" },
                { "op": "replace_slide", "slide_id": "s1", "html": "<h1>置換</h1>" },
                { "op": "set_meta", "key": "title", "value": "提案書" }
            ]
        }))
        .expect("deserialize");
        assert_eq!(input.ops.len(), 3);
        assert!(matches!(
            input.ops[0],
            collab::slide::SlideEditOp::AppendSlide { .. }
        ));
        assert!(matches!(
            input.ops[1],
            collab::slide::SlideEditOp::ReplaceSlide { .. }
        ));
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

    /// save_slide は保存せず下書き（slide_draft）を出す・確認ゲートは不要（Task 11.3）。
    /// content は正規化スライド JSON で、HTML はサニタイズ済み・theme_id がメタに載る。
    #[tokio::test]
    async fn save_slideは書き込まず下書きを出しhtmlをサニタイズする() {
        let tool = SaveSlideTool::new();
        assert!(!tool.requires_confirmation(), "下書きは確認ゲート不要");
        let out = tool
            .call(
                &ctx(),
                serde_json::json!({
                    "name": "提案書.slide",
                    "theme_id": "warm",
                    "slides": [
                        { "html": "<h1>表紙</h1><script>alert(1)</script>", "notes": "挨拶" },
                        { "html": "<h2>本題</h2>" }
                    ]
                }),
                None,
            )
            .await
            .expect("call");
        assert!(!out.is_error);
        assert!(out.artifacts.is_empty(), "保存はしない（成果物を出さない）");
        assert_eq!(out.slide_drafts.len(), 1, "下書きを 1 件出す");
        let draft = &out.slide_drafts[0];
        assert_eq!(draft.name, "提案書", ".slide を落として持つ");
        let doc = collab::slide::SlideDoc::from_json(&draft.content).expect("正規化 JSON");
        assert_eq!(doc.slides.len(), 2);
        assert!(!doc.slides[0].html.contains("script"), "サニタイズ済み");
        assert!(doc.slides[0].html.contains("表紙"));
        assert_eq!(doc.slides[0].notes, "挨拶");
        assert_eq!(doc.meta.title.as_deref(), Some("提案書"));
        assert_eq!(
            doc.meta.extra.get("theme_id").map(String::as_str),
            Some("warm")
        );
    }

    /// 空名・空 slides・未知 theme_id は Invalid（モデルに自己修正を促す・fail-closed）。
    #[tokio::test]
    async fn save_slideは不正入力を拒否する() {
        let tool = SaveSlideTool::new();
        for input in [
            serde_json::json!({ "name": " ", "slides": [{ "html": "<h1>x</h1>" }] }),
            serde_json::json!({ "name": "a", "slides": [] }),
            serde_json::json!({ "name": "a", "theme_id": "bogus", "slides": [{ "html": "<h1>x</h1>" }] }),
        ] {
            let err = tool.call(&ctx(), input, None).await;
            assert!(matches!(err, Err(ToolError::Invalid(_))));
        }
    }

    /// description にテーマカタログ（閉集合）とデザイン指針が焼き込まれる（design §4.8.3）。
    #[test]
    fn ツールdescriptionにテーマ語彙が載る() {
        let save = SaveSlideTool::new();
        for t in THEMES {
            assert!(Tool::description(&save).contains(t.id));
        }
        assert!(Tool::description(&save).contains("1280×720"));
        assert!(Tool::description(&save).contains("保存しない"));
    }
}

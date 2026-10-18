//! `office.read` / `office.excel_edit` / `office.word_edit` のユニットテスト（#725）。
//!
//! 実 Collabora は差さず、[`SheetEngine`] 境界をフェイクで置き換えて「観測とカードに何を出すか」
//! を固定する（エンジン本体は `crates/office/tests/engine_{calc,writer}_it.rs` が実機で確かめる）。

#![allow(clippy::unwrap_used)]

use std::sync::Mutex;

use super::*;

fn ctx() -> AuthContext {
    AuthContext::new(
        authz::Principal {
            kind: authz::PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some("default".into()),
        },
        "acme".into(),
        "default".into(),
    )
}

enum Next {
    Report(SheetEditReport),
    Denied,
    Unsupported,
    /// 種類違いの文書（Word の道具に Excel など）。
    WrongKind,
}

/// 記録する 1 回分の編集（要約・既定シート・操作数）。
type RecordedEdit = (String, Option<String>, usize);

struct FakeEngine {
    next: Mutex<Option<Next>>,
    edits: Mutex<Vec<RecordedEdit>>,
}

impl FakeEngine {
    fn new(next: Next) -> Arc<Self> {
        Arc::new(FakeEngine {
            next: Mutex::new(Some(next)),
            edits: Mutex::new(Vec::new()),
        })
    }

    fn take(&self) -> Result<SheetEditReport, OfficeEngineError> {
        match self.next.lock().unwrap().take() {
            Some(Next::Report(r)) => Ok(r),
            Some(Next::Denied) => Err(OfficeEngineError::Session(LiveEditError::Denied)),
            Some(Next::WrongKind) => Err(OfficeEngineError::Session(LiveEditError::Unsupported)),
            Some(Next::Unsupported) | None => Err(OfficeEngineError::Unsupported),
        }
    }
}

#[async_trait::async_trait]
impl SheetEngine for FakeEngine {
    async fn read(
        &self,
        _ctx: &AuthContext,
        _file_id: Uuid,
        req: &SheetReadRequest,
    ) -> Result<String, OfficeEngineError> {
        self.take().map(|_| format!("読んだ範囲: {:?}", req.range))
    }

    async fn edit_sheet(
        &self,
        _ctx: &AuthContext,
        _file_id: Uuid,
        label: &str,
        default_sheet: Option<&str>,
        ops: &[serde_json::Value],
    ) -> Result<SheetEditReport, OfficeEngineError> {
        self.edits.lock().unwrap().push((
            label.to_string(),
            default_sheet.map(str::to_string),
            ops.len(),
        ));
        self.take()
    }

    async fn edit_doc(
        &self,
        _ctx: &AuthContext,
        _file_id: Uuid,
        label: &str,
        ops: &[serde_json::Value],
    ) -> Result<SheetEditReport, OfficeEngineError> {
        self.edits
            .lock()
            .unwrap()
            .push((label.to_string(), None, ops.len()));
        self.take()
    }
}

fn report(applied: bool, save: LiveSaveResult) -> SheetEditReport {
    SheetEditReport {
        file_name: "売上.xlsx".into(),
        text: if applied {
            "2 件の操作を適用しました".into()
        } else {
            "適用できませんでした".into()
        },
        applied,
        save,
        views: 2,
    }
}

fn edit_tool(engine: Arc<FakeEngine>) -> OfficeExcelEditTool {
    OfficeExcelEditTool { engine }
}

fn input(ops: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "node_id": Uuid::nil(),
        "summary": "合計行を追加",
        "sheet": "売上",
        "ops": ops,
    })
}

#[tokio::test]
async fn applied_edit_reports_save_and_leaves_a_document_card() {
    let engine = FakeEngine::new(Next::Report(report(
        true,
        LiveSaveResult::Saved { version: 7 },
    )));
    let tool = edit_tool(Arc::clone(&engine));
    let out = tool
        .call(&ctx(), input(serde_json::json!([
            {"op": "set_values", "range": "A6", "values": [["合計", "=SUM(B2:B5)"]]},
            {"op": "format", "range": "A6:B6", "font": {"bold": true}, "border": {"top": "thin"}}
        ])), None)
        .await
        .unwrap();
    assert!(!out.is_error, "{}", out.content);
    assert!(
        out.content
            .contains("「売上.xlsx」: 2 件の操作を適用しました"),
        "{}",
        out.content
    );
    assert!(out.content.contains("新しい版 v7"), "{}", out.content);
    assert_eq!(out.document_refs.len(), 1);
    assert_eq!(
        engine.edits.lock().unwrap()[0],
        ("合計行を追加".into(), Some("売上".into()), 2)
    );
}

#[tokio::test]
async fn rolled_back_edit_is_an_error_without_a_card() {
    let tool = edit_tool(FakeEngine::new(Next::Report(report(
        false,
        LiveSaveResult::NotAttempted,
    ))));
    let out = tool
        .call(
            &ctx(),
            input(serde_json::json!([{"op": "merge", "range": "A1:C1"}])),
            None,
        )
        .await
        .unwrap();
    assert!(out.is_error);
    assert!(
        out.document_refs.is_empty(),
        "変わっていない文書のカードを出さない"
    );
}

#[tokio::test]
async fn malformed_input_is_reported_without_touching_the_document() {
    // op の中身の検証はエンジン（parse_ops）が持つ。ここでは入力の形そのものの誤りを見る。
    let engine = FakeEngine::new(Next::Report(report(true, LiveSaveResult::Unverified)));
    let tool = edit_tool(Arc::clone(&engine));
    let out = tool
        .call(
            &ctx(),
            serde_json::json!({"node_id": Uuid::nil(), "summary": "x", "ops": "merge A1"}),
            None,
        )
        .await
        .unwrap();
    assert!(out.is_error);
    assert!(
        out.content.contains("文書は変わっていません"),
        "{}",
        out.content
    );
    assert!(
        engine.edits.lock().unwrap().is_empty(),
        "エンジンを呼んでいない"
    );
}

#[tokio::test]
async fn denied_and_unsupported_map_to_actionable_messages() {
    let tool = edit_tool(FakeEngine::new(Next::Denied));
    let out = tool
        .call(
            &ctx(),
            input(serde_json::json!([{"op": "merge", "range": "A1:C1"}])),
            None,
        )
        .await
        .unwrap();
    assert!(out.is_error);
    assert!(
        out.content.contains("アクセスできません"),
        "{}",
        out.content
    );

    let tool = edit_tool(FakeEngine::new(Next::Unsupported));
    let out = tool
        .call(
            &ctx(),
            input(serde_json::json!([{"op": "merge", "range": "A1:C1"}])),
            None,
        )
        .await
        .unwrap();
    assert!(
        out.content.contains("office.live_edit"),
        "代わりの道具を示す: {}",
        out.content
    );
}

#[tokio::test]
async fn read_passes_the_range_through() {
    let engine = FakeEngine::new(Next::Report(report(true, LiveSaveResult::NotAttempted)));
    let tool = OfficeReadTool { engine };
    let out = tool
        .call(
            &ctx(),
            serde_json::json!({"node_id": Uuid::nil(), "range": "売上!A1:E5"}),
            None,
        )
        .await
        .unwrap();
    assert!(!out.is_error);
    assert!(out.content.contains("売上!A1:E5"), "{}", out.content);
    assert!(tool.is_read_only());
}

#[test]
fn edit_requires_confirmation_and_lists_every_op_in_the_schema() {
    let tool = edit_tool(FakeEngine::new(Next::Unsupported));
    assert!(tool.requires_confirmation());
    let schema = tool.input_schema();
    let listed: Vec<String> = schema["properties"]["ops"]["items"]["properties"]["op"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    // スキーマの列挙と説明文が、語彙（SheetOp）のすべての op を載せていること。
    for op in [
        "set_values",
        "fill_formula",
        "format",
        "merge",
        "unmerge",
        "column_width",
        "row_height",
        "insert_rows",
        "delete_rows",
        "insert_columns",
        "delete_columns",
        "hide_rows",
        "show_rows",
        "hide_columns",
        "show_columns",
        "clear",
        "sort",
        "autofilter",
        "freeze",
        "add_sheet",
        "rename_sheet",
        "delete_sheet",
        "move_sheet",
        "sheet_props",
        "conditional_format",
        "clear_conditional_formats",
        "data_validation",
        "define_name",
        "add_chart",
        "delete_chart",
        "page_setup",
        "style_table",
    ] {
        assert!(listed.iter().any(|l| l == op), "スキーマに {op} が無い");
        assert!(tool.description().contains(op), "説明文に {op} が無い");
    }
    assert_eq!(listed.len(), 32);
}

#[tokio::test]
async fn word_edit_applies_and_points_other_kinds_to_the_right_tool() {
    let engine = FakeEngine::new(Next::Report(SheetEditReport {
        file_name: "報告書.docx".into(),
        text: "1 件の操作を適用しました".into(),
        applied: true,
        save: LiveSaveResult::Saved { version: 3 },
        views: 1,
    }));
    let tool = OfficeWordEditTool {
        engine: Arc::clone(&engine) as Arc<dyn SheetEngine>,
    };
    let out = tool
        .call(
            &ctx(),
            serde_json::json!({"node_id": Uuid::nil(), "summary": "概要を追加",
                "ops": [{"op": "insert", "at": "end", "html": "<p>概要</p>"}]}),
            None,
        )
        .await
        .unwrap();
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("「報告書.docx」"), "{}", out.content);
    assert_eq!(out.document_refs.len(), 1);
    assert_eq!(
        engine.edits.lock().unwrap()[0],
        ("概要を追加".into(), None, 1)
    );

    // Excel を渡されたら excel_edit へ案内する（逆も同じ）。
    let tool = OfficeWordEditTool {
        engine: FakeEngine::new(Next::WrongKind),
    };
    let out = tool
        .call(
            &ctx(),
            serde_json::json!({"node_id": Uuid::nil(), "summary": "x",
                "ops": [{"op": "delete", "target": "b1-0000"}]}),
            None,
        )
        .await
        .unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("office.excel_edit"), "{}", out.content);
    let out = edit_tool(FakeEngine::new(Next::WrongKind))
        .call(
            &ctx(),
            input(serde_json::json!([{"op": "merge", "range": "A1:C1"}])),
            None,
        )
        .await
        .unwrap();
    assert!(out.content.contains("office.word_edit"), "{}", out.content);
}

#[test]
fn word_edit_requires_confirmation_and_lists_every_op_in_the_schema() {
    let tool = OfficeWordEditTool {
        engine: FakeEngine::new(Next::Unsupported),
    };
    assert!(tool.requires_confirmation());
    let schema = tool.input_schema();
    let listed: Vec<String> = schema["properties"]["ops"]["items"]["properties"]["op"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    // スキーマの列挙と説明文が、語彙（DocOp）のすべての op を載せていること。
    let ops = [
        "insert",
        "replace",
        "delete",
        "replace_text",
        "format_text",
        "format_paragraph",
        "set_style",
        "page_setup",
        "header",
        "footer",
        "insert_toc",
        "update_toc",
        "page_break",
        "title_page",
        "insert_rows",
        "delete_rows",
        "set_cells",
        "format_cells",
        "table_props",
        "style_table",
        "track_changes",
        "accept_all_changes",
        "reject_all_changes",
    ];
    for op in ops {
        assert!(listed.iter().any(|l| l == op), "スキーマに {op} が無い");
        assert!(tool.description().contains(op), "説明文に {op} が無い");
    }
    assert_eq!(listed.len(), ops.len());
}

//! `save_document` / `save_sheet` のユニットテスト（#381）。
//!
//! 実 Collabora / StorageService は差さず、[`OfficeCreate`] 境界をフェイクで置き換えて
//! 「何を作りに行き」「観測とカードに何を出すか」を固定する。

#![allow(clippy::unwrap_used)]

use std::sync::Mutex;

use super::*;
use office::live::LiveOpResult;
use uuid::Uuid;

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

/// 記録する 1 回分の呼び出し（名前・種別・貼り込み ops）。
type RecordedCall = (String, OfficeKind, Vec<LiveOp>);
/// フェイクが返す結果（`Err(())` は作成自体の失敗）。
type FakeResult = Option<Result<(CreatedOffice, Option<LiveSaveResult>), ()>>;

/// 呼び出し引数を記録し、決め打ちの結果を返すフェイク。
struct FakeCreator {
    calls: Mutex<Vec<RecordedCall>>,
    result: Mutex<FakeResult>,
    node_id: Uuid,
}

impl FakeCreator {
    fn saved(kind: OfficeKind, name: &str, save: LiveSaveResult) -> Arc<FakeCreator> {
        let node_id = Uuid::new_v4();
        Arc::new(FakeCreator {
            calls: Mutex::new(Vec::new()),
            result: Mutex::new(Some(Ok((
                CreatedOffice {
                    node_id,
                    name: name.to_string(),
                    kind,
                    version: 1,
                },
                Some(save),
            )))),
            node_id,
        })
    }

    /// 作成は成功するが、テンプレ書き込み自体が失敗する（storage エラー）。
    fn failing() -> Arc<FakeCreator> {
        Arc::new(FakeCreator {
            calls: Mutex::new(Vec::new()),
            result: Mutex::new(Some(Err(()))),
            node_id: Uuid::nil(),
        })
    }

    fn last_call(&self) -> RecordedCall {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .last()
            .cloned()
            .expect("1 回は呼ばれる")
    }
}

#[async_trait::async_trait]
impl OfficeCreate for FakeCreator {
    async fn create(
        &self,
        _ctx: &AuthContext,
        name: &str,
        kind: OfficeKind,
        ops: &[LiveOp],
        _trace_id: Option<&str>,
    ) -> Result<CreateOutcome, OfficeError> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((name.to_string(), kind, ops.to_vec()));
        let result = self
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        match result {
            Some(Ok((created, save))) => {
                // 本体と同契約: ops が空ならセッションを張らない（fill 無し）。
                let save = if ops.is_empty() { None } else { save };
                let fill = save.map(|save| {
                    Ok(LiveEditReport {
                        file_name: created.name.clone(),
                        results: ops
                            .iter()
                            .map(|op| LiveOpResult {
                                op: op.label(),
                                applied: true,
                                warning: None,
                            })
                            .collect(),
                        aborted: None,
                        save,
                        views: 1,
                    })
                });
                Ok((created, fill))
            }
            _ => Err(OfficeError::Storage(storage::StorageError::Forbidden)),
        }
    }
}

fn document_tool(fake: Arc<FakeCreator>) -> SaveDocumentTool {
    SaveDocumentTool { creator: fake }
}

fn sheet_tool(fake: Arc<FakeCreator>) -> SaveSheetTool {
    SaveSheetTool { creator: fake }
}

/// 作成系は**必ず承認ゲートを通る**（AI が黙ってドライブへファイルを作らない・受け入れ条件）。
#[test]
fn creation_tools_require_confirmation() {
    let fake = FakeCreator::saved(OfficeKind::Document, "x.docx", LiveSaveResult::Unverified);
    assert!(document_tool(fake.clone()).requires_confirmation());
    assert!(sheet_tool(fake).requires_confirmation());
}

/// Word: md は HTML の append_html op になり、`.docx` 名で作られ、document_ref が出る。
#[tokio::test]
async fn save_document_creates_docx_and_pastes_html() {
    let fake = FakeCreator::saved(
        OfficeKind::Document,
        "提案書.docx",
        LiveSaveResult::Saved { version: 2 },
    );
    let out = document_tool(fake.clone())
        .call(
            &ctx(),
            serde_json::json!({ "name": "提案書", "markdown": "# 提案\n\n**重要**な[案内](https://example.com)" }),
            None,
        )
        .await
        .unwrap();
    assert!(!out.is_error, "{}", out.content);

    let (name, kind, ops) = fake.last_call();
    assert_eq!(name, "提案書.docx");
    assert_eq!(kind, OfficeKind::Document);
    let LiveOp::AppendHtml { html } = &ops[0] else {
        panic!("append_html で貼り込む: {ops:?}");
    };
    // md→docx の自前サブセットで落ちていた書式が HTML として残る（#381 の眼目）。
    assert!(html.contains("<strong>重要</strong>"), "{html}");
    assert!(html.contains("href=\"https://example.com\""), "{html}");

    // 成果物への導線カード（document_ref）が付き、版まで載る。
    assert_eq!(out.document_refs.len(), 1);
    assert_eq!(out.document_refs[0]["kind"], "office");
    assert_eq!(out.document_refs[0]["name"], "提案書.docx");
    assert_eq!(out.document_refs[0]["version"], 2);
    assert_eq!(out.document_refs[0]["id"], fake.node_id.to_string());
    assert!(out.content.contains("node_id"), "{}", out.content);
}

/// 本文なしは op を出さない（空の .docx を作るだけ・worker も Collabora も要らない）。
#[tokio::test]
async fn save_document_without_markdown_creates_empty_file() {
    let fake = FakeCreator::saved(
        OfficeKind::Document,
        "無題.docx",
        LiveSaveResult::NotAttempted,
    );
    let out = document_tool(fake.clone())
        .call(&ctx(), serde_json::json!({ "name": "無題" }), None)
        .await
        .unwrap();
    assert!(fake.last_call().2.is_empty(), "op を出さない");
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(out.document_refs.len(), 1);
}

/// Excel: rows は A1 起点の set_cells になり、`.xlsx` 名で作られる。
#[tokio::test]
async fn save_sheet_creates_xlsx_and_sets_cells() {
    let fake = FakeCreator::saved(
        OfficeKind::Spreadsheet,
        "売上.xlsx",
        LiveSaveResult::Saved { version: 2 },
    );
    let out = sheet_tool(fake.clone())
        .call(
            &ctx(),
            serde_json::json!({ "name": "売上.xlsx", "rows": [["部門", "金額"], ["営業", 120]] }),
            None,
        )
        .await
        .unwrap();
    assert!(!out.is_error, "{}", out.content);
    let (name, kind, ops) = fake.last_call();
    assert_eq!(name, "売上.xlsx", "拡張子を二重付与しない");
    assert_eq!(kind, OfficeKind::Spreadsheet);
    let LiveOp::SetCells { anchor, rows } = &ops[0] else {
        panic!("set_cells で貼り込む: {ops:?}");
    };
    assert_eq!(anchor, "A1");
    assert_eq!(rows.len(), 2);
    assert_eq!(out.document_refs[0]["kind"], "office");
}

/// 名前が空・拡張子だけならツール入力エラー（モデルが直せる Invalid）。
#[tokio::test]
async fn rejects_empty_or_extension_only_name() {
    let fake = FakeCreator::saved(OfficeKind::Document, "x.docx", LiveSaveResult::Unverified);
    for name in ["  ", ".docx", ".DOCX", "  .docx  "] {
        let err = document_tool(fake.clone())
            .call(&ctx(), serde_json::json!({ "name": name }), None)
            .await;
        assert!(
            matches!(err, Err(ToolError::Invalid(_))),
            "{name:?} は拒否されること"
        );
    }
}

/// 生 HTML は Collabora へ実行可能な形で渡らない（正規化＋サニタイズの二層・PIT-40）。
#[tokio::test]
async fn raw_html_never_reaches_the_paste_op() {
    let fake = FakeCreator::saved(OfficeKind::Document, "x.docx", LiveSaveResult::Unverified);
    document_tool(fake.clone())
        .call(
            &ctx(),
            serde_json::json!({
                "name": "x",
                "markdown": "<script>alert(1)</script>\n\n<img src=x onerror=alert(1)>"
            }),
            None,
        )
        .await
        .unwrap();
    let (_, _, ops) = fake.last_call();
    let LiveOp::AppendHtml { html } = &ops[0] else {
        panic!("append_html で貼り込む: {ops:?}");
    };
    // タグとしては現れない（コードブロック内のエスケープ済みテキストとしてのみ残る）。
    assert!(!html.contains("<script"), "{html}");
    assert!(!html.contains("<img"), "{html}");
    assert!(
        html.contains("&lt;script&gt;"),
        "エスケープされて残る: {html}"
    );
}

/// 作成自体が拒否された場合は観測エラー（カードは出さない＝存在しないものを指さない）。
#[tokio::test]
async fn creation_denied_is_observed_without_card() {
    let out = document_tool(FakeCreator::failing())
        .call(&ctx(), serde_json::json!({ "name": "提案書" }), None)
        .await
        .unwrap();
    assert!(out.is_error);
    assert!(out.document_refs.is_empty());
    assert!(
        out.content.contains("作成する権限がありません"),
        "{}",
        out.content
    );
}

//! Office ファイル（Word/Excel）の新規作成ツール（save_document / save_sheet・#381）。
//!
//! **md 下書き画面（`/office/draft`）は廃止した**。同じ「Word 文書」なのに新規作成だけ md
//! エディタになり（「Word と言われたのにノートを書いている」）、しかも md→docx 変換は
//! 対応記法の追加でしか広がらない構造だったため、本物のエンジン（Collabora/LibreOffice）へ
//! 一本化した。実体は「空テンプレを作成 → 同じ HTML paste で本文を流し込む」の 2 段
//! （[`office::OfficeCreator`]）。
//!
//! 人間ゲートは**下書きカード → 承認カード**へ移した: `requires_confirmation = true` なので
//! AI は承認なしにドライブへファイルを作れない（#332 の意図を維持）。
//!
//! 認可は 2 段とも既存チョークポイント（StorageService の親フォルダ ReBAC / LiveEditor の
//! `editor@file` 毎回再判定）を通り、発話ユーザー権限のまま昇格しない。

use std::sync::Arc;

use agent_core::{Tool, ToolError, ToolName, ToolOutcome};
use authz::AuthContext;
use office::live::{CellValue, LiveEditError, LiveEditReport, LiveOp, LiveSaveResult};
use office::{CreatedOffice, OfficeError, OfficeKind};
use serde::Deserialize;

/// 作成結果と、本文流し込み（Collabora セッション）の結果。ops が空なら流し込みは `None`。
type CreateOutcome = (CreatedOffice, Option<Result<LiveEditReport, LiveEditError>>);

/// テスト差し替え用の薄い境界（本体は [`office::OfficeCreator`]）。
#[async_trait::async_trait]
trait OfficeCreate: Send + Sync {
    async fn create(
        &self,
        ctx: &AuthContext,
        name: &str,
        kind: OfficeKind,
        ops: &[LiveOp],
        trace_id: Option<&str>,
    ) -> Result<CreateOutcome, OfficeError>;
}

#[async_trait::async_trait]
impl OfficeCreate for office::OfficeCreator {
    async fn create(
        &self,
        ctx: &AuthContext,
        name: &str,
        kind: OfficeKind,
        ops: &[LiveOp],
        trace_id: Option<&str>,
    ) -> Result<CreateOutcome, OfficeError> {
        // 保存先はドライブのルート（org 直下・save_note / 成果物保存と同じ既定）。
        // future が大きい（WS セッション込み）ため heap へ逃がす（clippy::large_futures）。
        Box::pin(office::OfficeCreator::create(
            self, ctx, None, name, kind, ops, trace_id,
        ))
        .await
    }
}

/// 文書名の検証（空・拡張子だけの名前を弾く）。
fn validate_name(name: &str, kind: OfficeKind) -> Result<String, ToolError> {
    let name = name.trim();
    let stem = name
        .strip_suffix(&format!(".{}", kind.extension()))
        .or_else(|| name.strip_suffix(&format!(".{}", kind.extension().to_uppercase())))
        .unwrap_or(name)
        .trim();
    if stem.is_empty() {
        return Err(ToolError::Invalid(format!(
            "{}の名前を指定してください",
            kind.label()
        )));
    }
    Ok(kind.file_name(stem))
}

/// 作成＋流し込みの結果をモデルへの観測テキストへ整形する。
///
/// 「作れたが本文が入らなかった」を成功と偽らない（部分適用は正直に報告する・PIT-47 と同じ流儀）。
fn format_created(
    created: &CreatedOffice,
    fill: Option<&Result<LiveEditReport, LiveEditError>>,
) -> (String, bool, Option<i64>) {
    use std::fmt::Write as _;
    let mut content = format!(
        "{}「{}」を作成しました（node_id: {}）。",
        created.kind.label(),
        created.name,
        created.node_id
    );
    let mut version = Some(created.version);
    let mut is_error = false;
    match fill {
        None => content.push_str("本文は空です。"),
        Some(Ok(report)) => {
            let applied = report.results.iter().filter(|r| r.applied).count();
            let _ = write!(
                content,
                "本文を流し込みました（{applied}/{} 件適用）",
                report.results.len()
            );
            for r in &report.results {
                if let Some(warning) = &r.warning {
                    let _ = write!(content, "\n- {}: 不発（{warning}）", r.op);
                }
            }
            if let Some(reason) = &report.aborted {
                let _ = write!(
                    content,
                    "\n途中で失敗しました（{reason}）。以降は未適用です。"
                );
            }
            match &report.save {
                LiveSaveResult::Saved { version: v } => {
                    version = Some(*v);
                    let _ = write!(content, "\n保存: v{v}");
                }
                LiveSaveResult::Unverified => {
                    // 版が前進したか確認できていない＝作成時の版を名乗らない（嘘の版を出さない）。
                    version = None;
                    content.push_str("\n保存: 適用済みだが永続化は未確認です。");
                }
                LiveSaveResult::Failed(reason) => {
                    version = None;
                    is_error = true;
                    let _ = write!(content, "\n保存: 失敗しました（{reason}）。");
                }
                LiveSaveResult::NotAttempted => {
                    is_error = true;
                    content.push_str("\n本文が 1 件も入りませんでした（空のファイルのままです）。");
                }
            }
        }
        Some(Err(e)) => {
            is_error = true;
            let _ = write!(
                content,
                "ただし本文を流し込めませんでした（{e}）。空のファイルとして残っています。\
                 office.live_edit に node_id 「{}」を渡して書き込んでください。",
                created.node_id
            );
        }
    }
    content.push_str("\nユーザーの画面ではこのファイルが Collabora で開きます。");
    (content, is_error, version)
}

/// 作成結果を [`ToolOutcome`] へ（document_ref カード付き・#381）。
fn outcome_for(
    created: &CreatedOffice,
    fill: Option<&Result<LiveEditReport, LiveEditError>>,
) -> ToolOutcome {
    let (content, is_error, version) = format_created(created, fill);
    let mut outcome = if is_error {
        ToolOutcome::error(content)
    } else {
        ToolOutcome::ok(content)
    };
    // 作成自体は成功しているので、失敗観測でも導線カードは出す（ユーザーが開いて続けられる）。
    outcome
        .document_refs
        .push(crate::document_ref::created_payload(
            created.node_id,
            &created.name,
            version,
        ));
    outcome
}

/// `OfficeError` をモデルが行動できる観測へ写す（権限なし/未検出は畳む・#326）。
fn create_error(kind: OfficeKind, e: OfficeError) -> ToolOutcome {
    match e {
        OfficeError::Forbidden | OfficeError::NotFound => {
            ToolOutcome::error(format!("{}を作成する権限がありません。", kind.label()))
        }
        OfficeError::Storage(storage::StorageError::Conflict) => ToolOutcome::error(format!(
            "同名の{}が多すぎます。別の名前で作成してください。",
            kind.label()
        )),
        other => ToolOutcome::error(format!("{}の作成に失敗しました（{other}）。", kind.label())),
    }
}

// ---------------------------------------------------------------------------
// save_document（Word）
// ---------------------------------------------------------------------------

/// 新規 Word 文書（.docx）を作成するツール（#381）。
pub struct SaveDocumentTool {
    creator: Arc<dyn OfficeCreate>,
}

impl SaveDocumentTool {
    pub fn new(creator: Arc<office::OfficeCreator>) -> Self {
        SaveDocumentTool { creator }
    }
}

#[derive(Debug, Deserialize)]
struct SaveDocumentInput {
    /// 文書名（`.docx` は自動付与）。
    name: String,
    /// 本文 Markdown（HTML に変換して Collabora へ貼り込む）。
    #[serde(default)]
    markdown: String,
}

#[async_trait::async_trait]
impl Tool for SaveDocumentTool {
    fn name(&self) -> &str {
        ToolName::SaveDocument.as_str()
    }
    fn description(&self) -> &'static str {
        "新しい Word 文書（.docx）をドライブに作成し、本文を書き込む。ユーザーが「Word で〜を\
         作って」「docx にして」等と依頼したときに使う。本文は Markdown で渡すとサーバ側で\
         Collabora（LibreOffice）に変換させるため、見出し・箇条書き（ネスト可）・表・太字・\
         斜体・リンク・引用がそのまま Word の書式になる。作成後はユーザーの画面で Collabora \
         Writer が開く。追記・修正は返る node_id を office.live_edit に渡すこと（同じ文書を\
         二重に作らない）。**ドライブにファイルを作る操作のため実行にはユーザーの承認が要る。**"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "文書名（.docx は自動付与）" },
                "markdown": { "type": "string", "description": "本文の Markdown（省略すると空の文書を作る）" }
            },
            "required": ["name"]
        })
    }
    /// ドライブへファイルを作る破壊的操作＝承認ゲート対象（AI が黙って作らない・#332 の意図）。
    fn requires_confirmation(&self) -> bool {
        true
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let input: SaveDocumentInput = serde_json::from_value(input)
            .map_err(|e| ToolError::Invalid(format!("入力が不正です: {e}")))?;
        let file_name = validate_name(&input.name, OfficeKind::Document)?;
        // 正規化（生 HTML はコードブロックへ縮退）→ HTML → サニタイズ（PIT-40 多層防御）。
        let ops = if input.markdown.trim().is_empty() {
            Vec::new()
        } else {
            let markdown = collab::note::normalize_markdown(&input.markdown);
            let html =
                collab::slide::sanitize::sanitize_html(&collab::note::md_html::to_html(&markdown));
            if html.trim().is_empty() {
                return Ok(ToolOutcome::error(
                    "本文がサニタイズで空になりました（対応していない書式のみで構成されています）。\
                     見出し・段落・箇条書き・表など最小の書式で指定し直してください。",
                ));
            }
            vec![LiveOp::AppendHtml { html }]
        };
        match self
            .creator
            .create(ctx, &file_name, OfficeKind::Document, &ops, trace_id)
            .await
        {
            Ok((created, fill)) => Ok(outcome_for(&created, fill.as_ref())),
            Err(e) => Ok(create_error(OfficeKind::Document, e)),
        }
    }
}

// ---------------------------------------------------------------------------
// save_sheet（Excel）
// ---------------------------------------------------------------------------

/// 新規 Excel ブック（.xlsx）を作成するツール（#381）。
pub struct SaveSheetTool {
    creator: Arc<dyn OfficeCreate>,
}

impl SaveSheetTool {
    pub fn new(creator: Arc<office::OfficeCreator>) -> Self {
        SaveSheetTool { creator }
    }
}

#[derive(Debug, Deserialize)]
struct SaveSheetInput {
    /// ブック名（`.xlsx` は自動付与）。
    name: String,
    /// A1 起点に貼り込む値の矩形（1 行目はヘッダにするのが自然）。
    #[serde(default)]
    rows: Vec<Vec<CellValue>>,
}

#[async_trait::async_trait]
impl Tool for SaveSheetTool {
    fn name(&self) -> &str {
        ToolName::SaveSheet.as_str()
    }
    fn description(&self) -> &'static str {
        "新しい Excel ブック（.xlsx）をドライブに作成し、A1 から値の矩形を書き込む。ユーザーが\
         「Excel で〜を作って」「xlsx にして」等と依頼したときに使う。rows は行の配列で、\
         各セルは文字列・数値・真偽値のいずれか（1 行目をヘッダにする）。数式は入らない\
         （値として扱われる）。作成後はユーザーの画面で Collabora Calc が開く。追記・修正は\
         返る node_id を office.live_edit（set_cells）に渡すこと。表形式のデータを分析だけ\
         したい場合は CSV（save_csv）の方が扱いやすい。\
         **ドライブにファイルを作る操作のため実行にはユーザーの承認が要る。**"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "ブック名（.xlsx は自動付与）" },
                "rows": {
                    "type": "array",
                    "description": "A1 起点に貼り込む値の矩形（行の配列・省略すると空のブックを作る）",
                    // セル値の union（"type" 配列）は一部プロバイダの function 検証が 400 で
                    // 拒否するため、スキーマ上は any にして説明で制約する（office.live_edit と同じ扱い）。
                    "items": {
                        "type": "array",
                        "items": { "description": "セル値（文字列・数値・真偽値のいずれか）" }
                    }
                }
            },
            "required": ["name"]
        })
    }
    /// ドライブへファイルを作る破壊的操作＝承認ゲート対象。
    fn requires_confirmation(&self) -> bool {
        true
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let input: SaveSheetInput = serde_json::from_value(input)
            .map_err(|e| ToolError::Invalid(format!("入力が不正です: {e}")))?;
        let file_name = validate_name(&input.name, OfficeKind::Spreadsheet)?;
        // 値は LiveEditor 側で全て HTML エスケープされる（数式/HTML 注入は構造的に不能）。
        let ops = if input.rows.iter().all(Vec::is_empty) {
            Vec::new()
        } else {
            vec![LiveOp::SetCells {
                anchor: "A1".to_string(),
                rows: input.rows,
            }]
        };
        match self
            .creator
            .create(ctx, &file_name, OfficeKind::Spreadsheet, &ops, trace_id)
            .await
        {
            Ok((created, fill)) => Ok(outcome_for(&created, fill.as_ref())),
            Err(e) => Ok(create_error(OfficeKind::Spreadsheet, e)),
        }
    }
}

#[cfg(test)]
#[path = "office_create_tool/tests.rs"]
mod tests;

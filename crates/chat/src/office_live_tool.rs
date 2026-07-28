//! 開いている Office 文書への AI ライブ編集ツール（office.live_edit・issue #352）。
//!
//! AI は Collabora（CoolWSD）セッションの **headless 参加者（独立 view）** として
//! 接続し、自分のカーソル・選択でアンカー指定編集を行う（実体は
//! [`office::live::LiveEditor`]）。ユーザーの選択には一切依存しないため、
//! 承認までに選択が変わっても対象がずれない（旧 postMessage 注入方式の TOCTOU を
//! 構造的に解消・#352）。編集は CoolWSD の協調プロトコルで全参加者へ即時反映され、
//! 参加者リストには「Shiki AI」が表示される。
//!
//! 認可: `LiveEditor` が発話ユーザーの `editor@file` を毎回 OpenFGA
//! （HigherConsistency）で再判定する（confused-deputy 回避・PIT-11）。権限なし/
//! 未検出は同一メッセージに畳む（存在秘匿・#326）。HTML は適用前にサニタイズする
//! （PIT-40・ammonia 許可リスト）。同一文書の AI↔AI はファイル単位で直列化される。

use std::fmt::Write as _;
use std::sync::Arc;

use agent_core::{Tool, ToolError, ToolName, ToolOutcome};
use authz::AuthContext;
use office::live::{LiveEditError, LiveEditReport, LiveOp, LiveSaveResult};
use serde::Deserialize;
use uuid::Uuid;

/// テスト差し替え用の薄い境界（本体は [`office::live::LiveEditor`]）。
#[async_trait::async_trait]
trait LiveApply: Send + Sync {
    async fn apply(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        ops: &[LiveOp],
    ) -> Result<LiveEditReport, LiveEditError>;
}

#[async_trait::async_trait]
impl LiveApply for office::live::LiveEditor {
    async fn apply(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        ops: &[LiveOp],
    ) -> Result<LiveEditReport, LiveEditError> {
        office::live::LiveEditor::apply(self, ctx, file_id, ops).await
    }
}

/// 開いている Office セッションへ AI が参加者としてライブ編集するツール。
pub struct OfficeLiveEditTool {
    live: Arc<dyn LiveApply>,
}

impl OfficeLiveEditTool {
    pub fn new(live: Arc<office::live::LiveEditor>) -> Self {
        OfficeLiveEditTool { live }
    }
}

#[derive(Debug, Deserialize)]
struct LiveEditInput {
    /// 対象 Office ファイル（.docx/.xlsx/.pptx）のノード ID。
    node_id: Uuid,
    /// アンカー指定の編集操作列（順に適用）。
    ops: Vec<LiveOp>,
}

/// html を持つ op をサニタイズする（PIT-40 第 1 層）。
/// サニタイズで空になった op の位置を返す（1 つでもあれば実行前に弾く）。
fn sanitize_ops(ops: Vec<LiveOp>) -> (Vec<LiveOp>, Vec<usize>) {
    let mut emptied = Vec::new();
    let sanitized = ops
        .into_iter()
        .enumerate()
        .map(|(idx, op)| match op {
            LiveOp::ReplaceText { find, html } => {
                let html = collab::slide::sanitize::sanitize_html(&html);
                if html.trim().is_empty() {
                    emptied.push(idx);
                }
                LiveOp::ReplaceText { find, html }
            }
            LiveOp::AppendHtml { html } => {
                let html = collab::slide::sanitize::sanitize_html(&html);
                if html.trim().is_empty() {
                    emptied.push(idx);
                }
                LiveOp::AppendHtml { html }
            }
            // set_cells の値は LiveEditor 側でデータとして全エスケープされる。
            other @ LiveOp::SetCells { .. } => other,
        })
        .collect();
    (sanitized, emptied)
}

/// レポートをモデルへの観測メッセージに整形する。
fn format_report(report: &LiveEditReport) -> (String, bool) {
    let applied = report.results.iter().filter(|r| r.applied).count();
    let mut content = format!(
        "「{}」へのライブ編集: {applied}/{} 件適用（セッション参加 view 数 {}）",
        report.file_name,
        report.results.len(),
        report.views,
    );
    for r in &report.results {
        let _ = write!(
            content,
            "\n- {}: {}",
            r.op,
            if r.applied { "適用" } else { "不発" }
        );
        if let Some(warning) = &r.warning {
            let _ = write!(content, "（{warning}）");
        }
    }
    if let Some(reason) = &report.aborted {
        let _ = write!(
            content,
            "\nセッションが途中で失敗しました（{reason}）。以降の op は未適用です。"
        );
    }
    match &report.save {
        LiveSaveResult::Saved { version } => {
            let _ = write!(
                content,
                "\n保存: 新バージョン v{version}（開いている編集画面にも即時反映済み）"
            );
        }
        LiveSaveResult::Unverified => {
            let _ = write!(
                content,
                "\n保存: セッションへ適用済み。永続化の確認は取れていません\
                 （開いている参加者の保存または自動保存で確定します）"
            );
        }
        LiveSaveResult::Failed(reason) => {
            let _ = write!(
                content,
                "\n保存: 失敗しました（{reason}）。編集は開いているセッション内に残っています"
            );
        }
        LiveSaveResult::NotAttempted => {
            let _ = write!(content, "\n保存: 適用 0 件のため保存していません");
        }
    }
    // 1 件も適用できなかった場合はエラー観測（モデルに対象指定の見直しを促す）。
    (content, applied == 0)
}

#[async_trait::async_trait]
impl Tool for OfficeLiveEditTool {
    fn name(&self) -> &str {
        ToolName::OfficeLiveEdit.as_str()
    }

    fn description(&self) -> &'static str {
        "Office 文書（Word/Excel/PowerPoint）へ AI が共同編集参加者として接続し、アンカー指定で\
         ライブ編集する。ユーザーが文書を開いていれば編集画面へ即時反映され、参加者リストに\
         「Shiki AI」が表示される。開いていなくても実行でき、新バージョンとして保存される。\
         ops: replace_text{find,html}（find を検索して HTML で置換。**文書内で一意な文字列**を\
         指定すること・段落をまたぐ文字列は不可）/ append_html{html}（docx 末尾へ追記）/ \
         set_cells{anchor,rows}（xlsx・anchor 例 \"A1\"・\"Sheet2.B3\" を起点に値の矩形を貼り込む）。\
         html は段落・箇条書き・見出し・表・強調など最小の書式のみ（script 等は自動除去）。\
         シート/スライドの追加・削除など構造的なファイル編集は office.edit を使うこと。\
         適用結果（op ごとの適用/不発と保存状態）が返るので、不発時はアンカー指定を見直すこと。"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "node_id": { "type": "string", "format": "uuid", "description": "対象 Office ファイルのノード ID" },
                "ops": {
                    "type": "array",
                    "minItems": 1,
                    "description": "アンカー指定の編集操作列（順に適用）",
                    "items": {
                        "type": "object",
                        "properties": {
                            "op": { "type": "string", "enum": ["replace_text", "append_html", "set_cells"] },
                            "find": { "type": "string", "description": "replace_text: 置換対象（文書内で一意な文字列）" },
                            "html": { "type": "string", "description": "replace_text/append_html: 挿入する HTML（最小書式）" },
                            "anchor": { "type": "string", "description": "set_cells: 起点セル（例 \"A1\"・\"Sheet2.B3\"）" },
                            "rows": {
                                "type": "array",
                                // セル値は文字列/数値/真偽のいずれか。union の "type" 配列は一部
                                // プロバイダ（DeepSeek 等）の function 検証が 400 で拒否するため、
                                // スキーマ上は any にして説明で制約する（サーバ側 serde が最終検証）。
                                "items": {
                                    "type": "array",
                                    "items": { "description": "セル値（文字列・数値・真偽値のいずれか）" }
                                },
                                "description": "set_cells: 貼り込む値の矩形（行の配列）"
                            }
                        },
                        "required": ["op"]
                    }
                }
            },
            "required": ["node_id", "ops"]
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
        if input.ops.is_empty() {
            return Err(ToolError::Invalid("ops が空です".into()));
        }
        let (ops, emptied) = sanitize_ops(input.ops);
        if !emptied.is_empty() {
            return Ok(ToolOutcome::error(format!(
                "ops[{}] の HTML がサニタイズで空になりました（対応していない書式のみで\
                 構成されています）。段落・箇条書き・見出し・表など最小の書式で指定し直してください。",
                emptied
                    .iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            )));
        }

        match self.live.apply(ctx, input.node_id, &ops).await {
            Ok(report) => {
                let (content, is_error) = format_report(&report);
                let mut outcome = if is_error {
                    ToolOutcome::error(content)
                } else {
                    ToolOutcome::ok(content)
                };
                // 編集結果をチャットに残す（#381）。適用 0 件（is_error）ならカードを出さない
                // ——「編集しました」と示して実際は何も変わっていない、を作らない。
                if !is_error {
                    // 版は確認できたときだけ載せる（Unverified で作成時の版を名乗らない）。
                    let version = match &report.save {
                        LiveSaveResult::Saved { version } => Some(*version),
                        _ => None,
                    };
                    outcome.document_refs.push(crate::document_ref::payload(
                        input.node_id,
                        &report.file_name,
                        version,
                    ));
                }
                Ok(outcome)
            }
            // 権限なし/未検出は同一メッセージに畳む（存在秘匿・#326・API 404 統一と同契約）。
            Err(LiveEditError::Denied) => Ok(ToolOutcome::error(
                "指定された文書にアクセスできません（存在しないか、権限がありません）。",
            )),
            Err(e @ (LiveEditError::Unsupported | LiveEditError::Busy)) => {
                Ok(ToolOutcome::error(format!("{e}。")))
            }
            Err(LiveEditError::Session(e)) => Ok(ToolOutcome::error(format!(
                "編集セッションを開始できませんでした（{e}）。しばらくして再実行するか、\
                 ファイル編集（office.edit）を検討してください。"
            ))),
            Err(LiveEditError::Internal(e)) => Err(ToolError::Internal(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use office::live::LiveOpResult;

    /// 決め打ちの結果を返すフェイク。
    struct FakeLive(Result<LiveEditReport, LiveEditError>);

    #[async_trait::async_trait]
    impl LiveApply for FakeLive {
        async fn apply(
            &self,
            _ctx: &AuthContext,
            _file_id: Uuid,
            _ops: &[LiveOp],
        ) -> Result<LiveEditReport, LiveEditError> {
            match &self.0 {
                Ok(report) => Ok(report.clone()),
                Err(LiveEditError::Denied) => Err(LiveEditError::Denied),
                Err(LiveEditError::Busy) => Err(LiveEditError::Busy),
                Err(LiveEditError::Unsupported) => Err(LiveEditError::Unsupported),
                Err(other) => Err(LiveEditError::Internal(other.to_string())),
            }
        }
    }

    fn tool(result: Result<LiveEditReport, LiveEditError>) -> OfficeLiveEditTool {
        OfficeLiveEditTool {
            live: Arc::new(FakeLive(result)),
        }
    }

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

    fn input(ops: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "node_id": Uuid::new_v4(), "ops": ops })
    }

    #[tokio::test]
    async fn denied_is_concealed() {
        let outcome = tool(Err(LiveEditError::Denied))
            .call(
                &ctx(),
                input(
                    serde_json::json!([{ "op": "replace_text", "find": "a", "html": "<p>b</p>" }]),
                ),
                None,
            )
            .await
            .unwrap();
        assert!(outcome.is_error);
        assert!(outcome.content.contains("存在しないか、権限がありません"));
    }

    #[tokio::test]
    async fn sanitized_empty_html_is_rejected_before_session() {
        let outcome = tool(Err(LiveEditError::Busy)) // フェイクには到達しない
            .call(
                &ctx(),
                input(serde_json::json!([
                    { "op": "replace_text", "find": "a", "html": "<script>alert(1)</script>" }
                ])),
                None,
            )
            .await
            .unwrap();
        assert!(outcome.is_error);
        assert!(outcome.content.contains("サニタイズで空"));
    }

    #[tokio::test]
    async fn report_formats_partial_application() {
        let report = LiveEditReport {
            file_name: "report.docx".into(),
            results: vec![
                LiveOpResult {
                    op: "replace_text",
                    applied: true,
                    warning: None,
                },
                LiveOpResult {
                    op: "replace_text",
                    applied: false,
                    warning: Some("検索文字列が見つかりません".into()),
                },
            ],
            aborted: None,
            save: LiveSaveResult::Saved { version: 12 },
            views: 3,
        };
        let outcome = tool(Ok(report))
            .call(
                &ctx(),
                input(serde_json::json!([
                    { "op": "replace_text", "find": "a", "html": "<p>b</p>" },
                    { "op": "replace_text", "find": "c", "html": "<p>d</p>" }
                ])),
                None,
            )
            .await
            .unwrap();
        assert!(!outcome.is_error);
        assert!(outcome.content.contains("1/2 件適用"));
        assert!(outcome.content.contains("検索文字列が見つかりません"));
        assert!(outcome.content.contains("新バージョン v12"));
    }

    #[tokio::test]
    async fn zero_applied_is_error_observation() {
        let report = LiveEditReport {
            file_name: "sheet.xlsx".into(),
            results: vec![LiveOpResult {
                op: "set_cells",
                applied: false,
                warning: Some("anchor が不正です（例: \"A1\"・\"Sheet2.B3\"）".into()),
            }],
            aborted: None,
            save: LiveSaveResult::NotAttempted,
            views: 1,
        };
        let outcome = tool(Ok(report))
            .call(
                &ctx(),
                input(serde_json::json!([
                    { "op": "set_cells", "anchor": "1A", "rows": [["x"]] }
                ])),
                None,
            )
            .await
            .unwrap();
        assert!(outcome.is_error);
        assert!(outcome.content.contains("0/1 件適用"));
    }

    #[tokio::test]
    async fn busy_is_observable() {
        let outcome = tool(Err(LiveEditError::Busy))
            .call(
                &ctx(),
                input(serde_json::json!([{ "op": "append_html", "html": "<p>x</p>" }])),
                None,
            )
            .await
            .unwrap();
        assert!(outcome.is_error);
        assert!(outcome.content.contains("別の AI 編集が進行中"));
    }
}

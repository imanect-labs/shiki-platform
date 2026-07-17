//! CSV エージェントツール（csv.query / csv.patch / csv.write・Task 11P.9）。
//!
//! すべて `TabularService`（11P.7 の単一チョークポイント・隔離 DuckDB）を通し、実行主体の
//! ファイル ReBAC で判定する（**操作別 relation**: query=viewer / patch=editor / write=作成権限・
//! 昇格しない）。権限不足・存在しないはモデルが読む error 観測へ畳む（confused-deputy 回避）。

use std::sync::Arc;

use agent_core::{Tool, ToolError, ToolName, ToolOutcome};
use authz::AuthContext;
use serde::Deserialize;
use tabular::{PatchOp, TabularService};
use uuid::Uuid;

/// tabular エラーを**モデルが読む error 観測**へ（fail-closed・存在秘匿）。
fn denied(err: &tabular::TabularError) -> ToolOutcome {
    use tabular::TabularError as TE;
    // クエリ実行エラー（未知の列・型不一致等）は DuckDB の理由をそのまま返し、モデルが
    // SQL を自己修正できるようにする（viewer 権限は既に確認済み＝存在秘匿の懸念なし）。
    if let TE::QueryFailed(m) = err {
        return ToolOutcome::error(format!("SQL の実行に失敗しました: {m}"));
    }
    let msg = match err {
        TE::Forbidden | TE::Authz(_) | TE::Storage(storage::StorageError::Forbidden) => {
            "この CSV を操作する権限がありません。"
        }
        TE::NotFound(_) | TE::Storage(storage::StorageError::NotFound) => {
            "指定された CSV が見つかりません。"
        }
        TE::SqlRejected(_) => "SQL が拒否されました（読み取り専用の SELECT のみ実行できます）。",
        TE::RevConflict { .. } => "CSV が他の編集で更新されています。最新を読み直してください。",
        _ => "CSV 操作に失敗しました。",
    };
    ToolOutcome::error(msg)
}

/// 結果テーブルをモデル向けに整形（列＋先頭 N 行）。
fn format_table(resp: &tabular::RunnerResponse, max_preview: usize) -> String {
    use std::fmt::Write as _;
    let mut out = format!("列: {}\n", resp.columns.join(", "));
    if let Some(total) = resp.total_rows {
        let _ = writeln!(out, "総行数: {total}");
    }
    for row in resp.rows.iter().take(max_preview) {
        let cells: Vec<String> = row.iter().map(|c| c.clone().unwrap_or_default()).collect();
        out.push_str(&cells.join(", "));
        out.push('\n');
    }
    if resp.rows.len() > max_preview || resp.truncated {
        out.push_str("…（結果は一部のみ表示）\n");
    }
    out
}

/// csv.query（RO SQL・viewer）。
pub struct CsvQueryTool {
    tabular: Arc<TabularService>,
}
impl CsvQueryTool {
    pub fn new(tabular: Arc<TabularService>) -> Self {
        CsvQueryTool { tabular }
    }
}

#[derive(Debug, Deserialize)]
struct QueryInput {
    node_id: Uuid,
    sql: String,
}

#[async_trait::async_trait]
impl Tool for CsvQueryTool {
    fn name(&self) -> &str {
        ToolName::CsvQuery.as_str()
    }
    fn description(&self) -> &'static str {
        "CSV ファイルに読み取り専用 SQL（SELECT のみ）を実行して分析する。テーブル名は `data`。\
         例: SELECT category, count(*) FROM data GROUP BY category。ファイルの閲覧権限が要る。"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "node_id": { "type": "string", "format": "uuid", "description": "CSV のノード ID" },
                "sql": { "type": "string", "description": "読み取り専用 SELECT（テーブル名 data）" }
            },
            "required": ["node_id", "sql"]
        })
    }
    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let input: QueryInput = serde_json::from_value(input)
            .map_err(|e| ToolError::Invalid(format!("入力が不正です: {e}")))?;
        match self
            .tabular
            .query(ctx, input.node_id, &input.sql, trace_id)
            .await
        {
            Ok(resp) => Ok(ToolOutcome::ok(format_table(&resp, 50))),
            Err(e) => Ok(denied(&e)),
        }
    }
}

/// csv.patch（パッチ編集→新バージョン・editor）。
pub struct CsvPatchTool {
    tabular: Arc<TabularService>,
}
impl CsvPatchTool {
    pub fn new(tabular: Arc<TabularService>) -> Self {
        CsvPatchTool { tabular }
    }
}

#[derive(Debug, Deserialize)]
struct PatchInput {
    node_id: Uuid,
    base_rev: i64,
    ops: Vec<PatchOp>,
}

#[async_trait::async_trait]
impl Tool for CsvPatchTool {
    fn name(&self) -> &str {
        ToolName::CsvPatch.as_str()
    }
    fn description(&self) -> &'static str {
        "CSV をパッチ編集して新しいバージョンを保存する（編集権限が要る）。ops は cell_update /\
         row_insert / row_delete / column_add / column_delete / column_rename。base_rev には編集前の\
         版（csv.query 等で得た version）を渡す（競合検出）。"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "node_id": { "type": "string", "format": "uuid" },
                "base_rev": { "type": "integer", "description": "編集前の版（競合検出）" },
                "ops": { "type": "array", "description": "パッチ操作列", "items": { "type": "object" } }
            },
            "required": ["node_id", "base_rev", "ops"]
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
        let input: PatchInput = serde_json::from_value(input)
            .map_err(|e| ToolError::Invalid(format!("入力が不正です: {e}")))?;
        match self
            .tabular
            .patch(ctx, input.node_id, input.base_rev, &input.ops, trace_id)
            .await
        {
            Ok(a) => Ok(ToolOutcome::ok(format!(
                "CSV を更新しました（v{}・{} 行 × {} 列）。",
                a.version, a.rows, a.cols
            ))),
            Err(e) => Ok(denied(&e)),
        }
    }
}

/// csv.write（新規 CSV 保存・作成権限）。
pub struct CsvWriteTool {
    tabular: Arc<TabularService>,
}
impl CsvWriteTool {
    pub fn new(tabular: Arc<TabularService>) -> Self {
        CsvWriteTool { tabular }
    }
}

#[derive(Debug, Deserialize)]
struct WriteInput {
    name: String,
    csv: String,
    #[serde(default)]
    parent_id: Option<Uuid>,
}

#[async_trait::async_trait]
impl Tool for CsvWriteTool {
    fn name(&self) -> &str {
        ToolName::CsvWrite.as_str()
    }
    fn description(&self) -> &'static str {
        "新しい CSV ファイルを保存する（保存先フォルダの作成権限が要る）。csv には CSV 本文（\
         ヘッダ行＋データ行）を渡す。"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "ファイル名（.csv は自動付与）" },
                "csv": { "type": "string", "description": "CSV 本文" },
                "parent_id": { "type": "string", "format": "uuid", "description": "保存先フォルダ（省略で直下）" }
            },
            "required": ["name", "csv"]
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
        let input: WriteInput = serde_json::from_value(input)
            .map_err(|e| ToolError::Invalid(format!("入力が不正です: {e}")))?;
        if input.name.trim().is_empty() {
            return Err(ToolError::Invalid("ファイル名を指定してください".into()));
        }
        match self
            .tabular
            .save_new(
                ctx,
                input.parent_id,
                input.name.trim(),
                input.csv.as_bytes(),
                trace_id,
            )
            .await
        {
            Ok(s) => Ok(ToolOutcome::ok(format!(
                "CSV「{}」を保存しました（node_id: {}）。",
                s.name, s.node_id
            ))),
            Err(e) => Ok(denied(&e)),
        }
    }
}

/// AI 生成 CSV を**下書き**として用意するツール（csv_draft カード化・Task 11.11）。
///
/// 「表を作って」等の依頼に対し、CSV 本文を**下書き**として返す（この時点では StorageService
/// へは作らない）。フロントは下書き CSV 画面（グリッド）を開き、ユーザーがそこで内容を詰めて
/// から「ドライブに保存」を押して初めて CSV を実体化する（save_note と同型の下書き確定型・
/// issue #282 の状態機械を CSV へ展開）。
///
/// 下書きは**会話内で name をキーに識別**する: 同じ name で呼び直すと同じ下書きが更新され、
/// 別 name なら別の下書きになる。ストレージ書込を伴わないため確認ゲートは不要（確定は UI の
/// 保存ボタンが担う・保存時のパース/クォータは TabularService が最終防壁）。
pub struct SaveCsvTool;

impl SaveCsvTool {
    pub fn new() -> Self {
        SaveCsvTool
    }
}

impl Default for SaveCsvTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct SaveCsvInput {
    /// CSV 名（`.csv` は自動付与）。下書きの識別キーも兼ねる。
    name: String,
    /// CSV 本文（ヘッダ行＋データ行）。
    csv: String,
}

#[async_trait::async_trait]
impl Tool for SaveCsvTool {
    fn name(&self) -> &str {
        ToolName::SaveCsv.as_str()
    }
    fn description(&self) -> &'static str {
        "会話で生成した表データを新しい CSV の下書きとして用意する。ユーザーが「〜の表を作って」\
         「一覧表にして」等と依頼したときに使う。呼ぶと下書き CSV 画面（グリッド）が開き、ユーザー\
         はそこで内容を確認・編集してから自分で「ドライブに保存」して確定する（このツールは保存\
         しない）。内容を直す場合は**同じ name で呼び直す**と同じ下書きが更新される。別の表を\
         同時に作る場合は別の name で呼ぶ。既存 CSV の編集は csv.patch を使うこと。"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "CSV 名（.csv は自動付与）。同じ name で呼び直すと同じ下書きを更新する" },
                "csv": { "type": "string", "description": "CSV 本文（1 行目がヘッダ・RFC4180 のクォート）" }
            },
            "required": ["name", "csv"]
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
        let input: SaveCsvInput = serde_json::from_value(input)
            .map_err(|e| ToolError::Invalid(format!("入力が不正です: {e}")))?;
        let name = input.name.trim();
        if name.is_empty() {
            return Err(ToolError::Invalid("CSV 名を指定してください".into()));
        }
        if input.csv.trim().is_empty() {
            return Err(ToolError::Invalid(
                "csv が空です（ヘッダ行＋データ行を指定してください）".into(),
            ));
        }
        // 表示名は .csv を落として持つ（下書きカード/画面のタイトル用）。保存時に付与する。
        let display_name = name.strip_suffix(".csv").unwrap_or(name);
        let rows = input.csv.lines().filter(|l| !l.trim().is_empty()).count();
        let mut outcome = ToolOutcome::ok(format!(
            "下書き CSV「{display_name}」を用意しました（{rows} 行・ヘッダ含む）。画面で内容を\
             確認・編集し、「ドライブに保存」で確定してください。"
        ));
        outcome.csv_drafts.push(agent_core::CsvDraft {
            name: display_name.to_string(),
            csv: input.csv,
        });
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    //! error 写像（`denied`）と結果整形（`format_table`）の純関数を検証する。
    use super::{denied, format_table};
    use tabular::{RunnerResponse, TabularError as TE};

    #[test]
    fn denied_query_failed_returns_duckdb_reason() {
        let o = denied(&TE::QueryFailed("no column x".into()));
        assert!(o.is_error);
        assert!(
            o.content.contains("SQL の実行に失敗"),
            "理由を素通しする: {}",
            o.content
        );
        assert!(o.content.contains("no column x"));
    }

    #[test]
    fn denied_maps_each_variant_to_fixed_message() {
        assert!(denied(&TE::Forbidden).content.contains("権限がありません"));
        assert!(
            denied(&TE::Authz(authz::AuthzError::InvalidModel("m".into())))
                .content
                .contains("権限がありません")
        );
        assert!(denied(&TE::Storage(storage::StorageError::Forbidden))
            .content
            .contains("権限がありません"));
        assert!(denied(&TE::NotFound("f".into()))
            .content
            .contains("見つかりません"));
        assert!(denied(&TE::Storage(storage::StorageError::NotFound))
            .content
            .contains("見つかりません"));
        assert!(denied(&TE::SqlRejected("ddl".into()))
            .content
            .contains("SQL が拒否されました"));
        assert!(denied(&TE::RevConflict {
            base: 1,
            current: 2
        })
        .content
        .contains("他の編集で更新されています"));
        // フォールバック（_）: クォータ超過等は汎用メッセージへ。
        assert_eq!(
            denied(&TE::QuotaExceeded("mem".into())).content,
            "CSV 操作に失敗しました。"
        );
        assert!(denied(&TE::Runner("proc".into())).is_error);
    }

    fn resp(rows: Vec<Vec<Option<String>>>, total: Option<u64>, truncated: bool) -> RunnerResponse {
        RunnerResponse {
            ok: true,
            columns: vec!["a".into(), "b".into()],
            column_types: vec![],
            rows,
            total_rows: total,
            truncated,
            error: None,
        }
    }

    #[test]
    fn format_table_header_total_and_null_cells() {
        let r = resp(vec![vec![Some("1".into()), None]], Some(42), false);
        let out = format_table(&r, 50);
        assert!(out.contains("列: a, b"));
        assert!(out.contains("総行数: 42"));
        assert!(out.contains("1, "), "NULL セルは空文字へ: {out}");
        assert!(!out.contains("一部のみ表示"));
    }

    #[test]
    fn format_table_marks_preview_when_over_limit_or_truncated() {
        let many = resp(
            (0..5).map(|i| vec![Some(i.to_string()), None]).collect(),
            None,
            false,
        );
        let out = format_table(&many, 2);
        assert!(
            out.contains("一部のみ表示"),
            "max_preview 超過で注記: {out}"
        );
        assert!(!out.contains("総行数"), "total_rows None なら行数表示なし");

        let trunc = resp(vec![vec![Some("x".into()), Some("y".into())]], None, true);
        assert!(format_table(&trunc, 50).contains("一部のみ表示"));
    }

    /// save_csv: 下書きが csv_drafts に載り（保存はしない）、.csv は表示名から落ちる。
    #[tokio::test]
    async fn save_csv_returns_draft_without_saving() {
        use agent_core::Tool as _;
        let ctx = authz::AuthContext::new(
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
        );
        let tool = super::SaveCsvTool::new();
        assert!(!tool.requires_confirmation(), "下書きは確認ゲート不要");
        let out = tool
            .call(
                &ctx,
                serde_json::json!({ "name": "売上一覧.csv", "csv": "a,b\n1,2\n" }),
                None,
            )
            .await
            .unwrap();
        assert_eq!(out.csv_drafts.len(), 1);
        assert_eq!(out.csv_drafts[0].name, "売上一覧", ".csv は落とす");
        assert_eq!(out.csv_drafts[0].csv, "a,b\n1,2\n");
        assert!(out.content.contains("下書き CSV"));

        // 空 csv / 空 name は Invalid で差し戻す（モデルの自己修正へ）。
        assert!(tool
            .call(&ctx, serde_json::json!({ "name": "x", "csv": "  " }), None)
            .await
            .is_err());
        assert!(tool
            .call(&ctx, serde_json::json!({ "name": " ", "csv": "a,b" }), None)
            .await
            .is_err());
    }
}

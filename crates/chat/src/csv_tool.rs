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

//! AI によるワークフロー生成/編集ツール（emit_workflow / read_workflow・Task 10.13）。
//!
//! 「検証済みスペック方式」: モデルが出した IR を
//! **保存 API と同一のパイプライン（V1〜V7＋カタログ照合）**に通し、
//! - 通れば artifact として保存して参照カード（workflow_ref）を外部化する
//! - 通らなければ検証エラー**全件**を is_error の tool_result で返し、モデルに自己修正させる
//!
//! 未検証 IR が保存・表示される経路は存在しない（fail-closed）。認可は artifact 層
//! （発話ユーザーの AuthContext）が担い、ここでは昇格しない（confused-deputy 回避）。

use std::fmt::Write as _;
use std::sync::Arc;

use agent_core::{Tool, ToolError, ToolName, ToolOutcome};
use authz::AuthContext;
use uuid::Uuid;
use workflow_engine::{Catalog, ValidationError, WorkflowStore, WorkflowStoreError};

mod guide;

/// 発話ユーザー視点のカタログ（secret 名→許可ホスト・モデル一覧）を供給する差し込み点。
///
/// カタログの中身は API 層の資産（SecretStore・LLM 設定）なので、chat はトレイト裏で受け取る。
/// **保存 API の `build_catalog` と同一実装を注入すること**（検証結果の乖離をなくす）。
#[async_trait::async_trait]
pub trait WorkflowCatalogSource: Send + Sync {
    async fn catalog(&self, ctx: &AuthContext) -> Result<Catalog, String>;

    /// llm.invoke に書けるモデル（[`Catalog::models`] と同じ集合・空なら照合しない）。
    ///
    /// emit_workflow の説明文に run ごとに載せる。既定はカタログ全体を組んで取り出す（secret・skill の
    /// 一覧も引くので、モデルだけを安く引ける実装は上書きする）。
    async fn models(&self, ctx: &AuthContext) -> Result<Vec<String>, String> {
        self.catalog(ctx).await.map(|c| c.models)
    }
}

/// 検証エラー全件をモデルが自己修正しやすい形に整形する（params の誤りにはそのノード種の形を添える）。
fn format_validation_errors(errors: &[ValidationError], ir: &serde_json::Value) -> String {
    let mut out = format!(
        "ワークフロー IR の検証に失敗しました（{} 件）。全件を修正して emit_workflow を再実行してください。\n",
        errors.len()
    );
    for e in errors {
        let _ = write!(out, "- [{}] {}", e.code, e.message);
        if let Some(node) = &e.node_id {
            let _ = write!(out, "（node: {node}");
            if let Some(path) = &e.path {
                let _ = write!(out, ", path: {path}");
            }
            out.push('）');
        } else if let Some(path) = &e.path {
            let _ = write!(out, "（path: {path}）");
        }
        out.push('\n');
    }
    let hints = guide::validation_hints(errors, ir);
    if !hints.is_empty() {
        out.push_str("補足:\n");
        out.push_str(&hints);
    }
    out
}

/// ワークフロー IR の生成/更新ツール。
pub struct EmitWorkflowTool {
    store: Arc<WorkflowStore>,
    catalog: Arc<dyn WorkflowCatalogSource>,
    description: String,
}

impl EmitWorkflowTool {
    /// 使えるモデルを説明文に載せない版（モデルは検証エラーの候補で知らせる）。
    pub fn new(store: Arc<WorkflowStore>, catalog: Arc<dyn WorkflowCatalogSource>) -> Self {
        EmitWorkflowTool {
            store,
            catalog,
            description: guide::emit_description(None),
        }
    }

    /// 発話ユーザーの組織で使えるモデルを説明文に載せる（run ごとに組む・空なら照合しない旨を書く）。
    #[must_use]
    pub fn with_models(mut self, models: &[String]) -> Self {
        self.description = guide::emit_description(Some(models));
        self
    }
}

#[async_trait::async_trait]
impl Tool for EmitWorkflowTool {
    // requires_confirmation は既定 false のまま: 保存されるのは不変バージョン付き artifact で
    // 旧版に必ず戻せ（fs_write/fs_edit の自動承認と同じ判断基準）、保存しただけでは実行されない
    //（自動実行は別途ユーザー本人の enable 同意が必須・手動実行も本人操作）。カードが可視な
    // 確認面になる。ここを承認ゲートにすると 10.13 の中核 UX（会話から直接生成）が壊れる。
    fn name(&self) -> &str {
        ToolName::EmitWorkflow.as_str()
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "ir": {
                    "type": "object",
                    "description": "ワークフロー IR（JSON DAG）全体"
                },
                "workflow_id": {
                    "type": "string",
                    "description": "更新対象の既存ワークフロー ID（UUID）。新規作成なら省略"
                },
                "expected_version": {
                    "type": "integer",
                    "description": "更新時の楽観ロック。read_workflow が返した version をそのまま渡す（他者の編集と競合したら失敗が返る）"
                }
            },
            "required": ["ir"]
        })
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let Some(ir_json) = input.get("ir").filter(|v| v.is_object()).cloned() else {
            return Err(ToolError::Invalid("ir（object）は必須です".into()));
        };
        let catalog = self
            .catalog
            .catalog(ctx)
            .await
            .map_err(ToolError::Unavailable)?;

        // 保存 API と同一パイプライン（V1〜V7）。検証エラーはモデルへの観測として返す。
        let saved = if let Some(raw_id) = input.get("workflow_id").and_then(|v| v.as_str()) {
            let Ok(id) = Uuid::parse_str(raw_id) else {
                return Err(ToolError::Invalid(format!(
                    "workflow_id が UUID ではありません: {raw_id}"
                )));
            };
            // read_workflow で読んだ version を楽観ロックに使う（他編集者の保存を黙って潰さない）。
            let expected_version = input
                .get("expected_version")
                .and_then(serde_json::Value::as_i64);
            self.store
                .update(ctx, id, &ir_json, &catalog, expected_version, trace_id)
                .await
                .map(|(version, ir)| (id, version, ir))
        } else {
            self.store
                .create(ctx, &ir_json, &catalog, trace_id)
                .await
                .map(|(id, ir)| (id, 1, ir))
        };

        match saved {
            Ok((id, version, ir)) => {
                let display = ir.display_name.clone().unwrap_or_else(|| ir.name.clone());
                let mut text = format!(
                    "ワークフロー「{display}」を保存しました（id: {id}, v{version}）。ユーザーにはカードが表示され、エディタで開けます。"
                );
                // 保存は止めない警告（条件の意味論の切替で分岐の結果が変わり得る条件・Task 15.7・
                // テンプレートの `{{key}}` など）。保存 API と同じ集合。
                let warnings = workflow_engine::save_warnings(&ir);
                if !warnings.is_empty() {
                    text.push_str("\n注意（保存はしました・意図どおりか確かめてください）:");
                    for w in &warnings {
                        let _ = write!(
                            text,
                            "\n- [{}] {}（node: {}）",
                            w.code,
                            w.message,
                            w.node_id.as_deref().unwrap_or("トリガ")
                        );
                    }
                }
                let mut outcome = ToolOutcome::ok(text);
                outcome.workflow_refs.push(serde_json::json!({
                    "id": id,
                    "name": ir.name,
                    "display_name": ir.display_name,
                    "version": version,
                }));
                Ok(outcome)
            }
            Err(WorkflowStoreError::Validation(errors)) => Ok(ToolOutcome::error(
                format_validation_errors(&errors, &ir_json),
            )),
            // 権限不足・競合等もモデルに観測させ、ユーザーへ言語化させる（run は失敗させない）。
            Err(WorkflowStoreError::Artifact(e)) => Ok(ToolOutcome::error(format!(
                "ワークフローの保存に失敗しました: {e}"
            ))),
        }
    }
}

/// 既存ワークフロー IR の読み取りツール（AI 編集の前提）。
pub struct ReadWorkflowTool {
    store: Arc<WorkflowStore>,
}

impl ReadWorkflowTool {
    pub fn new(store: Arc<WorkflowStore>) -> Self {
        ReadWorkflowTool { store }
    }
}

#[async_trait::async_trait]
impl Tool for ReadWorkflowTool {
    fn name(&self) -> &str {
        ToolName::ReadWorkflow.as_str()
    }

    fn description(&self) -> &'static str {
        "既存ワークフローの最新 IR（JSON）を読む。emit_workflow で編集する前に必ず呼び、返った IR を基に変更し、返った version を emit_workflow の expected_version に渡すこと。"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "workflow_id": {
                    "type": "string",
                    "description": "ワークフロー ID（UUID）"
                }
            },
            "required": ["workflow_id"]
        })
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let Some(id) = input
            .get("workflow_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
        else {
            return Err(ToolError::Invalid("workflow_id（UUID）は必須です".into()));
        };
        match self.store.get_latest(ctx, id, trace_id).await {
            Ok((version, ir)) => {
                let ir_json = serde_json::to_value(&ir)
                    .map_err(|e| ToolError::Internal(format!("IR の直列化に失敗: {e}")))?;
                Ok(ToolOutcome::ok(
                    serde_json::json!({ "id": id, "version": version, "ir": ir_json }).to_string(),
                ))
            }
            // 存在しない/読めないはモデルへの観測（存在秘匿は artifact 層のエラー文言に従う）。
            Err(e) => Ok(ToolOutcome::error(format!(
                "ワークフローを読み取れませんでした: {e}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ノードカタログに登録した data ノードは emit_workflow の説明へ自動で載る（Task 15.7・手書きしない）。
    #[test]
    fn emit_description_lists_data_nodes_from_the_catalog() {
        let d = guide::emit_description(None);
        for t in [
            "data.query",
            "data.record.create",
            "data.record.update",
            "data.record.delete",
            "data.transition",
            "data.bulk_upsert",
        ] {
            assert!(d.contains(&format!("- {t}（")), "{t} が説明に無い");
        }
        assert!(d.contains("スコープ data.read。"));
        assert!(d.contains("スコープ data.write。"));
    }

    /// params の誤りには、そのノード種の params の形を添える（LLM が自分で直せるように）。
    #[test]
    fn validation_errors_carry_the_params_shape_of_the_node() {
        let ir = serde_json::json!({
            "ir_version": 1, "name": "wf", "triggers": [{ "kind": "interactive" }],
            "nodes": [{ "id": "read_file", "type": "storage.read", "params": { "id": "x" } }],
            "edges": []
        });
        let errors = workflow_engine::validate(&ir, &Catalog::default()).unwrap_err();
        let text = format_validation_errors(&errors, &ir);
        assert!(text.contains("[ir.schema_violation]"), "{text}");
        assert!(text.contains("node: read_file, path: /params/id"), "{text}");
        assert!(
            text.contains("補足:\nstorage.read の params の形（? は省略可）: { file: ValueExpr }"),
            "{text}"
        );
    }
}

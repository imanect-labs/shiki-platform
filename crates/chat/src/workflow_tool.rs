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

/// ai.decide の model に書ける判定用のモデル（emit_workflow の説明文に id と表示名を載せる・D-79）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionModel {
    /// model に書く id（[`Catalog::decision_models`] の要素）。
    pub id: String,
    /// 管理画面・モデル欄と同じ表示名（引けない実装では `None`・説明文には id だけを載せる）。
    pub label: Option<String>,
}

/// 発話ユーザー視点のカタログ（secret 名→許可ホスト・モデル一覧）を供給する差し込み点。
///
/// カタログの中身は API 層の資産（SecretStore・LLM 設定）なので、chat はトレイト裏で受け取る。
/// **保存 API の `build_catalog` と同一実装を注入すること**（検証結果の乖離をなくす）。
#[async_trait::async_trait]
pub trait WorkflowCatalogSource: Send + Sync {
    async fn catalog(&self, ctx: &AuthContext) -> Result<Catalog, String>;

    /// llm.invoke に書けるモデル（[`Catalog::models`] と同じ集合・照合しない構成と許可が 0 件はどちらも空）。
    ///
    /// emit_workflow の説明文に run ごとに載せる。空なら説明文は model を書かないよう案内する（省略は
    /// 組織の既定で動く）。既定はカタログ全体を組んで取り出す（secret・skill の一覧も引くので、
    /// モデルだけを安く引ける実装は上書きする）。
    async fn models(&self, ctx: &AuthContext) -> Result<Vec<String>, String> {
        self.catalog(ctx)
            .await
            .map(|c| c.models.unwrap_or_default())
    }

    /// ai.decide に書ける判定用のモデル（[`Catalog::decision_models`] と同じ集合・照合しない構成と許可が 0 件は
    /// どちらも空・D-79）。
    ///
    /// emit_workflow の説明文に run ごとに id と表示名を載せる（保存時の誤りの候補で知る往復を減らす）。既定は
    /// カタログ全体を組んで id だけを取り出す（表示名は無い）。表示名を引ける実装・判定用のモデルだけを安く
    /// 引ける実装は上書きする。
    async fn decision_models(&self, ctx: &AuthContext) -> Result<Vec<DecisionModel>, String> {
        self.catalog(ctx).await.map(|c| {
            c.decision_models
                .unwrap_or_default()
                .into_iter()
                .map(|id| DecisionModel { id, label: None })
                .collect()
        })
    }

    /// 保存しようとしている IR に合わせたカタログ（IR が参照するものだけを引いて足す・承認のテーブルなど）。
    ///
    /// 保存 API は IR を見て承認（human.approval）が束縛するテーブルを [`Catalog::tables`] に詰め、V4 で FSM・
    /// 状態・列を照合する。emit_workflow も同じ照合を通すため、IR を渡して組む（既定は IR を見ない
    /// [`Self::catalog`]。照合の材料を IR から引く実装は上書きする）。
    async fn catalog_for(
        &self,
        ctx: &AuthContext,
        _ir: &serde_json::Value,
    ) -> Result<Catalog, String> {
        self.catalog(ctx).await
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
        } else if let Some(edge) = &e.edge {
            // 線の誤り（領域をまたぐ線など）は、文言がブロックの名前で言うので、id は線で添える。
            let _ = write!(out, "（edge: {edge}）");
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
    /// 説明文に載せる llm.invoke のモデル（`None` は載せない）。
    models: Option<Vec<String>>,
    /// 説明文に載せる ai.decide の判定用のモデル（`None` は載せない）。
    decision_models: Option<Vec<DecisionModel>>,
    description: String,
}

impl EmitWorkflowTool {
    /// 使えるモデルを説明文に載せない版（モデルは検証エラーの候補で知らせる）。
    pub fn new(store: Arc<WorkflowStore>, catalog: Arc<dyn WorkflowCatalogSource>) -> Self {
        EmitWorkflowTool {
            store,
            catalog,
            models: None,
            decision_models: None,
            description: guide::emit_description(None, None),
        }
    }

    /// 発話ユーザーの組織で使えるモデルを説明文に載せる（run ごとに組む・空なら model を書かない旨を書く）。
    #[must_use]
    pub fn with_models(mut self, models: &[String]) -> Self {
        self.models = Some(models.to_vec());
        self.rebuild_description();
        self
    }

    /// 発話ユーザーの組織で使える判定用のモデル（id と表示名）を説明文に載せる（run ごとに組む・D-79）。
    #[must_use]
    pub fn with_decision_models(mut self, models: &[DecisionModel]) -> Self {
        self.decision_models = Some(models.to_vec());
        self.rebuild_description();
        self
    }

    fn rebuild_description(&mut self) {
        self.description =
            guide::emit_description(self.models.as_deref(), self.decision_models.as_deref());
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
            .catalog_for(ctx, &ir_json)
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
                let warnings = workflow_engine::save_warnings(&ir, &catalog);
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
        let d = guide::emit_description(None, None);
        for t in [
            "data.query",
            "data.record.create",
            "data.record.update",
            "data.record.upsert",
            "data.record.delete",
            "data.transition",
            "data.bulk_upsert",
        ] {
            assert!(d.contains(&format!("- {t}（")), "{t} が説明に無い");
        }
        assert!(d.contains("スコープ data.read。"));
        assert!(d.contains("スコープ data.write。"));
    }

    /// 判定用のモデルの一覧の既定実装は、カタログの判定用のモデルを id だけで返す（表示名を引けない実装・D-79）。
    /// emit_workflow はそれを説明文に載せ、llm.invoke のモデルと並べて組み直す。
    #[tokio::test]
    async fn decision_models_default_to_the_catalog_ids_and_reach_the_description() {
        struct Fixed;
        #[async_trait::async_trait]
        impl WorkflowCatalogSource for Fixed {
            async fn catalog(&self, _ctx: &AuthContext) -> Result<Catalog, String> {
                Ok(Catalog {
                    models: Some(vec!["fast".into()]),
                    decision_models: Some(vec!["judge".into()]),
                    ..Catalog::default()
                })
            }
        }
        let ctx = AuthContext::for_workflow("t".into(), "o".into(), "wf");
        let listed = Fixed.decision_models(&ctx).await.unwrap();
        assert_eq!(
            listed,
            [DecisionModel {
                id: "judge".into(),
                label: None
            }]
        );
        let models = Fixed.models(&ctx).await.unwrap();
        let both = guide::emit_description(Some(&models), Some(&listed));
        assert!(
            both.contains("使えるもの（id（表示名））: judge。"),
            "{both}"
        );
        assert!(
            both.contains("のどれかを書く（このユーザーの組織で使えるもの）: fast。"),
            "{both}"
        );
        // 判定用のモデルを載せないときは、従来どおり検証エラーの候補に任せる。
        let without = guide::emit_description(Some(&models), None);
        assert!(
            without.contains("使えるものは検証エラーの候補に出る"),
            "{without}"
        );
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

    /// 線の誤り（繰り返しの中と外をまたぐ線）は文言がブロックの名前で言うので、どの線かを id で添える。
    #[test]
    fn edge_errors_name_the_edge_by_id() {
        let ir = serde_json::json!({
            "ir_version": 1, "name": "wf", "triggers": [{ "kind": "interactive" }],
            "nodes": [
                { "id": "each_row", "type": "control.map",
                  "params": { "items": { "$from": "input", "path": "/rows" } } },
                { "id": "inner", "type": "script.run", "parent": "each_row",
                  "params": { "source": { "inline": "function main(i){ return i; }" } } },
                { "id": "after", "type": "script.run",
                  "params": { "source": { "inline": "function main(i){ return i; }" } } }
            ],
            "edges": [{ "from": "inner", "to": "after" }]
        });
        let errors = workflow_engine::validate(&ir, &Catalog::default()).unwrap_err();
        let text = format_validation_errors(&errors, &ir);
        assert!(text.contains("[ir.region_leak]"), "{text}");
        assert!(text.contains("（edge: inner -> after）"), "{text}");
    }
}

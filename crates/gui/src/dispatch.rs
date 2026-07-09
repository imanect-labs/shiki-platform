//! 宣言的アクションの実行系（Task 6.5）。
//!
//! UI からの操作は、**保存済み検証済みスペックの束縛**（[`ActionBinding`]）を API 層が引き、
//! ここで照合・実行する。クライアントが送れるのは `action_id + params` のみで、束縛定義は
//! 一切信用しない（アンビエント権限なし）。実行は常に**呼び出しユーザー自身の権限**
//! （`AuthContext`）で認可され、全経路が `ui_action.invoke` として監査に残る（Task 6.12）。

use std::collections::HashMap;
use std::sync::Arc;

use agent_core::{Tool, ToolName};
use authz::AuthContext;
use serde_json::json;
use sha2::{Digest, Sha256};
use storage::audit::{AuditEntry, AuditRecorder, Decision};
use uuid::Uuid;

use crate::action::{ActionBinding, ALLOWED_ACTION_TOOLS};
use crate::spec::UiSpecDoc;
use crate::vocab::HandlerKind;

/// params の直列化サイズ上限（防御的・フォーム値として十分）。
const MAX_PARAMS_BYTES: usize = 64 * 1024;

/// アクションの発生源（監査・ハンドラの文脈）。
#[derive(Debug, Clone)]
pub enum ActionSource {
    /// チャットメッセージ内の generative_ui ブロック。
    ChatMessage { thread_id: Uuid, message_id: Uuid },
    /// ミニアプリの UI スペック（バージョンピン済み）。
    MiniApp { artifact_id: Uuid, version: i64 },
}

impl ActionSource {
    fn audit_json(&self) -> serde_json::Value {
        match self {
            ActionSource::ChatMessage {
                thread_id,
                message_id,
            } => {
                json!({ "kind": "chat_message", "thread_id": thread_id, "message_id": message_id })
            }
            ActionSource::MiniApp {
                artifact_id,
                version,
            } => json!({ "kind": "mini_app", "artifact_id": artifact_id, "version": version }),
        }
    }
}

/// アクション実行のエラー（API 層が HTTP へ写す）。
#[derive(Debug, thiserror::Error)]
pub enum ActionError {
    #[error("対象が見つかりません")]
    NotFound,
    #[error("権限がありません")]
    Forbidden,
    #[error("不正なリクエスト: {0}")]
    Invalid(String),
    #[error("利用できません: {0}")]
    Unavailable(String),
    #[error("内部エラー: {0}")]
    Internal(String),
}

/// 明示登録のサーバ側ハンドラ（Task 6.5 の②・閉集合）。
///
/// 実装は所有ドメイン側（chat の `ChatSubmitHandler` 等）に置き、**内部で必ず既存
/// チョークポイント（ChatStore 等）の本人認可を通る**こと。
#[async_trait::async_trait]
pub trait ActionHandler: Send + Sync {
    fn kind(&self) -> HandlerKind;
    async fn invoke(
        &self,
        ctx: &AuthContext,
        source: &ActionSource,
        params: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<serde_json::Value, ActionError>;
}

/// workflow-engine 対話トリガの起動口（Task 6.5 の③・api 層がアダプタを実装）。
///
/// 実装は `start_interactive` 系（本人 ReBAC で IR 取得認可・実行時は
/// scope_ceiling ∩ 本人 ReBAC の二重ゲート）へ委譲すること。
#[async_trait::async_trait]
pub trait WorkflowStarter: Send + Sync {
    async fn start_pinned(
        &self,
        ctx: &AuthContext,
        workflow_id: Uuid,
        version: i64,
        input: &serde_json::Value,
    ) -> Result<Option<Uuid>, ActionError>;
}

/// アクション実行の合流点（照合・認可・監査の単一チョークポイント）。
pub struct ActionDispatcher {
    handlers: HashMap<HandlerKind, Arc<dyn ActionHandler>>,
    tools: HashMap<&'static str, Arc<dyn Tool>>,
    workflows: Option<Arc<dyn WorkflowStarter>>,
    audit: AuditRecorder,
}

impl ActionDispatcher {
    pub fn new(audit: AuditRecorder) -> Self {
        ActionDispatcher {
            handlers: HashMap::new(),
            tools: HashMap::new(),
            workflows: None,
            audit,
        }
    }

    /// ハンドラを登録する（[`HandlerKind`] の閉語彙）。
    pub fn register_handler(&mut self, handler: Arc<dyn ActionHandler>) {
        self.handlers.insert(handler.kind(), handler);
    }

    /// UI アクションとして呼べるツールを登録する。
    ///
    /// [`ALLOWED_ACTION_TOOLS`] 外・破壊系（`requires_confirmation`）は**登録自体を拒否**する
    /// （検証層と独立した二重防御・fail-closed）。
    pub fn register_tool(&mut self, name: ToolName, tool: Arc<dyn Tool>) {
        if !ALLOWED_ACTION_TOOLS.contains(&name) || tool.requires_confirmation() {
            tracing::error!(
                tool = name.as_str(),
                "UI アクションに登録できないツールを拒否"
            );
            return;
        }
        self.tools.insert(name.as_str(), tool);
    }

    pub fn set_workflow_starter(&mut self, starter: Arc<dyn WorkflowStarter>) {
        self.workflows = Some(starter);
    }

    /// 宣言済み束縛（検証済み文書）から `action_id` を照合し、本人権限で実行する。
    ///
    /// 未宣言 id・認可失敗・実行失敗は全て Deny として監査に残す。
    pub async fn dispatch(
        &self,
        ctx: &AuthContext,
        source: &ActionSource,
        doc: &UiSpecDoc,
        action_id: &str,
        params: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<serde_json::Value, ActionError> {
        let Some(binding) = doc.actions.iter().find(|b| b.id() == action_id) else {
            self.deny(ctx, source, action_id, "undeclared_action", trace_id)
                .await;
            return Err(ActionError::NotFound);
        };
        if serde_json::to_vec(&params).map_or(0, |v| v.len()) > MAX_PARAMS_BYTES {
            self.deny(ctx, source, action_id, "params_too_large", trace_id)
                .await;
            return Err(ActionError::Invalid("params が大きすぎます".into()));
        }
        let params_digest = format!("{:x}", Sha256::digest(params.to_string().as_bytes()));

        let result = self.execute(ctx, source, binding, params, trace_id).await;
        match &result {
            Ok(output) => {
                let run_id = output.get("run_id").cloned().unwrap_or(json!(null));
                self.record(
                    ctx,
                    action_id,
                    Decision::Allow,
                    trace_id,
                    json!({
                        "source": source.audit_json(),
                        "binding": binding.kind_str(),
                        "params_sha256": params_digest,
                        "run_id": run_id,
                    }),
                )
                .await;
            }
            Err(e) => {
                self.record(
                    ctx,
                    action_id,
                    Decision::Deny,
                    trace_id,
                    json!({
                        "source": source.audit_json(),
                        "binding": binding.kind_str(),
                        "params_sha256": params_digest,
                        "reason": e.to_string(),
                    }),
                )
                .await;
            }
        }
        result
    }

    /// 束縛照合前の拒否（未宣言 id 等）を監査に残す（API 層からも利用できる）。
    pub async fn deny(
        &self,
        ctx: &AuthContext,
        source: &ActionSource,
        action_id: &str,
        reason: &str,
        trace_id: Option<&str>,
    ) {
        self.record(
            ctx,
            action_id,
            Decision::Deny,
            trace_id,
            json!({ "source": source.audit_json(), "reason": reason }),
        )
        .await;
    }

    async fn execute(
        &self,
        ctx: &AuthContext,
        source: &ActionSource,
        binding: &ActionBinding,
        params: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<serde_json::Value, ActionError> {
        match binding {
            ActionBinding::Handler(b) => {
                let handler = self.handlers.get(&b.handler).ok_or_else(|| {
                    ActionError::Unavailable(format!(
                        "ハンドラ '{}' は無効です",
                        b.handler.as_str()
                    ))
                })?;
                let result = handler.invoke(ctx, source, params, trace_id).await?;
                Ok(json!({ "kind": "handler", "result": result }))
            }
            ActionBinding::Tool(b) => {
                // 検証層と独立の二重防御: 許可リスト外は保存済みスペックでも実行しない。
                if !ALLOWED_ACTION_TOOLS.contains(&b.tool) {
                    return Err(ActionError::Forbidden);
                }
                let tool = self.tools.get(b.tool.as_str()).ok_or_else(|| {
                    ActionError::Unavailable(format!("ツール '{}' は無効です", b.tool.as_str()))
                })?;
                if tool.requires_confirmation() {
                    return Err(ActionError::Forbidden);
                }
                // ツールは本人 ctx で実行（doc_search は二段 authz を内部で通る）。
                let outcome = tool
                    .call(ctx, params, trace_id)
                    .await
                    .map_err(|e| ActionError::Unavailable(format!("tool: {e}")))?;
                Ok(json!({
                    "kind": "tool",
                    "ok": !outcome.is_error,
                    "content": outcome.content,
                }))
            }
            ActionBinding::Workflow(b) => {
                let starter = self
                    .workflows
                    .as_ref()
                    .ok_or_else(|| ActionError::Unavailable("workflow 実行時が無効です".into()))?;
                // 保存済みスペックは解決済み（ピン必須）。欠落は不正データとして拒否。
                let (Some(id), Some(version)) = (b.workflow.artifact_id, b.workflow.version) else {
                    return Err(ActionError::Invalid(
                        "workflow 束縛が未解決です（検証済みスペックではありません）".into(),
                    ));
                };
                let run_id = starter.start_pinned(ctx, id, version, &params).await?;
                Ok(json!({ "kind": "workflow", "run_id": run_id }))
            }
        }
    }

    async fn record(
        &self,
        ctx: &AuthContext,
        action_id: &str,
        decision: Decision,
        trace_id: Option<&str>,
        metadata: serde_json::Value,
    ) {
        let entry = AuditEntry {
            action: "ui_action.invoke",
            object_type: "ui_action",
            object_id: action_id,
            decision,
            trace_id,
            metadata,
        };
        if let Err(e) = self.audit.record(ctx, entry).await {
            tracing::warn!(error = %e, "ui_action.invoke の監査記録に失敗");
        }
    }
}

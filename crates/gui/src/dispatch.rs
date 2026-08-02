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
    /// 1 回だけ実行できるアクションが既に実行済み（#410・API 層は 409）。
    #[error("この操作は送信済みです")]
    AlreadyInvoked,
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

    /// ミニアプリの**バンドル権限**で IR を読んで起動する（Task 6.10）。
    ///
    /// バンドル本体だけを共有された利用者が、部品 workflow の個別共有なしにピン版を
    /// 起動できる（実行主体は本人のまま・読取は `artifact.read_via_bundle` 監査）。
    async fn start_pinned_via_bundle(
        &self,
        ctx: &AuthContext,
        bundle_id: Uuid,
        workflow_id: Uuid,
        version: i64,
        input: &serde_json::Value,
    ) -> Result<Option<Uuid>, ActionError>;
}

/// 1 回だけ実行できるアクション（[`ActionBinding::single_use`]）の実行台帳（#410）。
///
/// 「どのメッセージのどの action が実行されたか」は**UI の状態そのもの**であり、
/// 監査ログ（追記専用・保持ポリシ別）とは別に一級の状態として持つ。実装は所有ドメイン側
/// （chat の `ChatActionLedger`）に置き、`AuthContext` のテナントで必ず絞る。
#[async_trait::async_trait]
pub trait ActionLedger: Send + Sync {
    /// **実行前に**確保する。既に実行済みなら `Ok(false)`（実行してはいけない）。
    async fn claim(
        &self,
        ctx: &AuthContext,
        source: &ActionSource,
        action_id: &str,
    ) -> Result<bool, ActionError>;

    /// 実行に失敗したときに確保を解く（押し直せる状態へ戻す・best-effort）。
    async fn release(&self, ctx: &AuthContext, source: &ActionSource, action_id: &str);

    /// **実行が完了した**ことを記録する（`run_id` があれば紐づける・best-effort）。
    ///
    /// 確保はハンドラ実行の前に取るので、確保と完了は別の事実として持つ。UI へ
    /// 「送信済み」と見せてよいのは完了した方だけ。
    async fn complete(
        &self,
        ctx: &AuthContext,
        source: &ActionSource,
        action_id: &str,
        run_id: Option<Uuid>,
    );
}

/// アクション実行の合流点（照合・認可・監査の単一チョークポイント）。
pub struct ActionDispatcher {
    handlers: HashMap<HandlerKind, Arc<dyn ActionHandler>>,
    tools: HashMap<&'static str, Arc<dyn Tool>>,
    workflows: Option<Arc<dyn WorkflowStarter>>,
    ledger: Option<Arc<dyn ActionLedger>>,
    audit: AuditRecorder,
}

impl ActionDispatcher {
    pub fn new(audit: AuditRecorder) -> Self {
        ActionDispatcher {
            handlers: HashMap::new(),
            tools: HashMap::new(),
            workflows: None,
            ledger: None,
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

    /// 単発アクションの実行台帳を配線する（#410）。
    ///
    /// 未配線のまま単発束縛が来たら**実行せず 503**にする（二重送信の抑止が無い状態で
    /// 発話と run を作らせない・fail-closed）。台帳と `chat.submit` ハンドラは同じ
    /// `chat` 由来なので、配線は必ず対で行う。
    pub fn set_ledger(&mut self, ledger: Arc<dyn ActionLedger>) {
        self.ledger = Some(ledger);
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

        // 1 回だけの束縛は**実行前に確保**する。表示だけ直しても押せてしまうので、
        // 二重送信はここで潰す（#410）。確保できなければ実行そのものを行わない。
        // 台帳側の失敗（未配線・DB エラー）も実行を止める側なので Deny として残す
        // — 公開 API で操作が拒否された事実は理由込みで追えるようにする。
        let ledger = match self.single_use_ledger(binding) {
            Ok(ledger) => ledger,
            Err(e) => {
                self.deny(ctx, source, action_id, "ledger_unavailable", trace_id)
                    .await;
                return Err(e);
            }
        };
        if let Some(ledger) = ledger {
            match ledger.claim(ctx, source, action_id).await {
                Ok(true) => {}
                Ok(false) => {
                    self.deny(ctx, source, action_id, "already_invoked", trace_id)
                        .await;
                    return Err(ActionError::AlreadyInvoked);
                }
                Err(e) => {
                    self.deny(ctx, source, action_id, "claim_failed", trace_id)
                        .await;
                    return Err(e);
                }
            }
        }

        let result = self.execute(ctx, source, binding, params, trace_id).await;
        match &result {
            Ok(output) => {
                // workflow はトップレベル、handler は result 内（chat.submit の run_id 等）に持つ。
                let run_id = output
                    .get("run_id")
                    .or_else(|| output.get("result").and_then(|r| r.get("run_id")))
                    .cloned()
                    .unwrap_or(json!(null));
                // ここで初めて「実行された」ことが確定する（確保だけの行は UI に出さない）。
                if let Some(ledger) = ledger {
                    let id = run_id.as_str().and_then(|s| Uuid::parse_str(s).ok());
                    ledger.complete(ctx, source, action_id, id).await;
                }
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
                // 実行できなかったものを「送信済み」にしたままにしない（押し直せる）。
                if let Some(ledger) = ledger {
                    ledger.release(ctx, source, action_id).await;
                }
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

    /// 単発束縛なら台帳を返す（未配線は fail-closed で 503）。繰り返せる束縛は `None`。
    fn single_use_ledger(
        &self,
        binding: &ActionBinding,
    ) -> Result<Option<&Arc<dyn ActionLedger>>, ActionError> {
        if !binding.single_use() {
            return Ok(None);
        }
        let Some(ledger) = self.ledger.as_ref() else {
            tracing::error!(
                binding = binding.kind_str(),
                "単発アクションの実行台帳が未配線（二重送信を抑止できないため実行しない）"
            );
            return Err(ActionError::Unavailable(
                "この操作はいま実行できません".into(),
            ));
        };
        Ok(Some(ledger))
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
                // ミニアプリ由来はバンドル権限で IR を読む（保存時に束縛 ⊆ バンドルのピン集合を
                // 照合済み・resolve で再検証済みのスペックのみここへ来る）。チャット由来は本人の
                // viewer 権限のまま（発話者がピンした版＝本人が読める版）。
                let run_id = match source {
                    ActionSource::MiniApp { artifact_id, .. } => {
                        starter
                            .start_pinned_via_bundle(ctx, *artifact_id, id, version, &params)
                            .await?
                    }
                    ActionSource::ChatMessage { .. } => {
                        starter.start_pinned(ctx, id, version, &params).await?
                    }
                };
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

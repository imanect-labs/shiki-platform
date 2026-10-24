//! 宣言的アクションの実行系（Task 6.5・#604）。
//!
//! 図のボタン（Yui の ```ui）からの操作は、**サーバーが保存済みの本文と束縛から引いた**
//! [`ActionBinding`] をここで実行する。クライアントが送れるのは `action_id + params` のみで、
//! 束縛定義は一切信用しない（アンビエント権限なし）。実行は常に**呼び出しユーザー自身の権限**
//! （`AuthContext`）で認可され、全経路が `ui_action.invoke` として監査に残る（Task 6.12）。

use std::collections::HashMap;
use std::sync::Arc;

use authz::AuthContext;
use serde_json::json;
use sha2::{Digest, Sha256};
use storage::audit::{AuditEntry, AuditRecorder, Decision};
use uuid::Uuid;

use crate::action::ActionBinding;
use crate::vocab::HandlerKind;

/// params の直列化サイズ上限（防御的・フォーム値として十分）。
const MAX_PARAMS_BYTES: usize = 64 * 1024;

/// アクションの発生源（監査・ハンドラの文脈）。
#[derive(Debug, Clone)]
pub enum ActionSource {
    /// 会話の回答の本文の図。
    ChatMessage { thread_id: Uuid, message_id: Uuid },
    /// ミニアプリの画面（バージョンピン済み）。
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
    /// 表（保存ビュー）の並べ替え・絞り込みに使う列の索引を準備中（失敗ではない待ち・Task 15.5）。
    /// API 層は data の API と同じ構造化した 409（`code = index_preparing`）にし、画面は進捗を出して読み直す。
    #[error("列「{field}」の索引を準備中です")]
    IndexPreparing {
        field: String,
        progress: Option<f64>,
    },
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
    /// **実行前に**確保する。既に確保されていれば `Ok(false)`（実行してはいけない）。
    ///
    /// 実装は**副作用の無い認可**（chat なら thread editor）をここで済ませること。実行時の
    /// 認可まで待つと、権限の無い相手でも確保だけは取れてしまう。
    async fn claim(
        &self,
        ctx: &AuthContext,
        source: &ActionSource,
        action_id: &str,
        trace_id: Option<&str>,
    ) -> Result<bool, ActionError>;

    /// 実行に失敗したときに確保を解く（押し直せる状態へ戻す・best-effort）。
    async fn release(&self, ctx: &AuthContext, source: &ActionSource, action_id: &str);

    /// **実行が完了した**ことを記録する（`run_id` があれば紐づける・best-effort）。
    ///
    /// 確保はハンドラ実行の前に取るので、確保と完了は別の事実として持つ。完了できなくても
    /// 実行は成功しているため、確保はそのまま残す（奪わせない）。
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
    workflows: Option<Arc<dyn WorkflowStarter>>,
    /// 保存ビューの実行（`data_grid` のデータ束縛・Task 15.7）。未配線なら 503。
    data_views: Option<data::DataViewStore>,
    ledger: Option<Arc<dyn ActionLedger>>,
    audit: AuditRecorder,
}

impl ActionDispatcher {
    pub fn new(audit: AuditRecorder) -> Self {
        ActionDispatcher {
            handlers: HashMap::new(),
            workflows: None,
            data_views: None,
            ledger: None,
            audit,
        }
    }

    /// ハンドラを登録する（[`HandlerKind`] の閉語彙）。
    pub fn register_handler(&mut self, handler: Arc<dyn ActionHandler>) {
        self.handlers.insert(handler.kind(), handler);
    }

    pub fn set_workflow_starter(&mut self, starter: Arc<dyn WorkflowStarter>) {
        self.workflows = Some(starter);
    }

    /// 保存ビューの実行系を配線する（`data_grid` のデータ束縛・Task 15.7）。
    pub fn set_data_views(&mut self, views: data::DataViewStore) {
        self.data_views = Some(views);
    }

    /// 単発アクションの実行台帳を配線する（#410）。
    ///
    /// 未配線のまま単発束縛が来たら**実行せず 503**にする（二重送信の抑止が無い状態で
    /// 発話と run を作らせない・fail-closed）。台帳と `chat.submit` ハンドラは同じ
    /// `chat` 由来なので、配線は必ず対で行う。
    pub fn set_ledger(&mut self, ledger: Arc<dyn ActionLedger>) {
        self.ledger = Some(ledger);
    }

    /// サーバーが引いた束縛をそのまま実行する（会話の図のボタン: 束縛は回答の確定時に
    /// 保存した宣言、または本文から読んだ「会話に送る」）。`action_id` は監査と台帳の鍵。
    ///
    /// 束縛は**呼び出し側がサーバーの状態から作ったもの**に限る（クライアントの値を束縛に
    /// しない）。params の上限・単発の確保・監査をここで一手に行う。
    pub async fn dispatch_binding(
        &self,
        ctx: &AuthContext,
        source: &ActionSource,
        binding: &ActionBinding,
        action_id: &str,
        params: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<serde_json::Value, ActionError> {
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
            match ledger.claim(ctx, source, action_id, trace_id).await {
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
                // ここで初めて「実行された」ことが確定する。確保だけの行も UI へは送信済みと
                // して出る（確保は奪わないので、そのカードはもう押せない）。この記録は監査との
                // 突合と、詰まりの発見（未完了のまま古くなった行）に使う。
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

    /// 実行せずに前の結果を返した（同じ冪等キーの再送）ことを監査に残す。
    ///
    /// 副作用は起こしていないが、送信があった事実は Allow として残す（再送・競合の試行が
    /// 監査から消えないように）。
    pub async fn record_replay(
        &self,
        ctx: &AuthContext,
        source: &ActionSource,
        action_id: &str,
        run_id: Option<Uuid>,
        trace_id: Option<&str>,
    ) {
        self.record(
            ctx,
            action_id,
            Decision::Allow,
            trace_id,
            json!({
                "source": source.audit_json(),
                "binding": "workflow",
                "replay": true,
                "run_id": run_id,
            }),
        )
        .await;
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
            ActionBinding::Workflow(b) => {
                let starter = self
                    .workflows
                    .as_ref()
                    .ok_or_else(|| ActionError::Unavailable("workflow 実行時が無効です".into()))?;
                // 束縛は解決済み（ピン必須）。欠落は不正データとして拒否。
                let (Some(id), Some(version)) = (b.workflow.artifact_id, b.workflow.version) else {
                    return Err(ActionError::Invalid("workflow 束縛が未解決です".into()));
                };
                // ミニアプリ由来はバンドル権限で IR を読む（保存時に画面のボタン ⊆ バンドルの
                // ピン集合を照合済み・resolve で再検証済みの画面のみここへ来る）。チャット由来は
                // 本人の viewer 権限のまま（回答の確定時に発話者がピンした版＝本人が読める版）。
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
            ActionBinding::DataView(b) => {
                let views = self
                    .data_views
                    .as_ref()
                    .ok_or_else(|| ActionError::Unavailable("表の描画が無効です".into()))?;
                // 画面から送れるのはページ位置と件数だけ（条件・並べ替えはピンしたビューの定義）。
                let page: data::DataViewPage = if params.is_null() {
                    data::DataViewPage::default()
                } else {
                    serde_json::from_value(params)
                        .map_err(|e| ActionError::Invalid(format!("表の params が不正です: {e}")))?
                };
                // 定義の読み方は発生源で変わる（ミニアプリはバンドル権限・会話は本人の viewer）が、
                // **データは常に閲覧者本人の権限**で毎回評価する（作成者・保存者の権限を引き継がない）。
                let run = match source {
                    ActionSource::MiniApp { artifact_id, .. } => {
                        views
                            .run_via_bundle(
                                ctx,
                                *artifact_id,
                                b.view_id,
                                b.pinned_version,
                                &page,
                                trace_id,
                            )
                            .await
                    }
                    ActionSource::ChatMessage { .. } => {
                        views
                            .run_pinned(ctx, b.view_id, b.pinned_version, &page, trace_id)
                            .await
                    }
                }
                .map_err(map_data_err)?;
                let result = serde_json::to_value(&run)
                    .map_err(|e| ActionError::Internal(format!("表の結果: {e}")))?;
                Ok(json!({ "kind": "data_view", "result": result }))
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

/// data のエラーを操作のエラーへ（存在秘匿: 見えないテーブル・ビューは区別しない）。
fn map_data_err(e: data::DataError) -> ActionError {
    use data::DataError as DE;
    match e {
        DE::NotFound => ActionError::NotFound,
        DE::Forbidden => ActionError::Forbidden,
        DE::Invalid(m) => ActionError::Invalid(m),
        DE::InvalidInput(e) => ActionError::Invalid(e.message().to_string()),
        DE::NameTaken => ActionError::Invalid("同じ名前のテーブルが既にあります".into()),
        DE::Rejected(r) => ActionError::Invalid(r.to_string()),
        DE::Gone(m) => ActionError::Invalid(format!("ページの位置が古くなりました: {m}")),
        DE::IndexPreparing { field, progress } => ActionError::IndexPreparing { field, progress },
        DE::ReadOverloaded | DE::ReadTimeout | DE::QueryTooExpensive { .. } => {
            ActionError::Unavailable(e.to_string())
        }
        other => ActionError::Internal(other.to_string()),
    }
}

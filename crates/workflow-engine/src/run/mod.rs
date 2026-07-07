//! run/step 実行エンジン（Task 10.2・engine.md §2〜§4）。
//!
//! - [`model`]: run/step 状態機械・冪等キー。
//! - [`readiness`]: DAG 前進の readiness/skip 判定（純関数）。
//! - [`graph`]: IR から前進に必要な隣接情報を前計算する。
//! - [`store`]: run/step の永続化（durable の claim/リース/fencing に載る）＋前進 TX。
//! - [`worker`]: ready step を claim して実行し前進させるワーカーループ。
//!
//! ノード種ごとの実処理は [`NodeExecutor`] トレイト裏（能力ノードは Task 10.6a/10.8/10.10）。
//! 制御ノード（branch/join 等）は [`store`] の前進ロジックが直接扱う（能力を呼ばない・pure）。

pub mod graph;
pub mod launcher;
pub mod model;
pub mod readiness;
pub mod store;
pub mod worker;

use async_trait::async_trait;
use serde_json::Value;
use uuid::Uuid;

pub use launcher::{LauncherError, WorkflowRunLauncher};
pub use model::{idempotency_key, RunStatus, StepStatus};
pub use store::{RunStore, RunStoreError};
pub use worker::{WorkerConfig, WorkflowWorker};

/// ノード実行のコンテキスト（NodeContext・engine.md §6.5/§7.2）。
///
/// AuthContext・scope_ceiling・冪等キーを運ぶ器。能力ノードはこれを通じて既存チョークポイントへ
/// 合流する（個別ノードに認可を散らさない）。Stage A の本 PR では実行入力の受け渡しに使う。
#[derive(Debug, Clone)]
pub struct NodeContext {
    pub tenant_id: String,
    pub org: String,
    pub run_id: Uuid,
    pub step_path: String,
    /// 冪等キー（attempt 非依存）。
    pub idempotency_key: String,
    /// 現在の attempt。
    pub attempt: i32,
    /// 実行主体の subject local id。
    pub principal: String,
    /// 実行主体の種別（'user' or 'workflow'）。interactive=user・schedule/event=workflow。
    pub principal_kind: String,
    /// run 起動入力（`$from input` の源）。
    pub input: Value,
    /// トリガペイロード（`$from trigger` の源。Stage A の interactive は run 入力と同一）。
    pub trigger: Value,
    /// 先行して成功した step の `node_id → output` マップ（`$from nodes.<id>.output` の源）。
    pub node_outputs: Value,
    /// OTel トレース id（監査 ↔ Langfuse ↔ OTel を束ねる・run から伝播）。
    pub trace_id: Option<String>,
    /// scope ceiling（= IR の declared_scopes ∩ ノード設定）。能力ゲートウェイが
    /// 「操作の要求スコープ ∈ scope_ceiling」を検証してから OpenFGA check を行う（二重ゲート）。
    /// run 開始時に declared ⊆ 委譲 が保証されるため実効スコープ = declared_scopes。
    pub scope_ceiling: Vec<String>,
}

/// ノード実行の結果（成功/失敗と出力ポート）。
#[derive(Debug, Clone)]
pub struct NodeResult {
    /// 成否。
    pub ok: bool,
    /// 出力（成功時・次ノードへ渡る）。
    pub output: Value,
    /// 確定した出力ポート（通常 `["out"]`、branch は `["true"]`/`["false"]` 等）。
    pub taken_ports: Vec<String>,
    /// 失敗時のエラー（`{code, message, retryable}`）。
    pub error: Option<NodeError>,
}

/// ノード実行エラー（リトライ分類・engine.md §7.5）。
#[derive(Debug, Clone)]
pub struct NodeError {
    pub code: String,
    pub message: String,
    /// リトライ可能か（false=permanent）。
    pub retryable: bool,
}

impl NodeResult {
    /// 成功（out ポート）。
    pub fn ok(output: Value) -> Self {
        NodeResult {
            ok: true,
            output,
            taken_ports: vec!["out".to_string()],
            error: None,
        }
    }

    /// 成功（指定ポート・branch/switch 用）。
    pub fn ok_port(output: Value, port: &str) -> Self {
        NodeResult {
            ok: true,
            output,
            taken_ports: vec![port.to_string()],
            error: None,
        }
    }

    /// 失敗。
    pub fn fail(code: &str, message: impl Into<String>, retryable: bool) -> Self {
        NodeResult {
            ok: false,
            output: Value::Null,
            taken_ports: Vec::new(),
            error: Some(NodeError {
                code: code.to_string(),
                message: message.into(),
                retryable,
            }),
        }
    }
}

/// ノード種ごとの実処理を担う委譲先（能力ノード=既存チョークポイント・Task 10.6a〜）。
///
/// 本 PR（10.2）はエンジン骨格を提供し、実処理は後続 PR で実装する。制御ノードは
/// エンジンが直接扱うため、このトレイトは非制御ノード（能力/script/http/AI）のみに呼ばれる。
#[async_trait]
pub trait NodeExecutor: Send + Sync {
    /// 1 ノードを実行する。`node_type` は vocab の閉集合文字列。
    async fn execute(&self, node_type: &str, params: &Value, ctx: &NodeContext) -> NodeResult;
}

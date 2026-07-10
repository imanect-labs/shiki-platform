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
use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

pub use launcher::{LauncherError, WorkflowRunLauncher};
pub use model::{idempotency_key, RunStatus, StepStatus};
pub use store::{
    RunDetail, RunEventRow, RunListFilter, RunListItem, RunStore, RunStoreError, StepDetail,
    StepOverview,
};
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
    /// map 領域の要素コンテキスト（`{ "item": …, "index": i }`・`$from each.*` の源）。
    /// 領域外ノードは None。
    pub each: Option<Value>,
    /// OTel トレース id（監査 ↔ Langfuse ↔ OTel を束ねる・run から伝播）。
    pub trace_id: Option<String>,
    /// scope ceiling（= IR の declared_scopes ∩ ノード設定）。能力ゲートウェイが
    /// 「操作の要求スコープ ∈ scope_ceiling」を検証してから OpenFGA check を行う（二重ゲート）。
    /// run 開始時に declared ⊆ 委譲 が保証されるため実効スコープ = declared_scopes。
    pub scope_ceiling: Vec<String>,
}

/// ノード実行の結果（成功/失敗と出力ポート）。
///
/// 通常ノードは `ok`/`fail` の 2 系統。制御ノード `control.wait`/`control.map` は
/// terminal 化せず durable に中断するため `suspend`/`fanout` を載せて返す（checkpoint 側で分岐）。
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
    /// wait ノードの中断指示（timer/event・engine.md §9）。設定時は step を待機状態にする。
    pub suspend: Option<Suspend>,
    /// map ノードの動的 fan-out 指示（engine.md §4.5）。設定時は要素を動的挿入し waiting_map にする。
    pub fanout: Option<MapFanout>,
}

/// wait ノードの中断指示（engine.md §9・「起床は ready に戻さず直接 terminal 化」）。
#[derive(Debug, Clone)]
pub enum Suspend {
    /// wait(duration/until): `wake_at` まで `waiting_timer`。スケジューラが起床する。
    Timer { wake_at: DateTime<Utc> },
    /// wait(event): イベント購読で `waiting_event`。マッチャがイベント到来で起床する。
    Event {
        source: String,
        /// テーブル/フォルダ束縛（祖先束縛あり）。
        scope: Value,
        /// 条件木（イベントペイロードに対して評価・省略可）。
        filter: Option<Value>,
        /// on_timeout の期限（None=無期限）。
        timeout_at: Option<DateTime<Utc>>,
        on_timeout: OnTimeout,
    },
}

/// wait(event) のタイムアウト方針（ir.md §7.5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnTimeout {
    /// 期限切れで run を失敗させる。
    Fail,
    /// 期限切れで `timeout` ポートへ継続する。
    Continue,
}

/// map の動的 fan-out 指示（engine.md §4.5・ir.md §5.3）。
#[derive(Debug, Clone)]
pub struct MapFanout {
    /// 展開する要素（入力順）。
    pub items: Vec<Value>,
    /// 要素並列度（Stage A では worker 並列数で実効的に制限）。
    pub max_concurrency: u32,
    /// 要素失敗時の方針。
    pub on_item_error: OnItemError,
}

/// map 要素失敗時の方針（ir.md §5.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnItemError {
    /// 1 要素の失敗で map ノード自体が失敗する（既定・map の on_error に準拠）。
    FailMap,
    /// 失敗要素は errors[] に集約し map は成功扱い。
    Collect,
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
            suspend: None,
            fanout: None,
        }
    }

    /// 成功（指定ポート・branch/switch 用）。
    pub fn ok_port(output: Value, port: &str) -> Self {
        NodeResult {
            ok: true,
            output,
            taken_ports: vec![port.to_string()],
            error: None,
            suspend: None,
            fanout: None,
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
            suspend: None,
            fanout: None,
        }
    }

    /// wait ノードの中断（timer/event）。checkpoint 側で待機状態へ遷移する。
    pub fn wait(suspend: Suspend) -> Self {
        NodeResult {
            ok: true,
            output: Value::Null,
            taken_ports: Vec::new(),
            error: None,
            suspend: Some(suspend),
            fanout: None,
        }
    }

    /// map ノードの動的 fan-out。checkpoint 側で要素を挿入し waiting_map へ遷移する。
    pub fn map_fanout(fanout: MapFanout) -> Self {
        NodeResult {
            ok: true,
            output: Value::Null,
            taken_ports: Vec::new(),
            error: None,
            suspend: None,
            fanout: Some(fanout),
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

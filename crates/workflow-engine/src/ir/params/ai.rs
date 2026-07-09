//! AI ノード（llm.invoke / agent.invoke）の params 契約（ir.md §7.3/§7.4）。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ir::expr::ValueExpr;

/// `llm.invoke` — llm-gateway 直行（pure・課金あり）。
///
/// `model` はモデルカタログ照合（V3）が必須性を担保する（カタログ未設定環境のみ省略可）。
/// `effort`/`output_schema` は llm-gateway 経路の enforcement 実装時に追加する（契約に
/// 「効かないフィールド」を含めない方針・ir.md §7.3）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct LlmInvokeParams {
    /// モデル id（テナントのモデルカタログへ保存時照合・実行時再照合）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
    /// ユーザープロンプト。
    pub prompt: ValueExpr,
    /// システムプロンプト（省略可）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub system: Option<ValueExpr>,
    /// 出力トークン上限（省略時は gateway 既定）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub max_tokens: Option<ValueExpr>,
}

/// `agent.invoke` — サンドボックスで agent-core を実行（best-effort・wasm ティア固定）。
///
/// サンドボックス設定は**縮小のみ**（実効 = 実行主体 ReBAC ∩ declared_scopes ∩ 本設定）。
/// `mount_scope`/`allowed_tools`/`model`/`max_tokens` は Phase 5 フルツール構成との結線
/// （enforcement）実装時に追加する（効かない制限を UI に約束しない・fail-closed）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct AgentInvokeParams {
    /// エージェントへの指示文。
    pub instruction: ValueExpr,
    /// egress allowlist（リテラルのみ・縮小のみ・省略時は外部通信なし）。
    #[serde(default)]
    pub egress_allowlist: Vec<String>,
    /// 実行時間上限（秒）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub max_duration_sec: Option<ValueExpr>,
}

//! 宣言的バックエンド束縛（Task 6.5）。
//!
//! UI から実行できる操作は、スペックに宣言された束縛（本モジュール）**のみ**。束縛は
//! ①安全ツール（閉語彙・破壊系は保存時に拒否）②明示登録サーバハンドラ（閉語彙）
//! ③workflow-engine 対話トリガ起動（バージョンピン）の 3 系統に閉じる。
//! クライアントは実行時に `action_id + params` しか送れず、束縛定義は保存済み検証済み
//! スペックからサーバが引く（アンビエント権限なし）。

use agent_core::ToolName;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::vocab::HandlerKind;

/// UI アクションとして束縛できる安全ツールの閉集合（Task 6.5 の①）。
///
/// `requires_confirmation` なツール（shell/fs_delete 等）と UI 発話専用の emit_ui は
/// **保存時点で拒否**する（UI 直接アクションに承認フローは設けない・fail-closed）。
pub const ALLOWED_ACTION_TOOLS: &[ToolName] = &[ToolName::DocSearch, ToolName::WebSearch];

/// アクション束縛（3 系統の閉集合）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum ActionBinding {
    /// 許可ツールの呼び出し（[`ALLOWED_ACTION_TOOLS`] のみ・実行ユーザー権限）。
    Tool(ToolBinding),
    /// 明示登録のサーバ側ハンドラ（[`HandlerKind`] の閉語彙）。
    Handler(HandlerBinding),
    /// workflow-engine の対話トリガ起動（本人 ReBAC ∩ 宣言スコープ ∩ ノード設定）。
    Workflow(WorkflowBinding),
}

impl ActionBinding {
    /// 束縛 id（[`ActionRef`](crate::spec::ActionRef) が参照する）。
    pub fn id(&self) -> &str {
        match self {
            ActionBinding::Tool(b) => &b.id,
            ActionBinding::Handler(b) => &b.id,
            ActionBinding::Workflow(b) => &b.id,
        }
    }

    /// 監査用の束縛種別。
    pub fn kind_str(&self) -> &'static str {
        match self {
            ActionBinding::Tool(_) => "tool",
            ActionBinding::Handler(_) => "handler",
            ActionBinding::Workflow(_) => "workflow",
        }
    }

    /// **1 メッセージにつき 1 回だけ**実行できる束縛か（#410）。
    ///
    /// true の束縛は [`ActionLedger`](crate::ActionLedger) で実行前に確保され、二度目は
    /// [`ActionError::AlreadyInvoked`](crate::ActionError::AlreadyInvoked) で拒否される。
    /// 閉語彙の網羅 match にしてあるので、束縛を足すときに必ずここで判断を迫られる。
    pub fn single_use(&self) -> bool {
        match self {
            // chat.submit は**発話と生成 run をまるごと作る**。質問カードの回答も計画カードの
            // 「開始」もこれで、二度押しは会話の二重化・調査の二重実行（実費）に直結する。
            ActionBinding::Handler(b) => match b.handler {
                HandlerKind::ChatSubmit => true,
            },
            // ツール束縛（検索）は同じフォームで条件を変えて何度でも引ける。ワークフロー起動も
            // 「もう一度回す」があり得るので抑止しない（起動の冪等性は workflow-engine の関心事）。
            ActionBinding::Tool(_) | ActionBinding::Workflow(_) => false,
        }
    }
}

/// ツール束縛。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ToolBinding {
    pub id: String,
    /// ツール名（閉語彙・[`ALLOWED_ACTION_TOOLS`] 外は検証で拒否）。
    pub tool: ToolName,
}

/// サーバハンドラ束縛。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct HandlerBinding {
    pub id: String,
    pub handler: HandlerKind,
}

/// ワークフロー起動束縛。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct WorkflowBinding {
    pub id: String,
    pub workflow: WorkflowPin,
}

/// ワークフロー参照（検証・解決で **artifact_id＋version が焼き込まれる**＝再現性）。
///
/// LLM は `name` だけ書けばよく、[`SpecValidator`](crate::validator::SpecValidator) が
/// 発話ユーザーの viewer 権限で解決し、未指定 version は current にピンする。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct WorkflowPin {
    /// 参照名（tenant 内の workflow artifact 名）。
    #[serde(default)]
    pub name: Option<String>,
    /// 解決済み artifact id（保存済みスペックでは常に Some）。
    #[serde(default)]
    pub artifact_id: Option<Uuid>,
    /// 解決済みバージョン（保存済みスペックでは常に Some）。
    /// JSON 経由の数値のため TS では number（bigint にしない）。
    #[serde(default)]
    #[ts(type = "number | null")]
    pub version: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_serde_shape() {
        let b: ActionBinding = serde_json::from_value(serde_json::json!({
            "type": "handler", "id": "submit", "handler": "chat.submit"
        }))
        .unwrap();
        assert_eq!(b.id(), "submit");
        assert_eq!(b.kind_str(), "handler");

        let b: ActionBinding = serde_json::from_value(serde_json::json!({
            "type": "tool", "id": "search", "tool": "doc_search"
        }))
        .unwrap();
        assert_eq!(b.kind_str(), "tool");

        let b: ActionBinding = serde_json::from_value(serde_json::json!({
            "type": "workflow", "id": "run", "workflow": { "name": "wf-1" }
        }))
        .unwrap();
        assert_eq!(b.kind_str(), "workflow");
    }

    #[test]
    fn unknown_binding_type_is_unrepresentable() {
        // 「任意 URL への fetch」のような未知の束縛種はスキーマ上表現できない。
        assert!(serde_json::from_value::<ActionBinding>(serde_json::json!({
            "type": "http", "id": "x", "url": "https://evil.example"
        }))
        .is_err());
    }

    /// 1 回だけ実行できる束縛（#410）。`chat.submit` は発話と生成 run を作るので単発、
    /// 検索・ワークフロー起動は繰り返して当然なので抑止しない。
    #[test]
    fn only_chat_submit_is_single_use() {
        let handler: ActionBinding = serde_json::from_value(serde_json::json!({
            "type": "handler", "id": "submit", "handler": "chat.submit"
        }))
        .unwrap();
        assert!(handler.single_use());

        let tool: ActionBinding = serde_json::from_value(serde_json::json!({
            "type": "tool", "id": "search", "tool": "doc_search"
        }))
        .unwrap();
        assert!(!tool.single_use());

        let workflow: ActionBinding = serde_json::from_value(serde_json::json!({
            "type": "workflow", "id": "run", "workflow": { "name": "wf-1" }
        }))
        .unwrap();
        assert!(!workflow.single_use());
    }

    #[test]
    fn allowed_action_tools_are_safe_subset() {
        // 破壊系（shell/fs_*）と emit_ui は UI 束縛に含めない。
        assert!(ALLOWED_ACTION_TOOLS.contains(&ToolName::DocSearch));
        assert!(!ALLOWED_ACTION_TOOLS.contains(&ToolName::Shell));
        assert!(!ALLOWED_ACTION_TOOLS.contains(&ToolName::FsDelete));
        assert!(!ALLOWED_ACTION_TOOLS.contains(&ToolName::EmitUi));
    }
}

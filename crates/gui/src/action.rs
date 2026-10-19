//! 宣言的バックエンド束縛（Task 6.5・#604）。
//!
//! 図のボタンから実行できる操作は、本モジュールの束縛**のみ**。束縛は
//! ①明示登録サーバハンドラ（閉語彙・会話に送る `chat.submit`）
//! ②workflow-engine 対話トリガ起動（バージョンピン）の 2 系統に閉じる。
//! クライアントは実行時に `action_id + params` しか送れず、束縛はサーバーが保存済みの
//! 本文（回答・ミニアプリの画面）と確定時の解決から作る（アンビエント権限なし）。

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::vocab::HandlerKind;

/// アクション束縛（2 系統の閉集合）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum ActionBinding {
    /// 明示登録のサーバ側ハンドラ（[`HandlerKind`] の閉語彙）。
    Handler(HandlerBinding),
    /// workflow-engine の対話トリガ起動（本人 ReBAC ∩ 宣言スコープ ∩ ノード設定）。
    Workflow(WorkflowBinding),
}

impl ActionBinding {
    /// 束縛 id。
    pub fn id(&self) -> &str {
        match self {
            ActionBinding::Handler(b) => &b.id,
            ActionBinding::Workflow(b) => &b.id,
        }
    }

    /// 監査用の束縛種別。
    pub fn kind_str(&self) -> &'static str {
        match self {
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
            // chat.submit は**発話と生成 run をまるごと作る**。ask の回答も plan の「開始」も
            // これで、二度押しは会話の二重化・調査の二重実行（実費）に直結する。
            ActionBinding::Handler(b) => match b.handler {
                HandlerKind::ChatSubmit => true,
            },
            // ワークフロー起動は「もう一度回す」があり得るので抑止しない（同じ送信の再送は
            // 呼び出し側の冪等キーで 1 回にする）。
            ActionBinding::Workflow(_) => false,
        }
    }
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
/// ボタンは `name` だけ書けばよく、[`WorkflowResolver`](crate::resolver::WorkflowResolver) が
/// 発話ユーザーの viewer 権限で解決し、未指定 version は current にピンする。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct WorkflowPin {
    /// 参照名（tenant 内の workflow artifact 名）。
    #[serde(default)]
    pub name: Option<String>,
    /// 解決済み artifact id（解決後は常に Some）。
    #[serde(default)]
    pub artifact_id: Option<Uuid>,
    /// 解決済みバージョン（解決後は常に Some）。
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
            "type": "workflow", "id": "run", "workflow": { "name": "wf-1" }
        }))
        .unwrap();
        assert_eq!(b.kind_str(), "workflow");
    }

    #[test]
    fn unknown_binding_type_is_unrepresentable() {
        // 「任意 URL への fetch」のような未知の束縛種・撤去したツール束縛はスキーマ上表現できない。
        for v in [
            serde_json::json!({ "type": "http", "id": "x", "url": "https://evil.example" }),
            serde_json::json!({ "type": "tool", "id": "search", "tool": "doc_search" }),
        ] {
            assert!(serde_json::from_value::<ActionBinding>(v).is_err());
        }
    }

    /// 1 回だけ実行できる束縛（#410）。`chat.submit` は発話と生成 run を作るので単発、
    /// ワークフロー起動は繰り返して当然なので抑止しない。
    #[test]
    fn only_chat_submit_is_single_use() {
        let handler: ActionBinding = serde_json::from_value(serde_json::json!({
            "type": "handler", "id": "submit", "handler": "chat.submit"
        }))
        .unwrap();
        assert!(handler.single_use());

        let workflow: ActionBinding = serde_json::from_value(serde_json::json!({
            "type": "workflow", "id": "run", "workflow": { "name": "wf-1" }
        }))
        .unwrap();
        assert!(!workflow.single_use());
    }
}

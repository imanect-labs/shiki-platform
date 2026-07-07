//! トリガ定義（schedule / event / interactive・ir.md §6）。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::expr::Condition;

/// トリガ（`nodes` とは別セクション・dnd 上は擬似ノード）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum Trigger {
    /// スケジュール（cron＋tz）。
    Schedule(ScheduleTrigger),
    /// イベント（outbox マッチング）。
    Event(EventTrigger),
    /// 対話（UI/チャット起動・入力は input_schema 契約）。
    Interactive(InteractiveTrigger),
}

impl Trigger {
    pub fn kind(&self) -> TriggerKind {
        match self {
            Trigger::Schedule(_) => TriggerKind::Schedule,
            Trigger::Event(_) => TriggerKind::Event,
            Trigger::Interactive(_) => TriggerKind::Interactive,
        }
    }
}

/// トリガ種別（run の実行主体決定・engine.md §6）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum TriggerKind {
    Schedule,
    Event,
    Interactive,
}

impl TriggerKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            TriggerKind::Schedule => "schedule",
            TriggerKind::Event => "event",
            TriggerKind::Interactive => "interactive",
        }
    }
}

/// スケジュールトリガ（cron 5 フィールド＋IANA tz）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ScheduleTrigger {
    /// cron 式（5 フィールド）。
    pub cron: String,
    /// IANA タイムゾーン（必須）。
    pub tz: String,
    /// misfire 時の扱い（skip 既定 / none。all は v1 でやらない）。
    #[serde(default)]
    pub catchup: Catchup,
}

/// catchup ポリシ（ir.md §6.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Catchup {
    /// 区間内直近 1 occurrence のみ発火（既定）。
    #[default]
    Skip,
    /// 全捨て watermark だけ前進。
    None,
}

/// イベントトリガ（source＋scope＋filter・ir.md §6.2）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct EventTrigger {
    /// イベント source（閉集合・V3 で照合。Stage A は storage.write のみ有効）。
    #[ts(type = "EventSource")]
    pub source: String,
    /// scope（テーブル/フォルダ束縛必須・全購読禁止）。
    #[ts(type = "unknown")]
    pub scope: serde_json::Value,
    /// filter（条件木・省略可）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub filter: Option<Condition>,
}

/// 対話トリガ（UI/チャット起動）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct InteractiveTrigger {
    /// 表示ラベル（省略可）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub label: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn schedule_trigger_parses() {
        let t: Trigger = serde_json::from_value(json!({
            "kind": "schedule", "cron": "0 9 * * *", "tz": "Asia/Tokyo"
        }))
        .unwrap();
        assert_eq!(t.kind(), TriggerKind::Schedule);
        if let Trigger::Schedule(s) = t {
            assert_eq!(s.catchup, Catchup::Skip);
        }
    }

    #[test]
    fn event_trigger_requires_scope() {
        let t: Trigger = serde_json::from_value(json!({
            "kind": "event", "source": "storage.write", "scope": { "folder_id": "f1" }
        }))
        .unwrap();
        assert_eq!(t.kind(), TriggerKind::Event);
    }

    #[test]
    fn interactive_trigger() {
        let t: Trigger = serde_json::from_value(json!({"kind": "interactive"})).unwrap();
        assert_eq!(t.kind(), TriggerKind::Interactive);
    }
}

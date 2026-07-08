//! 計画・サブタスク分解（Task 5.2）。
//!
//! 自律プロファイルの run 状態として**フラットなサブタスク列**を明示保持する。深いネストは避け、
//! 観測に応じた**動的再計画**（追加/削除/並べ替え/状態更新）を基本とする（phase-5.md §5.2）。
//! 計画はループ内で `plan` メタツール（[`crate::agent`] が横取り）経由でモデルが改訂し、
//! 変化を [`AgentEvent::PlanUpdated`](crate::event::AgentEvent) として外部化して可視化(5.9)・監査(5.10)へ流す。

use serde::{Deserialize, Serialize};

/// サブタスクの進行状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubtaskStatus {
    /// 未着手。
    Todo,
    /// 進行中（同時に高々 1 つを推奨・強制はしない）。
    Doing,
    /// 完了。
    Done,
    /// ブロック（依存待ち・要承認・行き詰まり）。
    Blocked,
}

impl SubtaskStatus {
    /// snake_case 文字列から解釈する（未知は Todo に倒す・敵対的入力を安全側へ）。
    fn parse(s: &str) -> Self {
        match s {
            "doing" => SubtaskStatus::Doing,
            "done" => SubtaskStatus::Done,
            "blocked" => SubtaskStatus::Blocked,
            _ => SubtaskStatus::Todo,
        }
    }
}

/// 1 サブタスク。`id` は計画内で安定（再計画でも既存タイトル一致なら状態を引き継ぐ）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subtask {
    pub id: String,
    pub title: String,
    pub status: SubtaskStatus,
}

/// 計画（サブタスクのフラットな順序付き列）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub subtasks: Vec<Subtask>,
    /// 改訂回数（0=未設定）。イベントの単調性確認・監査で使う。
    pub revision: u32,
}

/// サブタスク列の 1 件ぶんの改訂入力（モデルが渡す `plan` ツール引数の要素）。
#[derive(Debug, Clone, Deserialize)]
pub struct SubtaskInput {
    pub title: String,
    #[serde(default)]
    pub status: Option<String>,
}

/// 計画のサイズ上限（暴走・過剰階層化の防止・phase-5.md「フラット」方針）。
const MAX_SUBTASKS: usize = 64;

impl Plan {
    /// モデルの改訂入力で計画を**全置換**する（フラット列・決定的 id 採番）。
    ///
    /// 既存タイトルと一致する新サブタスクは、入力が状態を明示しない限り**旧状態を引き継ぐ**
    /// （done を todo へ巻き戻さない安全側）。上限超過分は切り捨てる。戻り値は「計画が変化したか」。
    pub fn revise(&mut self, inputs: Vec<SubtaskInput>) -> bool {
        let prior: std::collections::HashMap<&str, SubtaskStatus> = self
            .subtasks
            .iter()
            .map(|s| (s.title.as_str(), s.status))
            .collect();
        let mut next = Vec::with_capacity(inputs.len().min(MAX_SUBTASKS));
        for input in inputs {
            // 上限は**有効なサブタスク数**で数える（空タイトルが枠を消費しない）。
            if next.len() >= MAX_SUBTASKS {
                break;
            }
            let title = input.title.trim().to_string();
            if title.is_empty() {
                continue;
            }
            let status = match input.status.as_deref() {
                Some(s) => SubtaskStatus::parse(s),
                // 状態未指定は旧状態を引き継ぐ（無ければ Todo）。
                None => prior
                    .get(title.as_str())
                    .copied()
                    .unwrap_or(SubtaskStatus::Todo),
            };
            let idx = next.len();
            next.push(Subtask {
                id: format!("st-{idx}"),
                title,
                status,
            });
        }
        if next == self.subtasks {
            return false;
        }
        self.subtasks = next;
        self.revision = self.revision.saturating_add(1);
        true
    }

    /// 未完了（Todo/Doing/Blocked）が残っているか。終了判断の補助に使う。
    #[must_use]
    pub fn has_open(&self) -> bool {
        self.subtasks
            .iter()
            .any(|s| !matches!(s.status, SubtaskStatus::Done))
    }

    /// 進捗サマリ `(done, total)`。
    #[must_use]
    pub fn progress(&self) -> (usize, usize) {
        let done = self
            .subtasks
            .iter()
            .filter(|s| matches!(s.status, SubtaskStatus::Done))
            .count();
        (done, self.subtasks.len())
    }
}

/// `plan` メタツールの入力 JSON Schema（モデルへ提示する）。
#[must_use]
pub fn plan_tool_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "subtasks": {
                "type": "array",
                "description": "現時点の計画。全置換で渡す（追加/削除/並べ替え/状態更新を反映した完全な列）。",
                "items": {
                    "type": "object",
                    "properties": {
                        "title": { "type": "string", "description": "サブタスクの短い説明" },
                        "status": {
                            "type": "string",
                            "enum": ["todo", "doing", "done", "blocked"],
                            "description": "省略時は既存状態を引き継ぐ（新規は todo）"
                        }
                    },
                    "required": ["title"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["subtasks"],
        "additionalProperties": false
    })
}

/// `plan` ツール入力から `Vec<SubtaskInput>` を取り出す（不正は空＝計画不変）。
#[must_use]
pub fn parse_plan_input(input: &serde_json::Value) -> Vec<SubtaskInput> {
    input
        .get("subtasks")
        .and_then(|v| serde_json::from_value::<Vec<SubtaskInput>>(v.clone()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(items: &[(&str, Option<&str>)]) -> Vec<SubtaskInput> {
        items
            .iter()
            .map(|(t, s)| SubtaskInput {
                title: (*t).to_string(),
                status: s.map(str::to_string),
            })
            .collect()
    }

    #[test]
    fn revise_assigns_stable_ids_and_bumps_revision() {
        let mut plan = Plan::default();
        assert!(plan.revise(inputs(&[("A", None), ("B", Some("doing"))])));
        assert_eq!(plan.revision, 1);
        assert_eq!(plan.subtasks[0].id, "st-0");
        assert_eq!(plan.subtasks[1].status, SubtaskStatus::Doing);
        assert_eq!(plan.subtasks[0].status, SubtaskStatus::Todo);
    }

    #[test]
    fn revise_inherits_prior_status_when_unspecified() {
        let mut plan = Plan::default();
        plan.revise(inputs(&[("A", Some("done"))]));
        // 状態未指定の再計画では done を巻き戻さない。
        let changed = plan.revise(inputs(&[("A", None), ("B", None)]));
        assert!(changed);
        assert_eq!(plan.subtasks[0].status, SubtaskStatus::Done);
        assert_eq!(plan.subtasks[1].status, SubtaskStatus::Todo);
    }

    #[test]
    fn revise_is_noop_when_unchanged() {
        let mut plan = Plan::default();
        plan.revise(inputs(&[("A", Some("todo"))]));
        let rev = plan.revision;
        // 同一内容の再送は計画を変えない（イベントを無駄打ちしない）。
        assert!(!plan.revise(inputs(&[("A", Some("todo"))])));
        assert_eq!(plan.revision, rev);
    }

    #[test]
    fn revise_drops_empty_titles_and_caps_length() {
        let mut plan = Plan::default();
        let mut many = vec![("", None)];
        let titles: Vec<String> = (0..100).map(|i| format!("t{i}")).collect();
        for t in &titles {
            many.push((t.as_str(), None));
        }
        plan.revise(inputs(&many));
        assert_eq!(plan.subtasks.len(), MAX_SUBTASKS); // 空タイトル除外＋上限で切り捨て
    }

    #[test]
    fn progress_and_has_open() {
        let mut plan = Plan::default();
        plan.revise(inputs(&[("A", Some("done")), ("B", Some("todo"))]));
        assert_eq!(plan.progress(), (1, 2));
        assert!(plan.has_open());
        plan.revise(inputs(&[("A", Some("done")), ("B", Some("done"))]));
        assert!(!plan.has_open());
    }

    #[test]
    fn parse_plan_input_tolerates_garbage() {
        assert!(parse_plan_input(&serde_json::json!({})).is_empty());
        assert!(parse_plan_input(&serde_json::json!({"subtasks": "nope"})).is_empty());
        let ok = parse_plan_input(&serde_json::json!({"subtasks": [{"title": "x"}]}));
        assert_eq!(ok.len(), 1);
    }
}

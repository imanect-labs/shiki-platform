//! 制御ノードの params 契約（ir.md §5・§7.5）。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::ParamsIssue;
use crate::ir::expr::{Condition, ValueExpr};

/// `control.branch` — 条件を評価して `true`/`false` ポートを確定する。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct BranchParams {
    /// 判定条件（条件木・ir.md §3.3）。
    pub condition: Condition,
}

/// `control.switch` の 1 case（値がリテラル一致したらその port を取る）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct SwitchCase {
    /// 一致時に取る出力ポート名。
    pub port: String,
    /// 一致判定するリテラル値。
    #[ts(type = "unknown")]
    pub equals: serde_json::Value,
}

/// `control.switch` — 値を case とリテラル照合し、一致 port（なければ `default`）へ。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct SwitchParams {
    /// 照合する値。
    pub value: ValueExpr,
    /// case 一覧（上から順に照合）。
    #[serde(default)]
    pub cases: Vec<SwitchCase>,
}

/// `control.join` の待ち合わせモード（ir.md §5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum JoinMode {
    /// 全ての live 入エッジを待つ（既定・dead は吸収）。
    #[default]
    All,
    /// 最初の 1 本で発火する（敗者の下流は skip）。
    Any,
}

/// `control.join` — 待ち合わせ（発火判定の実行時セマンティクスは engine.md §4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct JoinParams {
    /// 待ち合わせモード（既定 all）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mode: Option<JoinMode>,
}

/// map 領域内の要素失敗の扱い（engine.md §4.5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum MapItemError {
    /// 要素失敗で map 全体を失敗させる（既定・map ノードの on_error に従う）。
    #[default]
    FailMap,
    /// 失敗要素を集約結果に `{ error }` として残し継続する。
    Collect,
}

/// `control.map` — 配列要素ごとに領域を動的 fan-out する（engine.md §4.5）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MapParams {
    /// 繰り返す配列（$from 参照が典型）。
    pub items: ValueExpr,
    /// 要素の同時実行上限（既定 10・Stage A はワーカー並列数が実効上限）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub max_concurrency: Option<u32>,
    /// 要素失敗の扱い（既定 fail_map）。
    #[serde(default)]
    pub on_item_error: MapItemError,
}

/// `control.wait` の kind（duration / until / event）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum WaitKind {
    /// 相対時間（`duration_sec`）。
    Duration,
    /// 絶対時刻（`until`・RFC3339）。
    Until,
    /// イベント待ち（`source`/`scope`/`filter`/`timeout_sec`/`on_timeout`）。
    Event,
}

/// `control.wait` の timeout 時の扱い。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum WaitTimeout {
    /// step を失敗させる（既定）。
    #[default]
    Fail,
    /// `timeout` ポートへ流して継続する。
    Continue,
}

/// `control.wait` — 時間/イベントを durable に待つ（ir.md §7.5・engine.md §9）。
///
/// kind タグ付き enum は serde の `deny_unknown_fields` と併用できないため、フラット struct
/// ＋ [`check_cross_fields`](Self::check_cross_fields)（kind ごとの必須/禁止）で厳密性を保つ。
/// JSON 表現は仕様（`{ "kind": "event", "source": ..., ... }`）と同一。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct WaitParams {
    /// 待ち方（duration / until / event）。
    pub kind: WaitKind,
    /// kind=duration: 待つ秒数（非負）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub duration_sec: Option<ValueExpr>,
    /// kind=until: 起床時刻（RFC3339）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub until: Option<ValueExpr>,
    /// kind=event: 待つイベント source（閉集合・トリガと同じ語彙）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "EventSource | null", optional)]
    pub source: Option<String>,
    /// kind=event: scope 束縛（`{ "folder": "<uuid>" }`・祖先束縛で照合）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "unknown", optional)]
    pub scope: Option<serde_json::Value>,
    /// kind=event: 追加 filter（条件木・fail-closed 評価）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub filter: Option<Condition>,
    /// kind=event: 待ちの上限秒（省略は無期限）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub timeout_sec: Option<ValueExpr>,
    /// kind=event: timeout 時の扱い（既定 fail）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub on_timeout: Option<WaitTimeout>,
}

impl WaitParams {
    /// kind ごとの必須/禁止フィールドを検査する（V1 の厳密性を enum 相当に保つ）。
    pub fn check_cross_fields(&self) -> Result<(), ParamsIssue> {
        let deny = |field: &str| {
            Err(ParamsIssue {
                path: format!("/params/{field}"),
                message: format!("wait({}) では {field} は指定できません", self.kind_str()),
            })
        };
        let require = |field: &str| {
            Err(ParamsIssue {
                path: format!("/params/{field}"),
                message: format!("wait({}) には {field} が必要です", self.kind_str()),
            })
        };
        match self.kind {
            WaitKind::Duration => {
                if self.duration_sec.is_none() {
                    return require("duration_sec");
                }
                for (name, absent) in [
                    ("until", self.until.is_none()),
                    ("source", self.source.is_none()),
                    ("scope", self.scope.is_none()),
                    ("filter", self.filter.is_none()),
                    ("timeout_sec", self.timeout_sec.is_none()),
                    ("on_timeout", self.on_timeout.is_none()),
                ] {
                    if !absent {
                        return deny(name);
                    }
                }
            }
            WaitKind::Until => {
                if self.until.is_none() {
                    return require("until");
                }
                for (name, absent) in [
                    ("duration_sec", self.duration_sec.is_none()),
                    ("source", self.source.is_none()),
                    ("scope", self.scope.is_none()),
                    ("filter", self.filter.is_none()),
                    ("timeout_sec", self.timeout_sec.is_none()),
                    ("on_timeout", self.on_timeout.is_none()),
                ] {
                    if !absent {
                        return deny(name);
                    }
                }
            }
            WaitKind::Event => {
                if self.source.is_none() {
                    return require("source");
                }
                for (name, absent) in [
                    ("duration_sec", self.duration_sec.is_none()),
                    ("until", self.until.is_none()),
                ] {
                    if !absent {
                        return deny(name);
                    }
                }
                // scope は空（run 内購読のワイルドカード）か { "folder": "<uuid>" } のみ
                // （実行時マッチャの fail-closed 規則と同一契約・非 folder 形状は保存時に弾く）。
                if let Some(scope) = &self.scope {
                    let ok = scope.as_object().is_some_and(|o| {
                        o.is_empty()
                            || (o.len() == 1
                                && o.get("folder")
                                    .and_then(serde_json::Value::as_str)
                                    .is_some_and(|v| uuid::Uuid::parse_str(v).is_ok()))
                    });
                    if !ok {
                        return Err(ParamsIssue {
                            path: "/params/scope".to_string(),
                            message: "wait(event) の scope は空か { \"folder\": \"<uuid>\" } のみ指定できます"
                                .to_string(),
                        });
                    }
                }
            }
        }
        Ok(())
    }

    fn kind_str(&self) -> &'static str {
        match self.kind {
            WaitKind::Duration => "duration",
            WaitKind::Until => "until",
            WaitKind::Event => "event",
        }
    }
}

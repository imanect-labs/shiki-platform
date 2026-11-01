//! 人の判断を待つノード（`human.approval`・GAP-08・D-24）の params 契約。
//!
//! 承認は FSM で管理するテーブルの 1 行に束縛する（レコード束縛）。判断は**承認者本人の FSM 遷移**で、
//! 誰が承認できるかの正は FSM の actor 述語 1 か所（承認 API・テーブル画面・ワークフロー・ミニアプリの
//! どこから遷移しても同じ判定）。ここで決めるのは、受信箱に出す相手（`assignees`）・判断の行き先の状態
//! （`approve` / `reject` / `return` の `to`）・受信箱に出す列・期限だけで、OpenFGA の relation は増やさない。
//!
//! - `title` / `instructions` はリテラルの文字列だけ（データの値を受信箱へ写さない。受信箱の中身は承認者本人の
//!   権限で行を読み直して作る）。
//! - `record.table` は UUID のリテラル（同意計画・保存時の照合・候補の索引の鍵）。`record.id` は値式で、停止時に
//!   解決する。
//! - `timeout_sec` はリテラルの秒数（保存時に `policies.run_timeout_sec` 未満であることを検査する）。
//! - 件名・テーブル・承認者・判断の行き先は、置いたばかりの空のまま（件名とテーブルと行き先が空の文字・承認者が
//!   0 件・承認者の列が空の文字）でも契約では誤りにしない（D-129・D-83 と同じ）。空のままなら保存時の警告にとどめ、
//!   実行と自動実行のオンの前に止める（`validate::readiness`）。値があるときの形の検査（件名の字数・テーブルの ID・
//!   承認者の件数と形・行き先の重なり）と、保存時の照合（V4）は今のまま行う。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{ParamsIssue, WaitTimeout};
use crate::ir::expr::ValueExpr;

/// 件名の上限（文字数）。
pub const MAX_APPROVAL_TITLE_CHARS: usize = 120;
/// 説明の上限（文字数）。
pub const MAX_APPROVAL_INSTRUCTIONS_CHARS: usize = 2000;
/// 担当者の上限。
pub const MAX_APPROVAL_ASSIGNEES: usize = 20;
/// 受信箱に出す列の上限。
pub const MAX_APPROVAL_SHOW_FIELDS: usize = 30;
/// 選択肢のラベルの上限（文字数）。
pub const MAX_APPROVAL_LABEL_CHARS: usize = 40;

/// 承認を束縛する行（FSM で管理するテーブルの 1 行）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ApprovalRecord {
    /// テーブル id（UUID のリテラル・小文字）。
    pub table: String,
    /// 行の id（値式・停止時に解決する）。
    pub id: ValueExpr,
}

/// 受信箱に出す相手（`user` / `role` / `field` のちょうど 1 つ）。
///
/// 承認できるかどうかは FSM の actor 述語が決める（担当者は受信箱に出すだけ・権限の正ではない）。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ApprovalAssignee {
    /// ユーザー id（固定値・前のノードの出力など）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub user: Option<ValueExpr>,
    /// ロール id（所属するユーザー全員の受信箱に出す）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub role: Option<ValueExpr>,
    /// 束縛した行の列（ユーザー・ロールの列の参照名）。停止時に実行主体の権限で読む。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub field: Option<String>,
}

/// 判断に添えるコメントの要否。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ApprovalComment {
    /// 書かなくてもよい（既定）。
    #[default]
    Optional,
    /// 書かないと判断できない。
    Required,
}

/// 判断の 1 つ（行き先の状態・ボタンの文言・コメントの要否）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ApprovalChoice {
    /// 判断したときに行を進める状態（FSM の状態・空なら実行の前に止める・D-129）。
    pub to: String,
    /// ボタンの文言（省略時は既定の文言）。受信箱は行き先の状態名も必ず添える。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub label: Option<String>,
    /// コメントの要否（既定 optional）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub comment: Option<ApprovalComment>,
}

/// `human.approval` — 担当者の承認・却下・差し戻しを待って分岐する（ir.md §7.8）。
///
/// 出口は `approved`・`rejected`（reject を指定したとき）・`returned`（return を指定したとき）・
/// `withdrawn`（判断の外で行が別の状態へ動いた・消えた）・`timeout`（on_timeout=continue のとき）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct HumanApprovalParams {
    /// 受信箱の件名（120 字までのリテラル・空なら実行の前に止める・D-129）。
    pub title: String,
    /// 担当者への説明（2000 字以内のリテラル）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub instructions: Option<String>,
    /// 承認を束縛する行。
    pub record: ApprovalRecord,
    /// 受信箱に出す相手（20 件まで・0 件なら実行の前に止める・D-129）。
    pub assignees: Vec<ApprovalAssignee>,
    /// 承認（必須）。
    pub approve: ApprovalChoice,
    /// 却下（省略可）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reject: Option<ApprovalChoice>,
    /// 差し戻し（省略可）。
    #[serde(default, rename = "return", skip_serializing_if = "Option::is_none")]
    #[ts(rename = "return", optional)]
    pub return_to: Option<ApprovalChoice>,
    /// 受信箱に出す列の参照名（30 列まで・担当者に読めない列は出さない）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub show_fields: Option<Vec<String>>,
    /// 判断の期限（秒・省略は無期限。`policies.run_timeout_sec` 未満）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub timeout_sec: Option<u32>,
    /// 期限切れの扱い（既定 fail。continue なら `timeout` 出口へ）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub on_timeout: Option<WaitTimeout>,
}

/// 判断の種別（受信箱の判断 API の語彙・出口の名前と対）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ApprovalDecisionKind {
    Approve,
    Reject,
    Return,
}

impl ApprovalDecisionKind {
    /// 判断 API・コメントの台帳の語彙。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ApprovalDecisionKind::Approve => "approve",
            ApprovalDecisionKind::Reject => "reject",
            ApprovalDecisionKind::Return => "return",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "approve" => ApprovalDecisionKind::Approve,
            "reject" => ApprovalDecisionKind::Reject,
            "return" => ApprovalDecisionKind::Return,
            _ => return None,
        })
    }

    /// 判断で取る出口。
    #[must_use]
    pub const fn port(self) -> &'static str {
        match self {
            ApprovalDecisionKind::Approve => "approved",
            ApprovalDecisionKind::Reject => "rejected",
            ApprovalDecisionKind::Return => "returned",
        }
    }

    /// 台帳の状態（判断で決まった承認の status）。
    #[must_use]
    pub const fn status(self) -> &'static str {
        self.port()
    }

    /// ボタンの既定の文言。
    #[must_use]
    pub const fn default_label(self) -> &'static str {
        match self {
            ApprovalDecisionKind::Approve => "承認する",
            ApprovalDecisionKind::Reject => "却下する",
            ApprovalDecisionKind::Return => "差し戻す",
        }
    }
}

impl HumanApprovalParams {
    /// 受信箱に出す列（省略は空）。
    #[must_use]
    pub fn show_fields(&self) -> &[String] {
        self.show_fields.as_deref().unwrap_or_default()
    }

    /// 指定した判断（承認・却下・差し戻しの順）。
    #[must_use]
    pub fn choices(&self) -> Vec<(ApprovalDecisionKind, &ApprovalChoice)> {
        let mut out = vec![(ApprovalDecisionKind::Approve, &self.approve)];
        if let Some(c) = &self.reject {
            out.push((ApprovalDecisionKind::Reject, c));
        }
        if let Some(c) = &self.return_to {
            out.push((ApprovalDecisionKind::Return, c));
        }
        out
    }

    /// 件数・文字数・形の検査（serde では表せない分・V1 の厳密性）。
    pub fn check_cross_fields(&self) -> Result<(), ParamsIssue> {
        let issue = |path: &str, message: String| {
            Err(ParamsIssue {
                path: format!("/params/{path}"),
                message,
            })
        };
        // 空の件名・テーブル・承認者・行き先は、ここでは止めない（実行の前に止める・D-129）。
        if self.title.chars().count() > MAX_APPROVAL_TITLE_CHARS {
            return issue(
                "title",
                format!("「受信箱の件名」は {MAX_APPROVAL_TITLE_CHARS} 字までにしてください。"),
            );
        }
        if self
            .instructions
            .as_ref()
            .is_some_and(|s| s.chars().count() > MAX_APPROVAL_INSTRUCTIONS_CHARS)
        {
            return issue(
                "instructions",
                format!("説明は {MAX_APPROVAL_INSTRUCTIONS_CHARS} 字以内で書いてください"),
            );
        }
        let canonical = uuid::Uuid::parse_str(&self.record.table)
            .is_ok_and(|u| u.hyphenated().to_string() == self.record.table);
        if !self.record.table.trim().is_empty() && !canonical {
            return issue(
                "record/table",
                "承認するテーブルを一覧から選んでください（テーブルの ID）".to_string(),
            );
        }
        if self.assignees.len() > MAX_APPROVAL_ASSIGNEES {
            return issue(
                "assignees",
                format!("承認者は {MAX_APPROVAL_ASSIGNEES} 件までにしてください。"),
            );
        }
        for (i, a) in self.assignees.iter().enumerate() {
            let n = usize::from(a.user.is_some())
                + usize::from(a.role.is_some())
                + usize::from(a.field.is_some());
            if n != 1 {
                return issue(
                    &format!("assignees/{i}"),
                    "担当者は user・role・field のどれか 1 つで指定してください".to_string(),
                );
            }
        }
        let mut tos: Vec<&str> = Vec::new();
        for (kind, c) in self.choices() {
            // params の項目名は判断の語彙と同じ（approve / reject / return）。
            let path = kind.as_str();
            if !c.to.trim().is_empty() && tos.contains(&c.to.as_str()) {
                return issue(
                    &format!("{path}/to"),
                    "承認・却下・差し戻しの行き先は別々の状態にしてください".to_string(),
                );
            }
            tos.push(&c.to);
            if c.label
                .as_ref()
                .is_some_and(|l| l.chars().count() > MAX_APPROVAL_LABEL_CHARS)
            {
                return issue(
                    &format!("{path}/label"),
                    format!("ボタンの文言は {MAX_APPROVAL_LABEL_CHARS} 字以内で書いてください"),
                );
            }
        }
        if self.show_fields().len() > MAX_APPROVAL_SHOW_FIELDS {
            return issue(
                "show_fields",
                format!("受信箱に出す列は {MAX_APPROVAL_SHOW_FIELDS} 列までです"),
            );
        }
        if self.timeout_sec == Some(0) {
            return issue(
                "timeout_sec",
                "期限は 1 秒以上にしてください（無期限なら空欄）".to_string(),
            );
        }
        if self.on_timeout.is_some() && self.timeout_sec.is_none() {
            return issue(
                "on_timeout",
                "期限切れの扱いは期限を決めたときだけ指定できます".to_string(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    const TABLE: &str = "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1";

    fn params(v: serde_json::Value) -> HumanApprovalParams {
        serde_json::from_value(v).unwrap()
    }

    fn base() -> serde_json::Value {
        json!({
            "title": "経費申請の承認",
            "record": { "table": TABLE, "id": { "$from": "trigger", "path": "/record_id" } },
            "assignees": [{ "field": "approver" }, { "role": "role-keiri" }],
            "approve": { "to": "approved" },
            "reject": { "to": "rejected", "comment": "required" },
            "return": { "to": "returned", "label": "差し戻す" },
            "show_fields": ["title", "amount"],
            "timeout_sec": 259_200, "on_timeout": "continue"
        })
    }

    /// `return` は Rust の予約語なので別名の項目に写る（JSON は `return` のまま往復する）。
    #[test]
    fn return_round_trips_under_its_json_name() {
        let p = params(base());
        assert_eq!(p.return_to.as_ref().unwrap().to, "returned");
        let back = serde_json::to_value(&p).unwrap();
        assert_eq!(back["return"]["to"], "returned");
        assert!(back.get("return_to").is_none());
        assert!(p.check_cross_fields().is_ok());
        let kinds: Vec<&str> = p.choices().iter().map(|(k, _)| k.port()).collect();
        assert_eq!(kinds, ["approved", "rejected", "returned"]);
    }

    #[test]
    fn rejects_bad_shapes_with_paths() {
        let cases = [
            (json!({ "title": "承".repeat(121) }), "/params/title"),
            (
                json!({ "record": { "table": "expense", "id": "x" } }),
                "/params/record/table",
            ),
            (
                json!({ "assignees": vec![json!({ "role": "r" }); 21] }),
                "/params/assignees",
            ),
            (
                json!({ "assignees": [{ "field": "approver", "role": "r" }] }),
                "/params/assignees/0",
            ),
            (
                json!({ "reject": { "to": "approved" } }),
                "/params/reject/to",
            ),
            (json!({ "timeout_sec": 0 }), "/params/timeout_sec"),
        ];
        for (patch, path) in cases {
            let mut v = base();
            for (k, val) in patch.as_object().unwrap() {
                v[k] = val.clone();
            }
            let e = params(v).check_cross_fields().unwrap_err();
            assert_eq!(e.path, path, "{patch}");
        }
        let mut v = base();
        v.as_object_mut().unwrap().remove("timeout_sec");
        let e = params(v).check_cross_fields().unwrap_err();
        assert_eq!(e.path, "/params/on_timeout");
    }

    /// D-129: 置いたばかりの承認（件名・テーブル・行き先が空の文字・承認者が 0 件）と、書きかけの空の欄（承認者の列・
    /// 却下と差し戻しの行き先が空の文字）は、契約では止めない（実行の前に止める）。空の行き先どうしは重なりと数えない。
    /// 値のある欄の検査は今のまま（120 字を超える件名・ID でないテーブル・同じ行き先）。
    #[test]
    fn blank_fields_of_a_freshly_placed_approval_pass_the_contract() {
        let placed = params(json!({
            "title": "",
            "record": { "table": "", "id": { "$from": "trigger", "path": "/record_id" } },
            "assignees": [],
            "approve": { "to": "" }
        }));
        assert_eq!(placed.check_cross_fields(), Ok(()));
        let mut v = base();
        v["title"] = json!("  ");
        v["assignees"] = json!([{ "field": "" }, { "user": "" }]);
        v["reject"] = json!({ "to": "" });
        v["return"] = json!({ "to": "" });
        assert_eq!(params(v).check_cross_fields(), Ok(()));
        let mut v = base();
        v["return"] = json!({ "to": "approved" });
        let e = params(v).check_cross_fields().unwrap_err();
        assert_eq!(e.path, "/params/return/to");
        let mut v = base();
        v["title"] = json!("承".repeat(121));
        let e = params(v).check_cross_fields().unwrap_err();
        assert_eq!(e.message, "「受信箱の件名」は 120 字までにしてください。");
    }
}

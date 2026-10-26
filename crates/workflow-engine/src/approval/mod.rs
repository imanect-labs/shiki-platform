//! 承認待ち（human.approval・GAP-08・D-24・D-27）の型と受信箱の台帳。
//!
//! 承認は FSM で管理するテーブルの 1 行に束縛する。**判断は承認者本人の FSM 遷移**（`FsmStore::transition`）で、
//! 承認 API・テーブル画面・ワークフロー・ミニアプリのどこから遷移しても同じ判定（actor 述語・行の可視性・
//! テーブルの editor・rev）を通る。エンジンはその行の遷移を wait(event `data.transition`) の一種として待ち、
//! 遷移先の状態を出口（approved / rejected / returned / withdrawn）へ写す。
//!
//! 台帳 `workflow_approval` は「誰の受信箱に出すか」（担当者）と、受信箱に出す件名・列・選択肢を持つ。
//! 担当者は権限の正ではない（OpenFGA の relation を増やさない・承認できるかは FSM の actor 述語が決める）。
//!
//! - 停止・起床・期限・取消は run の状態機械と同じ Tx で台帳を確定する（`run/store/approval.rs`）。
//! - 受信箱の照会と判断の受付は [`ApprovalStore`]（API が承認者本人の `AuthContext` で行を読み直す）。

mod decision_gate;
mod inbox;

pub use inbox::{
    AcceptedDecision, ApprovalRow, ApprovalStore, DecisionStart, DecisionTx, InboxCursor,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ir::params::{ApprovalComment, ApprovalDecisionKind};
use crate::run::OnTimeout;

/// 判断の選択肢 1 つ（台帳の `decisions`・受信箱のボタン）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalChoiceSpec {
    pub kind: ApprovalDecisionKind,
    /// 判断で進める状態。
    pub to: String,
    /// ボタンの文言（省略時は既定の文言を入れて保存する）。
    pub label: String,
    /// コメントの要否。
    pub comment: ApprovalComment,
}

/// 受信箱に出す相手の種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssigneeKind {
    User,
    Role,
}

impl AssigneeKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            AssigneeKind::User => "user",
            AssigneeKind::Role => "role",
        }
    }
}

/// 受信箱に出す相手 1 人（ユーザー）か 1 ロール。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AssigneeRef {
    pub kind: AssigneeKind,
    pub subject: String,
}

/// 承認の停止指示（executor が組み、checkpoint の Tx が台帳・担当者・購読を書く）。
#[derive(Debug, Clone)]
pub struct ApprovalSuspend {
    /// step の冪等キーから決める（再実行・再開で同じ承認になる・[`approval_id_for`]）。
    pub approval_id: Uuid,
    pub table_id: Uuid,
    pub record_id: Uuid,
    /// 停止時の行の状態（受信箱は今の状態がこれの行だけを出す）。
    pub from_state: String,
    /// 停止時の行の rev（これ以下の遷移では起きない）。
    pub suspend_rev: i64,
    pub title: String,
    pub instructions: Option<String>,
    pub show_fields: Vec<String>,
    pub choices: Vec<ApprovalChoiceSpec>,
    /// 解決済みの担当者（重複なし・空でない）。
    pub assignees: Vec<AssigneeRef>,
    /// 判断の期限（None は無期限・run の期限が来れば取り消される）。
    pub timeout_at: Option<DateTime<Utc>>,
    pub on_timeout: OnTimeout,
}

impl ApprovalSuspend {
    /// 遷移先の状態 → 出口（判断の外の状態は取り下げ）。
    #[must_use]
    pub fn port_for(
        choices: &[ApprovalChoiceSpec],
        to: Option<&str>,
    ) -> (&'static str, Option<ApprovalDecisionKind>) {
        match to.and_then(|to| choices.iter().find(|c| c.to == to)) {
            Some(c) => (c.kind.port(), Some(c.kind)),
            None => (WITHDRAWN_PORT, None),
        }
    }
}

/// 判断の外で行が別の状態へ動いた・消えたときの出口。
pub const WITHDRAWN_PORT: &str = "withdrawn";
/// 期限切れ（on_timeout=continue）の出口。
pub const TIMEOUT_PORT: &str = "timeout";

/// 承認を待つ間に実行主体（委譲・テーブルの権限）がテーブルを読めなくなったときの理由（D-62）。
///
/// step の `error.code`・run の `fail_reason`・決着の監査の `reason` に同じ値を書く（失敗の分類は
/// [`crate::run::failure::KNOWN_CODES`] の permission）。判断の遷移が実行主体に二度と見えず、出口へ進めても
/// 後続が委譲なしで中途半端に走るだけなので、出口へは進めずに run を失敗で終える。
pub const ACTOR_UNAVAILABLE: &str = "actor_unavailable";

/// 承認の確定（起床・期限・取り下げ・実行主体の失権）で台帳と step の出力に書くもの。
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalResolution {
    /// 台帳の status（approved / rejected / returned / withdrawn / expired / cancelled）。
    pub status: &'static str,
    /// step の出力の decision（approve / reject / return / withdrawn / timeout）。`failure` があるときは
    /// 出力を書かない（step は失敗で、後続へは進まない）。
    pub decision: &'static str,
    /// 進む出口（None は run の失敗: on_timeout=fail の期限切れ・実行主体の失権）。
    pub port: Option<&'static str>,
    /// run を失敗で終える理由（step の `error.code` と run の `fail_reason`・None は理由を書かない）。
    pub failure: Option<&'static str>,
    /// 判断で進んだ状態（遷移のとき）。
    pub to: Option<String>,
    /// 遷移した人（revision の changed_by）。
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    /// 遷移の後の rev。
    pub rev: Option<i64>,
}

impl ApprovalResolution {
    /// 行の遷移（`to`）による確定。
    #[must_use]
    pub fn transition(
        choices: &[ApprovalChoiceSpec],
        to: Option<String>,
        decided_by: Option<String>,
        decided_at: Option<DateTime<Utc>>,
        rev: Option<i64>,
    ) -> Self {
        let (port, kind) = ApprovalSuspend::port_for(choices, to.as_deref());
        ApprovalResolution {
            status: kind.map_or("withdrawn", ApprovalDecisionKind::status),
            decision: kind.map_or("withdrawn", ApprovalDecisionKind::as_str),
            port: Some(port),
            failure: None,
            to,
            decided_by,
            decided_at,
            rev,
        }
    }

    /// 行が消えた（実行主体に見えなくなった）ことによる取り下げ。
    #[must_use]
    pub fn withdrawn() -> Self {
        ApprovalResolution {
            status: "withdrawn",
            decision: "withdrawn",
            port: Some(WITHDRAWN_PORT),
            failure: None,
            to: None,
            decided_by: None,
            decided_at: None,
            rev: None,
        }
    }

    /// 期限切れ（continue なら timeout 出口・fail なら run の失敗）。
    #[must_use]
    pub fn expired(on_timeout: OnTimeout) -> Self {
        ApprovalResolution {
            status: "expired",
            decision: "timeout",
            port: match on_timeout {
                OnTimeout::Continue => Some(TIMEOUT_PORT),
                OnTimeout::Fail => None,
            },
            failure: None,
            to: None,
            decided_by: None,
            decided_at: None,
            rev: None,
        }
    }

    /// 実行主体（委譲・テーブルの権限）が承認を待つ間に外れた（D-62）: 取り下げの出口へは進めず、run を
    /// [`ACTOR_UNAVAILABLE`] で失敗させる。台帳は run の失敗と同じく `cancelled` にする。
    #[must_use]
    pub fn actor_unavailable() -> Self {
        ApprovalResolution {
            status: "cancelled",
            decision: "cancelled",
            port: None,
            failure: Some(ACTOR_UNAVAILABLE),
            to: None,
            decided_by: None,
            decided_at: None,
            rev: None,
        }
    }

    /// 判断の種別（コメントを引く鍵・取り下げと期限切れは None）。
    #[must_use]
    pub fn kind(&self) -> Option<ApprovalDecisionKind> {
        ApprovalDecisionKind::parse(self.decision)
    }
}

/// step の冪等キーから承認 id を決める（同じ step の再実行・再開で同じ id）。
#[must_use]
pub fn approval_id_for(tenant_id: &str, idempotency_key: &str) -> Uuid {
    use sha2::{Digest, Sha256};
    let h = Sha256::digest(format!("human.approval\0{tenant_id}\0{idempotency_key}").as_bytes());
    let mut b = [0u8; 16];
    b.copy_from_slice(&h[..16]);
    uuid::Builder::from_random_bytes(b).into_uuid()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choices() -> Vec<ApprovalChoiceSpec> {
        vec![
            ApprovalChoiceSpec {
                kind: ApprovalDecisionKind::Approve,
                to: "approved".into(),
                label: "承認する".into(),
                comment: ApprovalComment::Optional,
            },
            ApprovalChoiceSpec {
                kind: ApprovalDecisionKind::Return,
                to: "returned".into(),
                label: "差し戻す".into(),
                comment: ApprovalComment::Required,
            },
        ]
    }

    /// 判断の状態はその出口へ、それ以外の状態・状態なしは取り下げへ写る。
    #[test]
    fn transitions_map_to_ports() {
        let c = choices();
        let r = ApprovalResolution::transition(
            &c,
            Some("returned".into()),
            Some("bob".into()),
            None,
            Some(5),
        );
        assert_eq!(
            (r.status, r.decision, r.port),
            ("returned", "return", Some("returned"))
        );
        let r = ApprovalResolution::transition(&c, Some("draft".into()), None, None, Some(5));
        assert_eq!(
            (r.status, r.decision, r.port),
            ("withdrawn", "withdrawn", Some("withdrawn"))
        );
        assert_eq!(r.kind(), None);
        let e = ApprovalResolution::expired(OnTimeout::Fail);
        assert_eq!((e.status, e.port), ("expired", None));
        assert_eq!(
            ApprovalResolution::expired(OnTimeout::Continue).port,
            Some("timeout")
        );
        assert_eq!(e.failure, None);
        // 実行主体の失権は出口へ進めず、理由つきで run を失敗させる（取り下げとは別・D-62）。
        let a = ApprovalResolution::actor_unavailable();
        assert_eq!(
            (a.status, a.port, a.failure),
            ("cancelled", None, Some("actor_unavailable"))
        );
        assert_eq!(a.kind(), None);
        assert_eq!(ApprovalResolution::withdrawn().failure, None);
    }

    #[test]
    fn approval_id_is_stable_per_step() {
        let a = approval_id_for("t1", "wf:t1:r:ok");
        assert_eq!(a, approval_id_for("t1", "wf:t1:r:ok"));
        assert_ne!(a, approval_id_for("t2", "wf:t1:r:ok"));
        assert_ne!(a, approval_id_for("t1", "wf:t1:r:ok2"));
    }
}

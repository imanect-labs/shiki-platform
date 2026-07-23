//! 能力ゲートウェイ（INV-2 の単一チョークポイント・Task 10.6a/10.8/10.10・engine.md §9）。
//!
//! ノード実行・script の HostCall はすべてここに合流する。手順（engine.md §9.2）:
//! 1. **scope 天井交差**: 実効スコープ = 宣言スコープ ∩ 委譲（ノード設定で権限を拡大できない・縮小のみ）
//! 2. **effect_journal dedupe**: 副作用 API は高々 1 回（storage.write / workflow.start）
//! 3. rate limit（外部 API）・既存チョークポイント経由・監査記録
//!
//! 本モジュールはゲートの骨格（scope 天井・journal・監査フック）を提供する。個別能力の実処理は
//! `nodes/` のアダプタが担い、必ずこのゲートを通す。

pub mod journal;

use std::collections::BTreeSet;

use serde_json::Value;

use crate::vocab::Scope;
pub use journal::{op_digest, EffectJournal, JournalDecision, JournalError};

/// 能力呼び出しの監査シンク（run 監査・OTel へ流す）。実装は呼び出し側が注入する。
pub trait CapabilityAudit: Send + Sync {
    /// 能力呼び出しを 1 件記録する（api・許可可否・レダクト済みメタ）。
    fn record(&self, tenant_id: &str, api: &str, allowed: bool, meta: &Value);
}

/// 能力呼び出しが拒否された理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenyReason {
    /// 実効スコープに含まれない（宣言 ∩ 委譲の外）。
    OutOfScope,
    /// 冪等キー衝突かつ操作ダイジェスト不一致（permanent）。
    DigestMismatch,
}

/// scope 天井の判定結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeCeiling {
    Allowed,
    Denied(DenyReason),
}

/// 実効スコープ（宣言 ∩ 委譲）を計算する（engine.md §9.2・**ノード設定で拡大不能**）。
///
/// 委譲集合が空なら実効も空（fail-closed）。返すのは両者に共通するスコープのみ。
#[must_use]
pub fn effective_scopes(declared: &[String], delegated: &[String]) -> BTreeSet<String> {
    let deleg: BTreeSet<&String> = delegated.iter().collect();
    declared
        .iter()
        .filter(|s| deleg.contains(s))
        .cloned()
        .collect()
}

/// スコープ不要の内部制御 API（明示許可・これ以外の未マップ API は fail-closed で拒否）。
/// llm.invoke / agent.invoke は内部推論（外部到達は http.egress・データ到達は storage/rag で縛る）。
/// script.run 自体は無スコープ（内部の `Shiki.*` ホスト呼び出しが個別に scope ceiling で縛られる）。
const SCOPE_FREE_APIS: &[&str] = &[
    "control.branch",
    "control.switch",
    "control.join",
    "control.map",
    "control.wait",
    "llm.invoke",
    "agent.invoke",
    "script.run",
    // skill.invoke 自体は専用スコープを持たない（内部推論・vocab/scope.rs:41）。skill が
    // `.shiki` script / agent 経由で行う能力呼び出しは、その API のスコープが個別に効く
    // （script の Shiki.* は HostBridge が、agent は agent.invoke 経路がゲートする・#344）。
    "skill.invoke",
];

/// 能力 API に必要なスコープが実効スコープに含まれるか判定する（scope 天井）。
///
/// `api` から必要スコープを引き（[`Scope::for_api`]）、実効集合に無ければ `OutOfScope`。
/// **未マップ API は fail-closed で拒否**する（明示許可した制御 API のみスコープ不要）。
#[must_use]
pub fn check_scope_ceiling(api: &str, effective: &BTreeSet<String>) -> ScopeCeiling {
    match Scope::for_api(api) {
        Some(required) if effective.contains(required.as_str()) => ScopeCeiling::Allowed,
        // 明示許可した制御/内部 API のみスコープ不要。未知の API は天井をすり抜けさせず拒否する。
        None if SCOPE_FREE_APIS.contains(&api) => ScopeCeiling::Allowed,
        // 必要スコープ未充足、または未マップ API は fail-closed で拒否。
        Some(_) | None => ScopeCeiling::Denied(DenyReason::OutOfScope),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_is_intersection() {
        let eff = effective_scopes(
            &[
                "storage.read".into(),
                "storage.write".into(),
                "rag.search".into(),
            ],
            &["storage.read".into(), "rag.search".into()],
        );
        assert!(eff.contains("storage.read"));
        assert!(eff.contains("rag.search"));
        assert!(
            !eff.contains("storage.write"),
            "委譲されていない宣言は落ちる"
        );
    }

    #[test]
    fn empty_delegation_yields_empty_effective() {
        let eff = effective_scopes(&["storage.read".into()], &[]);
        assert!(eff.is_empty(), "委譲ゼロなら実効ゼロ（fail-closed）");
    }

    #[test]
    fn ceiling_denies_out_of_scope() {
        let eff = effective_scopes(&["storage.read".into()], &["storage.read".into()]);
        assert_eq!(
            check_scope_ceiling("storage.read", &eff),
            ScopeCeiling::Allowed
        );
        assert_eq!(
            check_scope_ceiling("storage.write", &eff),
            ScopeCeiling::Denied(DenyReason::OutOfScope)
        );
    }
}

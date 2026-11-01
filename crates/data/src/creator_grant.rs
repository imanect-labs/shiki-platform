//! 「追加だけ」の人に、作成者として許した進め方と書き換え（D-127・GAP-52）。
//!
//! 追加だけの人（D-113・[`crate::table_access`]）は、ふつうは書き換えも状態を進めることもできない。状態の進め方と
//! 書き込みの決まりが**作成者の列で本人を指す**ときだけ、その分を**自分が作成者のレコード**に許す（kintone の
//! プロセス管理で、作成者が差し戻しから直して再申請するのと同じ形）。申請する人に編集者を渡さずに済む。
//!
//! # 「作成者に許した」の判定（[`creator_part`]）
//!
//! 式のうち「作成者の列 = 本人」（`field_cmp(作成者の列, eq, $user.id)`・D-122）を通らないと満たせない部分を
//! 取り出す。
//!
//! - `field_cmp(作成者の列, eq, $user.id)`: そのもの。
//! - `any[…]`: 取り出せた枝だけの `any`。
//! - `all[…]`: 子のどれか 1 つから取り出せれば、その子を取り出した部分に置き換えた `all`（子ごとの `any`）。
//! - それ以外（`is_owner`・メンバーの列・`public`・部署）からは取り出さない。
//!
//! 取り出した部分は元の式を含意する（枝を減らすだけ）ので、編集者に許す範囲を越えない。当該行で真なら
//! 作成者 = 本人なので、他人のレコード（個別に共有された行を含む）は書き換えも進めることもできない。
//! `is_owner` は数えない: 前からの定義の読み・書き込みの決まりに広く使われており（編集者は自分の行だけ、など）、
//! 数えると保存済みのテーブルで追加だけの人の権限が黙って広がる。作成者の列の無いテーブル・メンバーの列で
//! 本人を指すテーブルは、これまでどおり断る。
//!
//! # 許すもの
//!
//! - **状態を進める**: 進め方（from → to）の進められる人から取り出した部分が、当該行で真のとき。
//! - **書き換え**: (1) いまの状態から、作成者に許した進め方のどれかが当該行で真で、(2) 書き込みの決まりから
//!   取り出した部分が当該行で真のとき（「その状態のあいだ作成者の書き換えを許す」決まり）。行の決まりの無い
//!   テーブルの書き換えは許さない（状態は進められる）。状態の進め方の画面の決まり（[`crate::fsm_write_guard`]）は、
//!   作成者が進められる状態のあいだ、作成者の書き換えを許す枝を自動で持つ。
//! - 削除・一括取込・作成者の列への書き込み・状態の列の直接の書き換えは、これまでどおり断る。
//!
//! どの入口（REST・ワークフロー・ゲートウェイ・チャット・script）も [`crate::DataStore::update_record`] と
//! [`crate::FsmStore::transition`] を通るので、判定はここ 1 か所になる。画面の出し分けの手がかり
//! （[`OwnRecordRights`]）も同じ取り出しから作る。

use authz::AuthContext;
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::fsm::{FsmBody, FsmTransition};
use crate::model::{DataTable, TableSchema};
use crate::policy::ast::MAX_POLICY_BRANCHES;
use crate::policy::memo::AuthzMemo;
use crate::policy::{CmpOp, PolicyExpr, PolicyOperand};
use crate::store::DataStore;
use crate::DataError;

/// 追加だけの人に作成者として許したもの（テーブルの状態の進め方と状態の列・[`crate::table_access`] が渡す）。
#[derive(Debug, Clone)]
pub(crate) struct CreatorGrant {
    pub(crate) fsm: FsmBody,
    pub(crate) status_field: String,
}

/// 作成者として何を許しているかを確かめる場面（[`DataStore::creator_grant`]）。
#[derive(Debug, Clone, Copy)]
pub(crate) enum GrantNeed<'a> {
    /// 書き換え（テーブルが指す状態の進め方を読み、書き込みの決まりも作成者を指すこと）。
    Write,
    /// 状態を進める（テーブルが指す状態の進め方を読む）。
    Move,
    /// 状態を進める（呼び出し側が読んだ状態の進め方と状態の列）。
    MoveWith(&'a FsmBody, &'a str),
}

/// 追加だけの人が、自分が追加したレコードでできること（D-127）。画面の出し分けの手がかりだけで、実際の書き換え・
/// 状態を進めるは各 API が当該行で改めて判定する。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct OwnRecordRights {
    /// 状態を進められることがある状態（作成者に許した進め方の元の状態・状態の進め方の順）。
    pub move_from: Vec<String>,
    /// 直せることがある状態（`move_from` のうち、書き込みの決まりが作成者の書き換えを許す状態）。
    pub edit_in: Vec<String>,
}

/// 作成者の列か（[`crate::model::FieldType::CreatedBy`]）。
fn creator_column(schema: &TableSchema) -> impl Fn(&str) -> bool + '_ {
    |f: &str| {
        schema
            .field(f)
            .is_some_and(crate::model::FieldDef::is_created_by)
    }
}

fn one_or_any(mut v: Vec<PolicyExpr>) -> Option<PolicyExpr> {
    match v.len() {
        0 => None,
        1 => v.pop(),
        _ => Some(PolicyExpr::Any(v)),
    }
}

/// `expr` のうち「`is_creator` の列 = 本人」を通らないと満たせない部分（無ければ `None`）。
///
/// `all` の子ごとの取り出しは [`MAX_POLICY_BRANCHES`] 通りまで（それより多い分は取り出さない＝許す範囲が
/// 狭くなる側に倒す）。
pub(crate) fn creator_part(
    expr: &PolicyExpr,
    is_creator: &dyn Fn(&str) -> bool,
) -> Option<PolicyExpr> {
    match expr {
        PolicyExpr::FieldCmp {
            field,
            op: CmpOp::Eq,
            value: PolicyOperand::UserId,
        } if is_creator(field) => Some(expr.clone()),
        PolicyExpr::Any(children) => one_or_any(
            children
                .iter()
                .filter_map(|c| creator_part(c, is_creator))
                .take(MAX_POLICY_BRANCHES)
                .collect(),
        ),
        PolicyExpr::All(children) => {
            let mut alts = Vec::new();
            for (i, c) in children.iter().enumerate() {
                let Some(part) = creator_part(c, is_creator) else {
                    continue;
                };
                if part == *c {
                    // この子そのものが作成者を指す: 全体が作成者を通る。
                    return Some(expr.clone());
                }
                let mut v = children.clone();
                v[i] = part;
                alts.push(PolicyExpr::All(v));
                if alts.len() >= MAX_POLICY_BRANCHES {
                    break;
                }
            }
            one_or_any(alts)
        }
        _ => None,
    }
}

/// 状態 `state` で、`is_creator` の列 = 本人のレコードについて `expr` が成り立ちうるか（行のほかの値と部署は
/// 分からないものとして扱う）。画面の手がかりと、書き込みの決まりの組み立て（[`resubmit_states`]）に使う。
/// 実際の判定は当該行の SQL が決める。
pub(crate) fn may_hold(
    expr: &PolicyExpr,
    status_field: &str,
    state: &str,
    is_creator: &dyn Fn(&str) -> bool,
) -> bool {
    tri(expr, status_field, state, is_creator) != Some(false)
}

/// 3 値（成り立つ・成り立たない・分からない）の評価。
fn tri(
    expr: &PolicyExpr,
    status_field: &str,
    state: &str,
    is_creator: &dyn Fn(&str) -> bool,
) -> Option<bool> {
    match expr {
        PolicyExpr::Public | PolicyExpr::IsOwner => Some(true),
        PolicyExpr::HasRole { .. } => None,
        PolicyExpr::FieldCmp { field, op, value } => match value {
            PolicyOperand::UserId if is_creator(field) => match op {
                CmpOp::Eq => Some(true),
                CmpOp::Ne => Some(false),
                CmpOp::In => None,
            },
            PolicyOperand::Lit(v) if field == status_field => match (op, v) {
                (CmpOp::Eq, Value::String(s)) => Some(s == state),
                (CmpOp::Ne, Value::String(s)) => Some(s != state),
                (CmpOp::In, Value::Array(a)) => Some(a.iter().any(|x| x.as_str() == Some(state))),
                _ => None,
            },
            _ => None,
        },
        PolicyExpr::Any(children) => {
            let vs: Vec<Option<bool>> = children
                .iter()
                .map(|c| tri(c, status_field, state, is_creator))
                .collect();
            if vs.contains(&Some(true)) {
                Some(true)
            } else if !vs.is_empty() && vs.iter().all(|v| *v == Some(false)) {
                Some(false)
            } else {
                None
            }
        }
        PolicyExpr::All(children) => {
            let vs: Vec<Option<bool>> = children
                .iter()
                .map(|c| tri(c, status_field, state, is_creator))
                .collect();
            if vs.is_empty() || vs.contains(&Some(false)) {
                Some(false)
            } else if vs.iter().all(|v| *v == Some(true)) {
                Some(true)
            } else {
                None
            }
        }
    }
}

/// 進め方 `t` のうち、`is_creator` の列 = 本人に許した部分（元の状態で成り立ちえないものは除く。例えば
/// 「作成者」と「申請した本人は除く」を両方付けた進め方）。
fn move_part(
    t: &FsmTransition,
    status_field: &str,
    is_creator: &dyn Fn(&str) -> bool,
) -> Option<PolicyExpr> {
    creator_part(&t.actor, is_creator).filter(|p| may_hold(p, status_field, &t.from, is_creator))
}

/// 作成者（`applicant` の列 = 本人）が進められる進め方の元の状態のうち、最初の状態を除くもの（状態の進め方の順・
/// 重複なし）。書き込みの決まり（[`crate::fsm_write_guard`]）が、この状態のあいだ作成者の書き換えを許す。
pub(crate) fn resubmit_states(body: &FsmBody, status_field: &str, applicant: &str) -> Vec<String> {
    let is_applicant = |f: &str| f == applicant;
    let initial = body.states.first();
    body.states
        .iter()
        .filter(|s| Some(*s) != initial)
        .filter(|s| {
            body.transitions
                .iter()
                .filter(|t| &t.from == *s)
                .any(|t| move_part(t, status_field, &is_applicant).is_some())
        })
        .cloned()
        .collect()
}

/// 書き込みの決まりのうち、作成者の書き換えを許す部分（行の決まりが無ければ `None`）。
fn write_part(schema: &TableSchema) -> Option<PolicyExpr> {
    let is_creator = creator_column(schema);
    schema
        .row_policy
        .as_ref()
        .and_then(|p| creator_part(p.write_expr(), &is_creator))
}

/// 画面の手がかり（[`OwnRecordRights`]）。作成者に許した進め方が無ければ `None`。
fn rights_of(schema: &TableSchema, fsm: &FsmBody, status_field: &str) -> Option<OwnRecordRights> {
    let is_creator = creator_column(schema);
    let move_from: Vec<String> = fsm
        .states
        .iter()
        .filter(|s| {
            fsm.transitions
                .iter()
                .filter(|t| &t.from == *s)
                .any(|t| move_part(t, status_field, &is_creator).is_some())
        })
        .cloned()
        .collect();
    if move_from.is_empty() {
        return None;
    }
    let write = write_part(schema);
    let edit_in = move_from
        .iter()
        .filter(|s| {
            write
                .as_ref()
                .is_some_and(|w| may_hold(w, status_field, s, &is_creator))
        })
        .cloned()
        .collect();
    Some(OwnRecordRights { move_from, edit_in })
}

impl DataStore {
    /// 追加だけの人に作成者として許したもの（作成者の列・状態の進め方・作成者に許した進め方が要る。書き換えなら
    /// 書き込みの決まりも作成者を指すこと）。何も許していなければ `None`（呼び出し側がこれまでどおり断る）。
    ///
    /// 形だけを見る（当該行の判定は [`Self::creator_may_transition`] / [`Self::creator_may_write`]）。
    pub(crate) async fn creator_grant(
        &self,
        ctx: &AuthContext,
        table: &DataTable,
        need: GrantNeed<'_>,
        trace_id: Option<&str>,
    ) -> Result<Option<CreatorGrant>, DataError> {
        let schema = &table.schema;
        if !crate::created_by::any(schema) {
            return Ok(None);
        }
        let (fsm, status_field) = match need {
            GrantNeed::MoveWith(fsm, status) => (fsm.clone(), status.to_string()),
            GrantNeed::Write | GrantNeed::Move => {
                if schema.fsm_ref.is_none() || schema.status_field.is_none() {
                    return Ok(None);
                }
                match self.pinned_fsm(ctx, table, trace_id).await? {
                    Some(x) => x,
                    None => return Ok(None),
                }
            }
        };
        // 画面の手がかりと同じ形の判定（進められる状態が無い・直せる状態が無いなら、何も許していない）。
        let granted = match rights_of(schema, &fsm, &status_field) {
            Some(r) => !matches!(need, GrantNeed::Write) || !r.edit_in.is_empty(),
            None => false,
        };
        Ok(granted.then_some(CreatorGrant { fsm, status_field }))
    }

    /// 追加だけの人がこのレコードを `from` から `to` へ進められるか（作成者に許した部分を当該行で評価する）。
    ///
    /// `table` は絞る前の定義（進められる人は行の決まりとは別）。行が見えることは呼び出し側が確かめておく。
    #[allow(clippy::too_many_arguments)] // 主体・メモ・テーブル・行・許したもの・組は全て別物。
    pub(crate) async fn creator_may_transition(
        &self,
        ctx: &AuthContext,
        az: &AuthzMemo<'_>,
        table: &DataTable,
        record_id: Uuid,
        grant: &CreatorGrant,
        from: &str,
        to: &str,
    ) -> Result<bool, DataError> {
        let is_creator = creator_column(&table.schema);
        for t in grant.fsm.transitions_between(from, to) {
            if let Some(part) = move_part(t, &grant.status_field, &is_creator) {
                if self.actor_allows(ctx, az, table, record_id, &part).await? {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// 追加だけの人がこのレコードを書き換えられるか（いまの状態 `from` から作成者に許した進め方が当該行で成り立ち、
    /// 書き込みの決まりのうち作成者に許した部分も当該行で成り立つ）。
    ///
    /// `table` は絞る前の定義（書き込みの決まりは絞る前のもの）。行が見えることは呼び出し側が確かめておく。
    pub(crate) async fn creator_may_write(
        &self,
        ctx: &AuthContext,
        az: &AuthzMemo<'_>,
        table: &DataTable,
        record_id: Uuid,
        grant: &CreatorGrant,
        from: Option<&str>,
    ) -> Result<bool, DataError> {
        let (Some(from), Some(write)) = (from, write_part(&table.schema)) else {
            return Ok(false);
        };
        let is_creator = creator_column(&table.schema);
        let mut movable = false;
        for t in grant.fsm.transitions.iter().filter(|t| t.from == from) {
            if let Some(part) = move_part(t, &grant.status_field, &is_creator) {
                if self.actor_allows(ctx, az, table, record_id, &part).await? {
                    movable = true;
                    break;
                }
            }
        }
        if !movable {
            return Ok(false);
        }
        self.actor_allows(ctx, az, table, record_id, &write).await
    }
}

/// 追加だけの閲覧者の画面の手がかり（[`OwnRecordRights`]・閲覧者の文脈が、指している状態の進め方を読んで渡す）。
///
/// `schema` は絞る前の定義（書き込みの決まりは絞る前のもの）。作成者の列が無いか、作成者に許した進め方が無ければ
/// `None`。
pub(crate) fn own_record_rights(
    schema: &TableSchema,
    fsm: &FsmBody,
    status_field: &str,
) -> Option<OwnRecordRights> {
    if !crate::created_by::any(schema) {
        return None;
    }
    rights_of(schema, fsm, status_field)
}

#[cfg(test)]
#[path = "creator_grant_tests.rs"]
mod tests;

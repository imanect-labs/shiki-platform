//! 受信箱の所属ロールと件数（`approvals.rs` から分割・BUG-43・BUG-161）。
//!
//! - 所属ロールは、台帳の担当のロール（候補）を先に引き、そのうち本人が所属するものだけを OpenFGA に確かめる
//!   （受信箱・件数・1 件・判断が照らすのは担当のロールだけ）。担当のロール宛ての承認が無ければ OpenFGA に問わない。
//!   候補が多いときだけ、従来どおり所属ロールの実効集合（ListObjects）を引く。
//! - 件数は、台帳の数えるのに要る列だけ（[`workflow_engine::ApprovalKey`]）を引き、行が本人に見えるかと今の状態だけを
//!   data に確かめる（[`data::DataStore::filter_visible_states`]・行の中身・派生値・リンクの表示名を作らない）。
//!   見え方は一覧と同じ（見えない承認を数えない・状態が動いたものを数えない）。

use std::collections::HashSet;

use authz::{AuthContext, ObjectType, Relation};
use uuid::Uuid;
use workflow_engine::{ApprovalKey, InboxCursor};

use super::approvals::ApprovalCountResponse;
use crate::{error::ApiError, state::AppState};

/// 1 回に読み直す台帳の行（件数）。
const COUNT_PAGE: i64 = 100;
/// 件数の打ち切り（バッジは 99+）。
const COUNT_CAP: usize = 99;
/// 一覧の 1 回・件数の 1 回で読み直す台帳の行の上限（見えない行ばかりでも際限なく読まない）。
pub(super) const SCAN_LIMIT: usize = 500;
/// 担当のロールの候補がこれ以下なら、所属ロールの実効集合（ListObjects）を引かずに候補だけを OpenFGA に確かめる。
const DIRECT_ROLE_CHECKS: i64 = 100;
/// 所属ロールの実効集合が ListObjects の上限で切れたときに、OpenFGA に確かめる担当のロールの数の上限
/// （[`my_roles`]・BUG-161）。
const MAX_ROLE_CHECKS: i64 = 1_000;
/// OpenFGA の object 識別子の最大長（`^[^\s]{2,256}$`）。
const MAX_FGA_OBJECT_LEN: usize = 256;

fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError::Internal(format!("approvals: {e}"))
}

fn unavailable(e: authz::AuthzError) -> ApiError {
    ApiError::ServiceUnavailable(format!("authz: {e}"))
}

/// 本人の所属ロール（ローカル id・配下ロール込み）のうち、承認の担当のロール（`approval` があればその承認の、
/// 無ければ保留中の承認の）にあるもの。
///
/// 担当のロールの候補が [`DIRECT_ROLE_CHECKS`] 件以下なら、候補だけを本人が所属するか OpenFGA に確かめる（候補が
/// 無ければ問わない・台帳の候補が pre-filter、OpenFGA の判定が post-filter）。多いときは実効集合（ListObjects）を
/// 引き、応答の上限で切れたら（BUG-161）集合に無い候補を [`MAX_ROLE_CHECKS`] 件まで確かめて足す。
pub(super) async fn my_roles(
    state: &AppState,
    ctx: &AuthContext,
    approval: Option<Uuid>,
) -> Result<Vec<String>, ApiError> {
    let ledger = &state.workflow_approvals;
    let few = ledger
        .assignee_roles(&ctx.tenant_id, &ctx.org, approval, DIRECT_ROLE_CHECKS + 1)
        .await
        .map_err(internal)?;
    if i64::try_from(few.len()).unwrap_or(i64::MAX) <= DIRECT_ROLE_CHECKS {
        return held_roles(state, ctx, few, &HashSet::new()).await;
    }
    let raw = state
        .authz
        .list_objects(&ctx.subject(), Relation::Member, ObjectType::Role)
        .await
        .map_err(unavailable)?;
    let ns = ctx.ns();
    let mut roles: Vec<String> = raw
        .iter()
        .filter_map(|o| o.split_once(':').map(|(_, id)| id))
        .filter_map(|id| ns.strip_object_id(id))
        .map(str::to_string)
        .collect();
    if !authz::visible::list_may_be_cut(raw.len()) {
        return Ok(roles);
    }
    let candidates = ledger
        .assignee_roles(&ctx.tenant_id, &ctx.org, approval, MAX_ROLE_CHECKS)
        .await
        .map_err(internal)?;
    // 候補は名前の順で上限まで。届いたら、後ろのロール宛ての承認はこの人の受信箱に出ない（件数にも数えない）ので、
    // 黙って切らずに残す（応答には印を足さない・BUG-161 の残り）。
    if i64::try_from(candidates.len()).unwrap_or(i64::MAX) >= MAX_ROLE_CHECKS {
        tracing::warn!(
            tenant_id = %ctx.tenant_id,
            org = %ctx.org,
            approval = ?approval,
            limit = MAX_ROLE_CHECKS,
            "所属ロールが上限に届いた人の承認の担当のロールの候補が上限に届きました。名前の順で後ろのロール宛ての承認は、\
             この人の受信箱に出ません"
        );
    }
    let known: HashSet<String> = roles.iter().cloned().collect();
    let more = held_roles(state, ctx, candidates, &known).await?;
    roles.extend(more);
    Ok(roles)
}

/// 担当のロールの候補のうち、`known`（実効集合で分かっている分）に無く、本人が所属するもの（候補の順）。
///
/// 台帳の値は作者が書いたもの。OpenFGA が object として受け付けない形は問わない（誰も所属できない・BatchCheck は
/// 1 問の形式違反で全体を拒否する）。
async fn held_roles(
    state: &AppState,
    ctx: &AuthContext,
    candidates: Vec<String>,
    known: &HashSet<String>,
) -> Result<Vec<String>, ApiError> {
    let ns = ctx.ns();
    let ask: Vec<(String, authz::FgaObject)> = candidates
        .into_iter()
        .filter(|r| !known.contains(r) && authz::validate_local_id(r).is_ok())
        .map(|r| {
            let object = ns.role(&r);
            (r, object)
        })
        .filter(|(_, o)| o.as_str().len() <= MAX_FGA_OBJECT_LEN)
        .collect();
    let objects: Vec<authz::FgaObject> = ask.iter().map(|(_, o)| o.clone()).collect();
    let held = authz::visible::holds_any(
        state.authz.as_ref(),
        &ctx.subject(),
        &[Relation::Member],
        &objects,
        authz::Consistency::HigherConsistency,
    )
    .await
    .map_err(unavailable)?;
    Ok(ask
        .into_iter()
        .zip(held)
        .filter_map(|((r, _), member)| member.then_some(r))
        .collect())
}

/// 自分宛ての承認待ちの件数（一覧と同じ絞り込み・99 で打ち切り・読み直す台帳の行の上限で打ち切ったら `more`）。
pub(super) async fn count(
    state: &AppState,
    ctx: &AuthContext,
) -> Result<ApprovalCountResponse, ApiError> {
    let roles = my_roles(state, ctx, None).await?;
    let mut count = 0usize;
    let mut scanned = 0usize;
    let mut truncated = false;
    let mut after: Option<InboxCursor> = None;
    loop {
        let keys = state
            .workflow_approvals
            .inbox_keys(
                &ctx.tenant_id,
                &ctx.org,
                &ctx.principal.id,
                &roles,
                after,
                COUNT_PAGE,
            )
            .await
            .map_err(internal)?;
        let n = keys.len();
        scanned += n;
        after = keys.last().map(|k| InboxCursor {
            created_at: k.created_at,
            approval_id: k.approval_id,
        });
        count += visible_count(state, ctx, &keys).await?;
        if count > COUNT_CAP || (n as i64) < COUNT_PAGE {
            break;
        }
        if scanned >= SCAN_LIMIT {
            // 見えない承認が多く数え切れなかった（数えた分より多いかもしれない）。
            truncated = true;
            break;
        }
    }
    Ok(ApprovalCountResponse {
        count: u32::try_from(count.min(COUNT_CAP)).unwrap_or(99),
        more: count > COUNT_CAP || truncated,
    })
}

/// 台帳の承認のうち、行が本人に見えて、今の状態が停止時の状態のものの数（テーブルごとに 1 本のクエリ）。
///
/// 見えない・無い・確定的に読めないテーブルは数えない（一覧と同じ・Deny の監査に残さない・D-68）。一時障害だけを
/// 応答の誤りにする。
async fn visible_count(
    state: &AppState,
    ctx: &AuthContext,
    keys: &[ApprovalKey],
) -> Result<usize, ApiError> {
    let mut tables: Vec<Uuid> = Vec::new();
    for k in keys {
        if !tables.contains(&k.table_id) {
            tables.push(k.table_id);
        }
    }
    let mut count = 0;
    for table_id in tables {
        let mine: Vec<&ApprovalKey> = keys.iter().filter(|k| k.table_id == table_id).collect();
        let ids: Vec<Uuid> = mine.iter().map(|k| k.record_id).collect();
        let states = match state.data.filter_visible_states(ctx, table_id, &ids).await {
            Ok(Some(s)) => s,
            Ok(None) | Err(data::DataError::NotFound | data::DataError::Forbidden) => continue,
            Err(e) if e.is_transient() => return Err(e.into()),
            Err(e) => {
                tracing::warn!(error = %e, %table_id, "承認のテーブルを読めないので件数に数えない");
                continue;
            }
        };
        count += mine
            .iter()
            .zip(states)
            .filter(|(k, s)| {
                s.as_ref().and_then(|s| s.status.as_deref()) == Some(k.from_state.as_str())
            })
            .count();
    }
    Ok(count)
}

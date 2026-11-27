//! outbox のイベントを起こした書込の由来の run（因果の鎖・`by_workflow`・BUG-44）。
//!
//! run の中の書込は outbox の consumer 非公開部に由来の run の印を必ず載せる（`storage::event::write_origins`・
//! 文脈の `AuthContext::origin_run`）。relay は印だけを信じ、書込の trace_id からは run を推しはからない
//! （利用者の要求の trace_id も run の trace_id と同じ 32 桁 hex の形なので、名乗れば抑止を偽れ、実在しない run
//! の id が `trigger_firing.cause_run_id` に残っていた）。
//!
//! 印のキーが無い行は、印を書く前の版が発行した行だけ（この版への移行の間・ローリングデプロイの古い版）。その
//! 行に限り従来どおり trace_id から run を推しはかり、実在する run のときだけ由来とする（古い版の run の書込で
//! 自分を起こす循環を、移行の間にも止める）。新しい版の行では trace_id を読まず、run の実在も問わない。

use std::collections::{HashMap, HashSet};

use storage::event::{OutboxEvent, WriteOrigin};
use uuid::Uuid;

/// outbox の id → イベントを起こした書込の由来の run（run の書込でなければ `None`）。
pub(super) type Causes = HashMap<i64, Option<Uuid>>;

/// 1 件の印の読み方。
#[derive(Debug, PartialEq, Eq)]
enum Classified {
    /// 印で決まった（run の書込ならその run）。
    Known(Option<Uuid>),
    /// 印の無い行で、trace_id が run の形だった（実在を確かめてから由来にする）。
    Legacy(Uuid),
}

fn classify(origin: WriteOrigin, trace_id: Option<&str>) -> Classified {
    match origin {
        WriteOrigin::Run(run) => Classified::Known(Some(run)),
        WriteOrigin::NotRun => Classified::Known(None),
        WriteOrigin::Unmarked => match workflow_engine::scheduler::cause_run_of_trace(trace_id) {
            Some(run) => Classified::Legacy(run),
            None => Classified::Known(None),
        },
    }
}

/// イベントの由来の run をまとめて引く（印は 1 回の照会・印の無い行の run の実在も 1 回の照会）。
///
/// 呼び出し側は、候補のトリガ・wait があるイベントだけを渡す（候補の無い配送で照会を増やさない）。
pub(super) async fn cause_runs(
    db: &sqlx::PgPool,
    events: &[&OutboxEvent],
) -> Result<Causes, anyhow::Error> {
    if events.is_empty() {
        return Ok(Causes::new());
    }
    let ids: Vec<i64> = events.iter().map(|e| e.id).collect();
    let origins = {
        let mut conn = db.acquire().await?;
        storage::event::write_origins(&mut conn, &ids).await?
    };
    let mut out = Causes::with_capacity(events.len());
    let mut legacy: Vec<(i64, &str, Uuid)> = Vec::new();
    for &ev in events {
        let origin = origins
            .get(&ev.id)
            .copied()
            .unwrap_or(WriteOrigin::Unmarked);
        match classify(origin, ev.trace_id.as_deref()) {
            Classified::Known(cause) => {
                out.insert(ev.id, cause);
            }
            Classified::Legacy(run) => legacy.push((ev.id, ev.tenant_id.as_str(), run)),
        }
    }
    if !legacy.is_empty() {
        let tenants: Vec<&str> = legacy.iter().map(|(_, t, _)| *t).collect();
        let runs: Vec<Uuid> = legacy.iter().map(|(_, _, r)| *r).collect();
        let existing: HashSet<(String, Uuid)> = sqlx::query_as::<_, (String, Uuid)>(
            "SELECT tenant_id, run_id FROM workflow_run \
             WHERE (tenant_id, run_id) IN (SELECT * FROM unnest($1::text[], $2::uuid[]))",
        )
        .bind(&tenants)
        .bind(&runs)
        .fetch_all(db)
        .await?
        .into_iter()
        .collect();
        for (id, tenant, run) in legacy {
            let known = existing.contains(&(tenant.to_string(), run));
            out.insert(id, known.then_some(run));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 新しい版の行は印だけで決まり、trace_id が run の形でも読まない（実在の照会も走らない）。
    #[test]
    fn marked_rows_ignore_the_trace_id() {
        let run = Uuid::new_v4();
        let looks_like_a_run = run.simple().to_string();
        assert_eq!(
            classify(WriteOrigin::NotRun, Some(&looks_like_a_run)),
            Classified::Known(None),
            "利用者の要求の trace_id は run の形でも由来にならない"
        );
        let other = Uuid::new_v4();
        assert_eq!(
            classify(WriteOrigin::Run(other), Some(&looks_like_a_run)),
            Classified::Known(Some(other)),
            "印の run が正"
        );
    }

    /// 印の無い行（印を書く前の版）だけ、trace_id が run の形なら実在を確かめる候補にする。
    #[test]
    fn unmarked_rows_fall_back_to_the_trace_id() {
        let run = Uuid::new_v4();
        assert_eq!(
            classify(WriteOrigin::Unmarked, Some(&run.simple().to_string())),
            Classified::Legacy(run)
        );
        assert_eq!(
            classify(WriteOrigin::Unmarked, Some(&run.to_string())),
            Classified::Known(None),
            "run の形でない trace_id"
        );
        assert_eq!(
            classify(WriteOrigin::Unmarked, None),
            Classified::Known(None)
        );
    }
}

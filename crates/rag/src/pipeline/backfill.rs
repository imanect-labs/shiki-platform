//! 取り込みの記録が無いファイルのまとめての取り込みを、少しずつ積む（D-126・GAP-51）。
//!
//! 管理者が始めたもの（`rag_backfill` の `running` の行・[`crate::SearchService::start_backfill`]）を、リーダーの
//! 取り込み（[`super::run_as_leader`]）が 1 回に 1 件ずつ進める。1 回の手順は 1 Tx で、
//!
//! 1. 動いているもののうち、いちばん長く進めていないものを `FOR UPDATE SKIP LOCKED` で押さえる（テナントの間で
//!    順番に進める）。
//! 2. その組織の取り込みを待っている作業が [`MAX_WAITING`] 件以上なら、積まずに次の回へ回す（大きな組織でも
//!    待ち行列を溜めない。取り込みが進むと待つ件数が減り、また積む。同じテナントのほかの組織の待ちでは止めない）。
//! 3. 続きの位置（ファイルの id の順）から [`SCAN_BATCH`] 件のファイルを読み、取り込みの状態を同じ判定
//!    （`index_state.rs` の `classify`）で決め、取り込みの記録が無いものだけを outbox に積む（空いている件数まで）。
//! 4. 続きの位置と件数を進め、最後まで読んだら `done` にする。
//!
//! 1 回の手順がロールバックしても、続きの位置は前のまま（積んだイベントと位置は同じ Tx）なので、次の回に同じ位置から
//! やり直す。ファイルごとの依頼（[`crate::SearchService::request_reindex`]）と重なっても、同じファイルの xact ロックの
//! 後で状態を決めるので二重に積まない。

use std::time::Duration;

use authz::{AuthContext, Principal, PrincipalKind};
use sqlx::Postgres;
use uuid::Uuid;

use super::PipelineDeps;
use crate::error::RagError;
use crate::search::index_state::{classify, facts_on, precheck};
use crate::search::reingest::{backfill_target, lock_nodes, reindex_event, waiting_on};

/// 1 回に読むファイルの数。
pub const SCAN_BATCH: i64 = 200;
/// 組織の取り込みを待っている作業がこの件数に達したら、積むのを待つ。
pub const MAX_WAITING: i64 = 100;
/// 進めるものが無い・待つときの間隔。
pub(super) const IDLE: Duration = Duration::from_secs(2);
/// 続きがあるときの間隔（ほかの処理に DB を譲る）。
pub(super) const BUSY: Duration = Duration::from_millis(200);

/// 進めたまとめての取り込み（`rag_backfill` の行）。
#[derive(sqlx::FromRow)]
struct Running {
    id: i64,
    tenant_id: String,
    org: String,
    requested_by: String,
    after_id: Option<Uuid>,
    trace_id: Option<String>,
}

/// 読んだファイル（取り込めるかを中身を見ずに決める材料）。
#[derive(sqlx::FromRow)]
struct Candidate {
    id: Uuid,
    name: String,
    content_type: Option<String>,
    size_bytes: Option<i64>,
}

/// 1 回の手順の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOutcome {
    /// 動いているものが無い。
    Idle,
    /// 待っている作業が多いので積まなかった。
    Throttled,
    /// 読んで、積んだ（`done` は最後まで読み終えたか）。
    Advanced {
        scanned: usize,
        queued: usize,
        done: bool,
    },
}

/// まとめての取り込みを 1 回進める。
pub async fn step_once(deps: &PipelineDeps) -> Result<StepOutcome, RagError> {
    let mut tx = deps.pool.begin().await?;
    let Some(run) = sqlx::query_as::<_, Running>(
        "select id, tenant_id, org, requested_by, after_id, trace_id from rag_backfill \
         where status = 'running' order by updated_at, id limit 1 for update skip locked",
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(StepOutcome::Idle);
    };
    // DB の値も信頼しない。文字ポリシーに反する tenant_id では積まずに終える（fail-closed・#1032）。止めずに
    // 返すと、同じ行が先頭に残ってほかのテナントのまとめての取り込みを塞ぐので、終えてから誤りを返す。
    let tenant = match authz::TenantId::parse(run.tenant_id.as_str()) {
        Ok(tenant) => tenant,
        Err(e) => {
            finish(&mut tx, run.id, None, 0, 0, true).await?;
            tx.commit().await?;
            return Err(RagError::InvalidTenant(e.to_string()));
        }
    };
    // 削除中・削除済みのテナントには積まない（撤去と並行して索引を作り直させない・SAAS.2）。
    if !deps
        .indexer_storage
        .tenant_is_active(&run.tenant_id)
        .await?
    {
        finish(&mut tx, run.id, None, 0, 0, true).await?;
        tx.commit().await?;
        return Ok(StepOutcome::Advanced {
            scanned: 0,
            queued: 0,
            done: true,
        });
    }
    let waiting = waiting_on(&mut tx, &run.tenant_id, &run.org, MAX_WAITING).await?;
    let budget = usize::try_from(MAX_WAITING - waiting).unwrap_or(0);
    if budget == 0 {
        // 順番を後ろへ回す（ほかのテナントのものを先に進める）。
        sqlx::query("update rag_backfill set updated_at = now() where id = $1")
            .bind(run.id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(StepOutcome::Throttled);
    }

    let candidates: Vec<Candidate> = sqlx::query_as(
        "select id, name, content_type, size_bytes from node \
         where tenant_id = $1 and org = $2 and kind = 'file' and deleted_at is null \
           and system = false and ($3::uuid is null or id > $3) \
         order by id limit $4",
    )
    .bind(&run.tenant_id)
    .bind(&run.org)
    .bind(run.after_id)
    .bind(SCAN_BATCH)
    .fetch_all(&mut *tx)
    .await?;
    // 中身を見る前に取り込めないと決まるもの（種類・大きさ）は、状態を引かない。
    let checkable: Vec<Uuid> = candidates
        .iter()
        .filter(|c| {
            precheck(
                &deps.config,
                c.content_type.as_deref(),
                &c.name,
                c.size_bytes,
            )
            .is_none()
        })
        .map(|c| c.id)
        .collect();
    lock_nodes(&mut tx, &checkable).await?;
    let facts = facts_on(&mut tx, &run.tenant_id, &run.org, &checkable).await?;
    let needs: std::collections::HashMap<Uuid, i64> = facts
        .iter()
        .filter(|f| backfill_target(classify(f)))
        .map(|f| (f.id, f.content))
        .collect();

    let mut events = Vec::new();
    let mut scanned = 0usize;
    let mut last = run.after_id;
    for c in &candidates {
        if let Some(version) = needs.get(&c.id) {
            // 空いている件数を使い切ったら、このファイルの手前で止める（次の回にここから読む）。
            if events.len() >= budget {
                break;
            }
            events.push((reindex_event(c.id, *version, "backfill"), None));
        }
        scanned += 1;
        last = Some(c.id);
    }
    let done = scanned == candidates.len() && candidates.len() < SCAN_BATCH as usize;
    if !events.is_empty() {
        let actor = AuthContext::new(
            Principal {
                kind: PrincipalKind::User,
                id: run.requested_by.clone(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some(run.tenant_id.clone()),
            },
            run.org.clone(),
            tenant,
        );
        storage::event::emit_many_on(&mut tx, &actor, &events, run.trace_id.as_deref()).await?;
    }
    finish(&mut tx, run.id, last, scanned, events.len(), done).await?;
    tx.commit().await?;
    Ok(StepOutcome::Advanced {
        scanned,
        queued: events.len(),
        done,
    })
}

/// 続きの位置と件数を進める（`done` なら終える）。
async fn finish(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    id: i64,
    after_id: Option<Uuid>,
    scanned: usize,
    queued: usize,
    done: bool,
) -> Result<(), RagError> {
    sqlx::query(
        "update rag_backfill set after_id = coalesce($2, after_id), \
             files_scanned = files_scanned + $3, files_queued = files_queued + $4, \
             updated_at = now(), \
             status = case when $5 then 'done' else status end, \
             finished_at = case when $5 then now() else finished_at end \
         where id = $1",
    )
    .bind(id)
    .bind(after_id)
    .bind(i64::try_from(scanned).unwrap_or(i64::MAX))
    .bind(i64::try_from(queued).unwrap_or(i64::MAX))
    .bind(done)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

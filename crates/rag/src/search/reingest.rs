//! 社内文書の検索への取り込み直し（D-126・GAP-51）。
//!
//! 取り込まれなかったファイルを、利用者と管理者の手で取り込み直せるようにする。
//!
//! - **ファイルごと**（[`SearchService::request_reindex`]）: ファイルを編集できる人が頼む（認可は呼び出し側が
//!   `StorageService::authorize_reindex` で済ませた証拠の [`storage::Node`] を渡す）。取り込みの記録が無いファイルと、
//!   取り込みに失敗したファイルだけを積む。
//! - **まとめて**（[`SearchService::start_backfill`]）: テナントの管理者が頼む。取り込みの記録が無いファイルだけを、
//!   取り込みの待ち行列が溜まらないよう少しずつ積む（1 回で全部は積まない・[`crate::pipeline::backfill`]）。
//!   取り込みに失敗したファイルは、壊れたファイルを繰り返し取り込まないよう対象にしない（ファイルごとに頼む）。
//!
//! どちらも、取り込みの状態を確かめた同じ Tx で outbox に [`storage::event::WriteOp::Reindex`] を積み、relay →
//! `job_queue` → 取り込み（`rag_ingest_job` の冪等キー `(tenant, node, version, op)`）の既存の経路を通す。積むかの
//! 判定は取り込みの状態（[`IndexState`]・`index_state.rs` の 1 か所）から決める:
//!
//! - いまの内容が索引にある・取り込み中・取り込みへ向かう記録がある: 積まない（同じ内容の版を二重に取り込まない）。
//! - 取り込めない種類・大きすぎる・取り込まない場所・本文が無かった: 積まない（取り込み直しても読めない）。
//!
//! 同じファイルの依頼（2 人が同時に押した・まとめての取り込みと重なった）は、ファイルごとの xact ロックで 1 つずつ
//! 判定する。先に積んだ依頼の outbox の行が見えてから判定するので、2 つ目は「取り込み中」になる。

use authz::AuthContext;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use sqlx::PgConnection;
use storage::audit::{record_on, AuditAction, AuditEntry, Chain, Decision};
use storage::event::{emit_on, WriteEvent, WriteOp};
use utoipa::ToSchema;
use uuid::Uuid;

use super::index_state::{state_on, IndexState};
use super::SearchService;
use crate::error::RagError;
use crate::pipeline::RAG_INGEST_QUEUE;

/// 取り込み直しを頼んだ結果（[`SearchService::request_reindex`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReindexOutcome {
    /// 取り込みの待ち行列に積んだ（`version` はいまの内容のバージョン）。
    Queued { version: i64 },
    /// いまの内容がすでに索引にある（積まない）。
    AlreadyIndexed,
    /// 取り込みを待っている・取り込み中（積まない）。
    InProgress,
    /// 取り込み直しても読めない（積まない）。
    NotIndexable(NotIndexable),
}

/// 取り込み直しても読めない理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotIndexable {
    /// 取り込めない種類。
    Unsupported,
    /// 取り込みの上限より大きい。
    TooLarge { limit_bytes: i64 },
    /// 社内文書の検索に取り込まない場所（AI エージェントの作業用のフォルダー）にある。
    Excluded,
    /// 取り込んだが、本文が無かった。
    Empty,
}

/// 取り込み直しで積まない状態と、頼んだ人への答え（積むなら `None`）。
///
/// 積むのは、取り込みの記録が無いもの（[`IndexState::NotIngested`]）と取り込みに失敗したもの
/// （[`IndexState::Failed`]）だけ。まとめての取り込みは、このうち記録が無いものだけを積む（[`backfill_target`]）。
pub(crate) fn refusal(state: IndexState) -> Option<ReindexOutcome> {
    Some(match state {
        IndexState::NotIngested | IndexState::Failed => return None,
        IndexState::Ready => ReindexOutcome::AlreadyIndexed,
        IndexState::Pending => ReindexOutcome::InProgress,
        IndexState::Unsupported => ReindexOutcome::NotIndexable(NotIndexable::Unsupported),
        IndexState::TooLarge { limit_bytes } => {
            ReindexOutcome::NotIndexable(NotIndexable::TooLarge { limit_bytes })
        }
        IndexState::Excluded => ReindexOutcome::NotIndexable(NotIndexable::Excluded),
        IndexState::Empty => ReindexOutcome::NotIndexable(NotIndexable::Empty),
    })
}

/// まとめての取り込みが積む状態か（取り込みの記録が無いものだけ）。
pub(crate) fn backfill_target(state: IndexState) -> bool {
    state == IndexState::NotIngested
}

/// 取り込み直しの判定を、同じファイルごとに 1 つずつにする xact ロックを取る（`ids` は id の順に並べて渡す。
/// ほかの Tx と同じ順で取り、待ち合いの輪を作らない）。
pub(crate) async fn lock_nodes(conn: &mut PgConnection, ids: &[Uuid]) -> Result<(), RagError> {
    if ids.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "select pg_advisory_xact_lock(hashtextextended('rag_reindex:' || t.id::text, 0)) \
         from unnest($1::uuid[]) with ordinality as t(id, ord) order by t.ord",
    )
    .bind(ids)
    .execute(conn)
    .await?;
    Ok(())
}

/// 取り込み直しの outbox のイベント（いまの内容のバージョン・内容は変わらない）。
pub(crate) fn reindex_event(node_id: Uuid, version: i64, reason: &str) -> WriteEvent {
    WriteEvent {
        node_id,
        version,
        op: WriteOp::Reindex,
        payload: json!({ "kind": "file", "reason": reason }),
    }
}

/// 状態の名前（監査と outbox の payload に残す）。
fn state_name(state: IndexState) -> &'static str {
    match state {
        IndexState::NotIngested => "not_ingested",
        IndexState::Failed => "failed",
        _ => "other",
    }
}

/// まとめての取り込みの状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BackfillState {
    /// ファイルを確かめて、取り込みに回している。
    Running,
    /// すべてのファイルを確かめ終えた（回したファイルの取り込みは、待ち行列で続く）。
    Done,
}

/// まとめての取り込み（取り込みの記録が無いファイル）の進み具合。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct BackfillStatus {
    pub state: BackfillState,
    /// 始めたときのファイルの数（ごみ箱と AI エージェントの作業用のフォルダーのものを除く）。
    pub files_total: i64,
    /// 確かめたファイルの数（始めた後に置いたファイルも数えるので、`files_total` を超えることがある）。
    pub files_scanned: i64,
    /// 取り込みに回したファイルの数（取り込みの記録が無かったもの）。
    pub files_queued: i64,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// 社内文書の検索の取り込みの様子（管理コンソール）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct SearchIngestStatus {
    /// この環境で社内文書の検索が使えるか（使えなければ、ほかの項目は空）。
    pub enabled: bool,
    /// 取り込みを待っている件数（このテナントの取り込みの待ち行列。[`WAITING_COUNT_CAP`] 件で数えるのをやめる）。
    pub waiting: i64,
    /// 待っている件数が [`WAITING_COUNT_CAP`] 件以上で、数えるのをやめたか。
    pub waiting_capped: bool,
    /// いちばん新しいまとめての取り込み（まだ頼んでいなければ `null`）。
    pub backfill: Option<BackfillStatus>,
}

impl SearchIngestStatus {
    /// 社内文書の検索が使えない環境の答え。
    #[must_use]
    pub fn disabled() -> Self {
        SearchIngestStatus {
            enabled: false,
            waiting: 0,
            waiting_capped: false,
            backfill: None,
        }
    }
}

/// 取り込みを待っている件数を数える上限（管理コンソールの表示用・大きなテナントで数えきらない）。
pub const WAITING_COUNT_CAP: i64 = 10_000;

/// テナントの取り込みを待っている内容の作業の数（outbox の未処理と待ち行列・`cap` 件で打ち切る）。
pub(crate) async fn waiting_on(
    conn: &mut PgConnection,
    tenant_id: &str,
    cap: i64,
) -> Result<i64, RagError> {
    let n: i64 = sqlx::query_scalar(
        "select (select count(*) from (select 1 from job_queue q \
                  where q.tenant_id = $1 and q.queue = $2 \
                    and q.payload->>'op' in ('create', 'update', 'restore', 'reindex') \
                  limit $3) a) \
              + (select count(*) from (select 1 from storage_event_outbox o \
                  where o.tenant_id = $1 and o.processed_at is null \
                    and o.op in ('create', 'update', 'restore', 'reindex') \
                  limit $3) b)",
    )
    .bind(tenant_id)
    .bind(RAG_INGEST_QUEUE)
    .bind(cap)
    .fetch_one(conn)
    .await?;
    Ok(n)
}

/// `rag_backfill` の行（進み具合）。
#[derive(sqlx::FromRow)]
struct BackfillRow {
    status: String,
    files_total: i64,
    files_scanned: i64,
    files_queued: i64,
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
}

impl BackfillRow {
    fn into_status(self) -> BackfillStatus {
        BackfillStatus {
            state: if self.status == "running" {
                BackfillState::Running
            } else {
                BackfillState::Done
            },
            files_total: self.files_total,
            files_scanned: self.files_scanned,
            files_queued: self.files_queued,
            started_at: self.started_at,
            finished_at: self.finished_at,
        }
    }
}

/// 始めた行（id つき）。
#[derive(sqlx::FromRow)]
struct IdRow {
    id: i64,
    #[sqlx(flatten)]
    row: BackfillRow,
}

const BACKFILL_COLS: &str =
    "status, files_total, files_scanned, files_queued, started_at, finished_at";

impl SearchService {
    /// `node`（呼び出し側が `StorageService::authorize_reindex` で編集できることを確かめたもの）を、社内文書の検索に
    /// 取り込み直す。積んだときは、許可の監査を同じ Tx で残す。
    pub async fn request_reindex(
        &self,
        ctx: &AuthContext,
        node: &storage::Node,
        trace_id: Option<&str>,
    ) -> Result<ReindexOutcome, RagError> {
        let mut tx = self.pool.begin().await?;
        lock_nodes(&mut tx, &[node.id]).await?;
        let (state, content) = state_on(&mut tx, &self.config, ctx, node).await?;
        if let Some(answer) = refusal(state) {
            return Ok(answer);
        }
        // 積む状態（記録が無い・失敗した）は、材料の行から決めたもの（いまの内容のバージョンがある）。
        let Some(version) = content else {
            return Ok(ReindexOutcome::InProgress);
        };
        let reason = state_name(state);
        emit_on(
            &mut tx,
            ctx,
            reindex_event(node.id, version, reason),
            trace_id,
        )
        .await?;
        record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: AuditAction::FileReindex,
                object_type: "file",
                object_id: &node.id.to_string(),
                decision: Decision::Allow,
                trace_id,
                metadata: json!({ "version": version, "state": reason }),
            },
            Chain::No,
        )
        .await?;
        tx.commit().await?;
        Ok(ReindexOutcome::Queued { version })
    }

    /// 取り込みの記録が無いファイルのまとめての取り込みを始める（テナントの管理者・呼び出し側がルートで確かめる）。
    ///
    /// 同じ組織で動いているものがあれば、新しく始めずにそれを返す（`started = false`）。始めたときは監査を残す。
    /// 積むのは常駐の取り込み（[`crate::pipeline::backfill`]）が少しずつ行う。
    pub async fn start_backfill(
        &self,
        ctx: &AuthContext,
        trace_id: Option<&str>,
    ) -> Result<(BackfillStatus, bool), RagError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("select pg_advisory_xact_lock(hashtextextended('rag_backfill:' || $1, 0))")
            .bind(format!("{}|{}", ctx.tenant_id, ctx.org))
            .execute(&mut *tx)
            .await?;
        let running: Option<BackfillRow> = sqlx::query_as(&format!(
            "select {BACKFILL_COLS} from rag_backfill \
             where tenant_id = $1 and org = $2 and status = 'running'"
        ))
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(row) = running {
            return Ok((row.into_status(), false));
        }
        // 分母（始めたときのファイルの数）。索引 node_file_scan_idx だけで数える。
        let total: i64 = sqlx::query_scalar(
            "select count(*) from node \
             where tenant_id = $1 and org = $2 and kind = 'file' and deleted_at is null \
               and system = false",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .fetch_one(&mut *tx)
        .await?;
        let (id, row): (i64, BackfillRow) = {
            let row: IdRow = sqlx::query_as(&format!(
                "insert into rag_backfill (tenant_id, org, requested_by, status, files_total, trace_id) \
                 values ($1, $2, $3, 'running', $4, $5) returning id, {BACKFILL_COLS}"
            ))
            .bind(&ctx.tenant_id)
            .bind(&ctx.org)
            .bind(&ctx.principal.id)
            .bind(total)
            .bind(trace_id)
            .fetch_one(&mut *tx)
            .await?;
            (row.id, row.row)
        };
        record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: AuditAction::OrgAdminSearchBackfill,
                object_type: "organization",
                object_id: &ctx.org,
                decision: Decision::Allow,
                trace_id,
                metadata: json!({ "backfill_id": id, "files_total": total }),
            },
            Chain::No,
        )
        .await?;
        tx.commit().await?;
        Ok((row.into_status(), true))
    }

    /// 取り込みの様子（待っている件数と、いちばん新しいまとめての取り込みの進み具合）。
    pub async fn ingest_status(&self, ctx: &AuthContext) -> Result<SearchIngestStatus, RagError> {
        let mut conn = self.pool.acquire().await?;
        let latest: Option<BackfillRow> = sqlx::query_as(&format!(
            "select {BACKFILL_COLS} from rag_backfill \
             where tenant_id = $1 and org = $2 order by started_at desc, id desc limit 1"
        ))
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .fetch_optional(&mut *conn)
        .await?;
        let waiting = waiting_on(&mut conn, &ctx.tenant_id, WAITING_COUNT_CAP).await?;
        Ok(SearchIngestStatus {
            enabled: true,
            waiting: waiting.min(WAITING_COUNT_CAP),
            waiting_capped: waiting >= WAITING_COUNT_CAP,
            backfill: latest.map(BackfillRow::into_status),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 積むのは記録が無いものと失敗したものだけ。まとめての取り込みは記録が無いものだけ。
    #[test]
    fn queues_only_files_without_a_usable_record() {
        assert_eq!(refusal(IndexState::NotIngested), None);
        assert_eq!(refusal(IndexState::Failed), None);
        assert_eq!(
            refusal(IndexState::Ready),
            Some(ReindexOutcome::AlreadyIndexed)
        );
        assert_eq!(
            refusal(IndexState::Pending),
            Some(ReindexOutcome::InProgress)
        );
        for (state, why) in [
            (IndexState::Unsupported, NotIndexable::Unsupported),
            (IndexState::Excluded, NotIndexable::Excluded),
            (IndexState::Empty, NotIndexable::Empty),
            (
                IndexState::TooLarge { limit_bytes: 9 },
                NotIndexable::TooLarge { limit_bytes: 9 },
            ),
        ] {
            assert_eq!(refusal(state), Some(ReindexOutcome::NotIndexable(why)));
        }
        assert!(backfill_target(IndexState::NotIngested));
        assert!(!backfill_target(IndexState::Failed));
        assert!(!backfill_target(IndexState::Ready));
    }
}

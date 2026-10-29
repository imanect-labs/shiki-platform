//! 実行履歴の read クエリ（Task 10.14 backend・engine.md §11.3「履歴の正 = run/step/run_event」）。
//!
//! パフォーマンス規約: **必要列のみ SELECT・keyset ページング**。一覧に `ir_snapshot` や
//! step `output` 本体を載せない（output は step 詳細の遅延取得・記録時レダクト済み）。

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::types::Json;
use uuid::Uuid;

use super::outcome::WAITING_FOR;
use super::{map_db, RunStore, RunStoreError};

/// run 一覧の 1 行（一覧に必要な列のみ）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RunListItem {
    pub run_id: Uuid,
    pub status: String,
    /// 待機中（running で、実行中・実行待ちの step が無く、待つブロックで止まっている）なら待っているもの
    /// （`approval`・`event`・`timer`・engine.md §3.1 の導出値・BUG-119）。
    pub waiting_for: Option<String>,
    pub trigger_kind: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// run 一覧のフィルタ（UI の faceted filter に対応）。
#[derive(Debug, Default, Clone)]
pub struct RunListFilter {
    /// 絞り込む status 集合（空 = 全て）。
    pub statuses: Vec<String>,
    /// 絞り込むトリガ種集合（空 = 全て）。
    pub trigger_kinds: Vec<String>,
    /// created_at がこの時刻以降。
    pub from: Option<DateTime<Utc>>,
    /// created_at がこの時刻以前。
    pub to: Option<DateTime<Utc>>,
}

/// step の概要（run 詳細に同梱・output 本体は含めない）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct StepOverview {
    pub step_path: String,
    pub node_id: String,
    pub status: String,
    pub attempt: i32,
    pub taken_ports: Vec<String>,
    /// output の有無（本体は step 詳細で遅延取得）。
    pub has_output: bool,
    pub error: Option<Json<Value>>,
    pub next_retry_at: Option<DateTime<Utc>>,
    pub wake_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
    pub langfuse_trace_id: Option<String>,
    /// 待っている step なら待っているもの（保留中の承認の台帳がある `waiting_event` は `approval`、ほかの
    /// `waiting_event` は `event`、`waiting_timer` は `timer`・run の [`RunDetail::waiting_for`] と同じ決め方）。
    pub waiting_for: Option<String>,
}

/// step が待っているもの（別名 `s` の step に対する式・run の `WAITING_FOR` と同じ決め方・BUG-119）。
const STEP_WAITING_FOR: &str = "CASE \
     WHEN s.status = 'waiting_event' AND EXISTS ( \
         SELECT 1 FROM workflow_approval a WHERE a.tenant_id = s.tenant_id AND a.run_id = s.run_id \
            AND a.step_path = s.step_path AND a.status = 'pending') THEN 'approval' \
     WHEN s.status = 'waiting_event' THEN 'event' \
     WHEN s.status = 'waiting_timer' THEN 'timer' END";

/// run 詳細（履歴 UI のヘッダ＋タイムライン素材）。
#[derive(Debug, Clone)]
pub struct RunDetail {
    pub run_id: Uuid,
    pub status: String,
    /// 待機中なら待っているもの（[`RunListItem::waiting_for`] と同じ）。
    pub waiting_for: Option<String>,
    pub trigger_kind: String,
    pub version: i64,
    pub input: Value,
    pub fail_reason: Option<String>,
    pub trace_id: Option<String>,
    pub cancel_requested: bool,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    /// ステップ（**実行の順**: 実行に入った・終わった順に並べ、まだ実行していないものを末尾に置く・検証 C P3）。
    ///
    /// 同じ時刻（1 つの前進の Tx で書いた・ブロックの完了と通らなかった後続の skip など）の中は、run の
    /// `step_order`（線の前のブロックが先・map の後ろに領域）の順にする（名前の辞書順にしない・BUG-117）。
    /// `step_order` の無い古い run と、順に無いノードは step_path の順。
    pub steps: Vec<StepOverview>,
}

/// step 詳細（入出力プレビュー・遅延取得）。
#[derive(Debug, Clone)]
pub struct StepDetail {
    pub step_path: String,
    pub node_id: String,
    pub status: String,
    pub attempt: i32,
    pub taken_ports: Vec<String>,
    /// 記録時レダクト済みの出力（secret 平文はデータフローに載らない設計）。
    pub output: Value,
    pub error: Value,
    pub langfuse_trace_id: Option<String>,
}

/// run_event の 1 行（タイムライン・SSE リプレイ共用）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RunEventRow {
    pub seq: i64,
    pub kind: String,
    pub payload: Json<Value>,
    pub created_at: DateTime<Utc>,
}

impl RunStore {
    /// workflow の run 一覧（keyset・created_at 降順・フィルタ付き）。
    pub async fn list_runs(
        &self,
        tenant_id: &str,
        workflow_id: Uuid,
        filter: &RunListFilter,
        before: Option<(DateTime<Utc>, Uuid)>,
        limit: i64,
    ) -> Result<Vec<RunListItem>, RunStoreError> {
        let limit = limit.clamp(1, 100);
        let (before_at, before_id) = match before {
            Some((at, id)) => (Some(at), Some(id)),
            None => (None, None),
        };
        let statuses = (!filter.statuses.is_empty()).then_some(&filter.statuses);
        let kinds = (!filter.trigger_kinds.is_empty()).then_some(&filter.trigger_kinds);
        sqlx::query_as(&format!(
            "SELECT r.run_id, r.status, {WAITING_FOR} AS waiting_for, r.trigger_kind, r.version, \
                    r.created_at, r.started_at, r.finished_at \
             FROM workflow_run r \
             WHERE r.tenant_id = $1 AND r.workflow_id = $2 \
               AND ($3::text[] IS NULL OR r.status = ANY($3)) \
               AND ($4::text[] IS NULL OR r.trigger_kind = ANY($4)) \
               AND ($5::timestamptz IS NULL OR r.created_at >= $5) \
               AND ($6::timestamptz IS NULL OR r.created_at <= $6) \
               AND ($7::timestamptz IS NULL OR (r.created_at, r.run_id) < ($7, $8)) \
             ORDER BY r.created_at DESC, r.run_id DESC LIMIT $9"
        ))
        .bind(tenant_id)
        .bind(workflow_id)
        .bind(statuses)
        .bind(kinds)
        .bind(filter.from)
        .bind(filter.to)
        .bind(before_at)
        .bind(before_id)
        .bind(limit)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)
    }

    /// run 詳細＋step 概要（workflow_id 束縛・別 workflow の run_id は None = 404 秘匿）。
    pub async fn run_detail(
        &self,
        tenant_id: &str,
        workflow_id: Uuid,
        run_id: Uuid,
    ) -> Result<Option<RunDetail>, RunStoreError> {
        type RunRow = (
            String,
            Option<String>,
            String,
            i64,
            Json<Value>,
            Option<String>,
            Option<String>,
            bool,
            DateTime<Utc>,
            Option<DateTime<Utc>>,
            Option<DateTime<Utc>>,
        );
        let row: Option<RunRow> = sqlx::query_as(&format!(
            "SELECT r.status, {WAITING_FOR}, r.trigger_kind, r.version, r.input, r.fail_reason, \
                    r.trace_id, r.cancel_requested, r.created_at, r.started_at, r.finished_at \
             FROM workflow_run r \
             WHERE r.tenant_id = $1 AND r.workflow_id = $2 AND r.run_id = $3"
        ))
        .bind(tenant_id)
        .bind(workflow_id)
        .bind(run_id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        let Some((
            status,
            waiting_for,
            trigger_kind,
            version,
            input,
            fail_reason,
            trace_id,
            cancel_requested,
            created_at,
            started_at,
            finished_at,
        )) = row
        else {
            return Ok(None);
        };
        let steps: Vec<StepOverview> = sqlx::query_as(&format!(
            "SELECT s.step_path, s.node_id, s.status, s.attempt, s.taken_ports, \
                    (s.output IS NOT NULL) AS has_output, s.error, s.next_retry_at, s.wake_at, \
                    s.updated_at, s.langfuse_trace_id, {STEP_WAITING_FOR} AS waiting_for \
             FROM step_execution s \
             JOIN workflow_run r ON r.tenant_id = s.tenant_id AND r.run_id = s.run_id \
             WHERE s.tenant_id = $1 AND s.run_id = $2 \
             ORDER BY (s.status IN ('pending', 'ready')), s.updated_at, \
                      array_position(r.step_order, s.node_id), s.step_path"
        ))
        .bind(tenant_id)
        .bind(run_id)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(Some(RunDetail {
            run_id,
            status,
            waiting_for,
            trigger_kind,
            version,
            input: input.0,
            fail_reason,
            trace_id,
            cancel_requested,
            created_at,
            started_at,
            finished_at,
            steps,
        }))
    }

    /// step 詳細（output/error 本体・workflow_id 束縛で存在秘匿）。
    pub async fn step_detail(
        &self,
        tenant_id: &str,
        workflow_id: Uuid,
        run_id: Uuid,
        step_path: &str,
    ) -> Result<Option<StepDetail>, RunStoreError> {
        type StepRow = (
            String,
            String,
            i32,
            Vec<String>,
            Option<Json<Value>>,
            Option<Json<Value>>,
            Option<String>,
        );
        let row: Option<StepRow> = sqlx::query_as(
            "SELECT s.node_id, s.status, s.attempt, s.taken_ports, s.output, s.error, \
                    s.langfuse_trace_id \
             FROM step_execution s \
             JOIN workflow_run r ON r.tenant_id = s.tenant_id AND r.run_id = s.run_id \
             WHERE s.tenant_id = $1 AND r.workflow_id = $2 AND s.run_id = $3 AND s.step_path = $4",
        )
        .bind(tenant_id)
        .bind(workflow_id)
        .bind(run_id)
        .bind(step_path)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        Ok(row.map(
            |(node_id, status, attempt, taken_ports, output, error, langfuse_trace_id)| {
                StepDetail {
                    step_path: step_path.to_string(),
                    node_id,
                    status,
                    attempt,
                    taken_ports,
                    output: output.map_or(Value::Null, |j| j.0),
                    error: error.map_or(Value::Null, |j| j.0),
                    langfuse_trace_id,
                }
            },
        ))
    }

    /// run_event の追記列（after_seq より後・タイムライン/SSE リプレイ共用・workflow_id 束縛）。
    pub async fn list_events(
        &self,
        tenant_id: &str,
        workflow_id: Uuid,
        run_id: Uuid,
        after_seq: i64,
        limit: i64,
    ) -> Result<Vec<RunEventRow>, RunStoreError> {
        let limit = limit.clamp(1, 1000);
        sqlx::query_as(
            "SELECT e.seq, e.kind, e.payload, e.created_at \
             FROM run_event e \
             JOIN workflow_run r ON r.tenant_id = e.tenant_id AND r.run_id = e.run_id \
             WHERE e.tenant_id = $1 AND r.workflow_id = $2 AND e.run_id = $3 AND e.seq > $4 \
             ORDER BY e.seq ASC LIMIT $5",
        )
        .bind(tenant_id)
        .bind(workflow_id)
        .bind(run_id)
        .bind(after_seq)
        .bind(limit)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)
    }
}

//! 実行履歴の read クエリ（Task 10.14 backend・engine.md §11.3「履歴の正 = run/step/run_event」）。
//!
//! パフォーマンス規約: **必要列のみ SELECT・keyset ページング**。一覧に `ir_snapshot` や
//! step `output` 本体を載せない（output は step 詳細の遅延取得・記録時レダクト済み）。

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::types::Json;
use uuid::Uuid;

use super::{map_db, RunStore, RunStoreError};

/// run 一覧の 1 行（一覧に必要な列のみ）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RunListItem {
    pub run_id: Uuid,
    pub status: String,
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
}

/// run 詳細（履歴 UI のヘッダ＋タイムライン素材）。
#[derive(Debug, Clone)]
pub struct RunDetail {
    pub run_id: Uuid,
    pub status: String,
    pub trigger_kind: String,
    pub version: i64,
    pub input: Value,
    pub fail_reason: Option<String>,
    pub trace_id: Option<String>,
    pub cancel_requested: bool,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
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
        sqlx::query_as(
            "SELECT run_id, status, trigger_kind, version, created_at, started_at, finished_at \
             FROM workflow_run \
             WHERE tenant_id = $1 AND workflow_id = $2 \
               AND ($3::text[] IS NULL OR status = ANY($3)) \
               AND ($4::text[] IS NULL OR trigger_kind = ANY($4)) \
               AND ($5::timestamptz IS NULL OR created_at >= $5) \
               AND ($6::timestamptz IS NULL OR created_at <= $6) \
               AND ($7::timestamptz IS NULL OR (created_at, run_id) < ($7, $8)) \
             ORDER BY created_at DESC, run_id DESC LIMIT $9",
        )
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
        let row: Option<RunRow> = sqlx::query_as(
            "SELECT status, trigger_kind, version, input, fail_reason, trace_id, \
                    cancel_requested, created_at, started_at, finished_at \
             FROM workflow_run \
             WHERE tenant_id = $1 AND workflow_id = $2 AND run_id = $3",
        )
        .bind(tenant_id)
        .bind(workflow_id)
        .bind(run_id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        let Some((
            status,
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
        let steps: Vec<StepOverview> = sqlx::query_as(
            "SELECT step_path, node_id, status, attempt, taken_ports, \
                    (output IS NOT NULL) AS has_output, error, next_retry_at, wake_at, \
                    updated_at, langfuse_trace_id \
             FROM step_execution \
             WHERE tenant_id = $1 AND run_id = $2 ORDER BY step_path",
        )
        .bind(tenant_id)
        .bind(run_id)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(Some(RunDetail {
            run_id,
            status,
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

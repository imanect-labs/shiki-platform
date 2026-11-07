//! run/step の永続化と DAG 前進 TX（engine.md §2/§4）。
//!
//! claim/リース/fencing は `crates/durable` のプリミティブに載る。前進は checkpoint（terminal
//! への確定＝実行済みの唯一の真実）と後続 readiness 化を**単一 TX**で行う。

use durable::{Key, KeyValue, RunTableSpec};
use serde_json::Value;
use sqlx::types::Json;
use sqlx::PgPool;
use uuid::Uuid;

use super::graph::RunGraph;
use super::model::{idempotency_key, RunStatus, StepStatus};
use super::readiness::EdgeState;
use crate::vocab::RunEventKind;

mod advance;
mod approval;
mod backoff;
mod claim;
mod gc;
mod history;
mod limits;
mod map_region;
mod ops;
mod outcome;
mod retry_wait;
mod wait;
mod wait_data;

pub use approval::{
    ApprovalWait, DueApprovalCursor, DueApprovalPage, PendingApproval, ResolvedApproval,
};
pub use claim::{LEASE_RECLAIMS_EXHAUSTED_MESSAGE, MAX_LEASE_RECLAIMS};
pub use gc::{GcReport, HistoryGcWorker, WORKFLOW_GC_QUEUE};
pub use history::{RunDetail, RunEventRow, RunListFilter, RunListItem, StepDetail, StepOverview};
pub use limits::PromoteReport;
pub(crate) use ops::cancel_runs_within;
pub use ops::{CancelOutcome, ResumeOutcome};
pub use outcome::{LiveState, OwnOutcome, RunOutcomeReader, MAX_REPLY_BYTES, MAX_REPLY_SECTIONS};
pub use wait::{DueSweepLimits, EventWaitGate, EventWaitSubscriber};
pub use wait_data::DataWaitCandidate;

/// step の durable テーブル記述子（複合キー・engine.md §9.5）。
///
/// `attempt_column: None` は「`durable::claim` の無条件 +1 に乗らない」という意味。workflow は
/// クラッシュ takeover で試行を消費しない（§9.5）ため、+1 と、それを打ち消す -1 を対で持つ独自の
/// 会計が要る。規則は `claim` モジュールのドキュメントが正（#438）。
const STEP_SPEC: RunTableSpec = RunTableSpec {
    event_seq_column: None,
    table: "step_execution",
    status_column: "status",
    fencing_column: "fencing_token",
    lease_column: "lease_expires_at",
    worker_column: "lease_owner",
    attempt_column: None,
    updated_at_column: Some("updated_at"),
    queued_status: "ready",
    running_status: "running",
    resumable_statuses: &[],
};

const STEP_KEY: &[&str] = &["tenant_id", "run_id", "step_path"];

/// run/step 操作のエラー。
#[derive(Debug, thiserror::Error)]
pub enum RunStoreError {
    #[error("対象が見つかりません")]
    NotFound,
    #[error("内部エラー: {0}")]
    Internal(String),
}

#[allow(clippy::needless_pass_by_value)]
fn map_db(e: sqlx::Error) -> RunStoreError {
    RunStoreError::Internal(format!("db: {e}"))
}

/// claim した step（実行に必要な材料）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ClaimedStep {
    pub run_id: Uuid,
    /// run を始めた時刻（`started_at`・まだ無ければ作った時刻。`$from run` の日時の源・D-97）。
    pub run_started_at: chrono::DateTime<chrono::Utc>,
    /// 予定の日時（スケジュールの予定の時刻・手動の実行で指定した日時・親の run から引き継いだ日時。ほかは `None`・
    /// `$from run` の `scheduled_at` の源・GAP-15・[`super::planned`]）。
    pub run_scheduled_at: Option<chrono::DateTime<chrono::Utc>>,
    pub workflow_id: Uuid,
    pub step_path: String,
    pub node_id: String,
    pub tenant_id: String,
    pub org: String,
    pub principal: String,
    /// 実行主体の種別（'user' or 'workflow'）。ワーカーが AuthContext を組む。
    pub principal_kind: String,
    pub attempt: i32,
    pub fencing_token: i64,
    pub idempotency_key: String,
    pub input: Json<Value>,
    /// 手動の実行で試すきっかけの内容（`trigger_payload`・D-93）。無い run（ほとんどすべて）は `None` で、きっかけの
    /// 内容は入力と同じ（schedule / event の run は入力にペイロードを載せる）。入力を DB から 2 回運ばないよう、
    /// 入力での補いはワーカーがする（[`ClaimedStep::trigger_or_input`]）。
    pub trigger: Option<Json<Value>>,
    /// step 固有の入力（map 要素の `each` コンテキスト等）。静的ノードは NULL。
    pub step_input: Option<Json<Value>>,
    /// 開始時にピンした IR（ワーカーがノード params/retry を引く）。
    pub ir_snapshot: Json<Value>,
}

impl ClaimedStep {
    /// きっかけの内容（`$from trigger` の源）。試すきっかけの内容が無ければ入力と同じ（D-93）。
    #[must_use]
    pub fn trigger_or_input(&self) -> Value {
        self.trigger
            .as_ref()
            .map_or_else(|| self.input.0.clone(), |t| t.0.clone())
    }
}

/// resume がリース失効を待つときの上乗せ（秒）。旧 worker のリースが切れた「直後」を狙わず、
/// 時計ずれと最後の heartbeat の遅延を吸収するための余白。
const RESUME_GRACE_MARGIN_SECS: i64 = 5;

/// run の由来（[`RunStore::create_run_from`]・GAP-07）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunOrigin<'a> {
    /// 発火元の outbox イベント（イベントトリガの run・`(trigger_id, trigger_event_id)` で一意）。
    pub trigger_event_id: Option<i64>,
    /// 親の run（workflow.start で起動した子）。
    pub parent_run_id: Option<Uuid>,
    /// 手動の実行で試すきっかけの内容（`$from trigger` の源・D-93）。`None` なら、きっかけの内容は入力と同じ
    /// （今までどおり）。
    pub trigger_payload: Option<&'a Value>,
    /// 予定の日時（実行情報の `scheduled_at` の源・GAP-15）。スケジュールの発火はその回の予定の時刻、手動の実行は
    /// 指定した日時（試す値・GAP-88）を渡す。`None` なら、親の run（`parent_run_id`）の予定の日時を引き継ぎ
    /// （GAP-90）、親も無ければ空のまま（実行情報は始めた時刻を予定の日時として返す）。
    pub scheduled_at: Option<chrono::DateTime<chrono::Utc>>,
    /// やり直しの元の run（「同じ入力でもう一度実行」で作る run だけ・実行履歴が予定の日時の由来を言い分ける・GAP-89）。
    pub rerun_of: Option<Uuid>,
}

/// running の run が多重度（`max_parallel_runs`）に数える条件（SQL の述語・別名 `r`）。
///
/// **待機中の run は数えない**（D-26・H10 (1)）: 実行中・実行待ち（ready / running）の step が無く、
/// wait（waiting_timer / waiting_event）で止まっている run。承認やイベントを何日も待つ run が枠を
/// 占めて、新しい起動が queued のまま進まなくなるのを防ぐ。起床して上限を一時的に超えるのは許す。
pub(crate) const ACTIVE_RUN: &str = "NOT ( \
     NOT EXISTS (SELECT 1 FROM step_execution s \
                 WHERE s.tenant_id = r.tenant_id AND s.run_id = r.run_id \
                   AND s.status IN ('ready', 'running')) \
     AND EXISTS (SELECT 1 FROM step_execution s \
                 WHERE s.tenant_id = r.tenant_id AND s.run_id = r.run_id \
                   AND s.status IN ('waiting_timer', 'waiting_event')))";

/// run/step のデータチョークポイント。
#[derive(Clone)]
pub struct RunStore {
    db: PgPool,
    /// ワーカーが取るリース期間（秒）。`resume_failed` の猶予をここから導出する。
    lease_secs: i64,
}

impl RunStore {
    /// ワーカーと同じ `lease_secs` を渡して組む。
    ///
    /// **`lease_secs` は必須引数にしてある。** `resume_failed` の猶予をここから導出するので、
    /// ビルダで後付けにすると「呼び忘れると既定 30 のまま」という壊し方ができてしまう
    /// （`WorkerConfig` 側だけ 120 に上げた構成で、猶予 35 秒 < リース 120 秒になる・#446）。
    /// 必須にすれば新しい配線を書く人が必ず値を選ぶことになり、自然な選択が
    /// `config.workflow.lease_secs` になる。resume 経路を持たない用途（GC ワーカー等）は
    /// [`crate::DEFAULT_LEASE_SECS`] を渡してよい。
    pub fn new(db: PgPool, lease_secs: i64) -> Self {
        RunStore { db, lease_secs }
    }

    /// run の DB（起動口が自動実行の見送りを同じ DB に記録する・GAP-67）。
    pub(crate) fn db(&self) -> &PgPool {
        &self.db
    }

    /// `resume_failed` が claim 済み step を `ready` へ戻すときに置く猶予（秒）。
    ///
    /// 旧 worker が有効リースを保持したまま別 worker が再 claim すると、checkpoint は fencing が
    /// 無害化するものの**外部副作用が併走する**。リース期間より必ず長くすることでこれを避ける。
    pub(crate) fn resume_grace_secs(&self) -> i64 {
        self.lease_secs.saturating_add(RESUME_GRACE_MARGIN_SECS)
    }

    /// run を作成し本体ノードを一括実体化する（root=ready・他=pending・run.started 追記）。
    ///
    /// `trigger_id` は発火元トリガ（schedule/event の実体化トリガ id・interactive は None）。run 履歴の
    /// リプレイ/監査/キャンセルがどのトリガ由来か辿れるようにする。由来（イベント・親の run）を持つ run は
    /// [`Self::create_run_from`] で作る。
    ///
    /// **run 多重度**（engine.md §8.3・10.5）: `policies.max_parallel_runs` を超える場合は
    /// `on_trigger_overflow` に従い **queue**（status=queued で滞留・scheduler tick が promote）
    /// または **skip**（run を作らず `None`）。判定は per-workflow advisory lock で直列化する
    /// （並行 create の数え漏れによる上限突破を防ぐ）。待機中の run（[`ACTIVE_RUN`]）は数えない。
    #[allow(clippy::too_many_arguments)]
    pub async fn create_run(
        &self,
        tenant_id: &str,
        org: &str,
        workflow_id: Uuid,
        version: i64,
        trigger_kind: &str,
        trigger_id: Option<&str>,
        principal: &str,
        principal_kind: &str,
        input: &Value,
        ir_snapshot: &Value,
        graph: &RunGraph,
    ) -> Result<Option<Uuid>, RunStoreError> {
        self.create_run_from(
            tenant_id,
            org,
            workflow_id,
            version,
            trigger_kind,
            trigger_id,
            principal,
            principal_kind,
            input,
            ir_snapshot,
            graph,
            RunOrigin::default(),
        )
        .await
    }

    /// [`Self::create_run`] の由来つき版（GAP-07）。
    ///
    /// - `origin.trigger_event_id`: 発火元の outbox イベント。`(trigger_id, trigger_event_id)` で一意にし、
    ///   同じイベントで作った run が既にあればそれを返す（再配送・再試行で二重に作らない・PIT-31）。
    /// - `origin.parent_run_id`: 親の run（workflow.start の子）。子は親の鎖 ∪ {親のワークフロー} を
    ///   同じ Tx で引き継ぐ（子の書込が親を起こす循環を見つける・D-26）。`origin.scheduled_at` が無ければ、親の
    ///   予定の日時も同じ読みで引き継ぐ（スケジュールの run から始めた子が、同じ週・月を数える・GAP-90）。
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub async fn create_run_from(
        &self,
        tenant_id: &str,
        org: &str,
        workflow_id: Uuid,
        version: i64,
        trigger_kind: &str,
        trigger_id: Option<&str>,
        principal: &str,
        principal_kind: &str,
        input: &Value,
        ir_snapshot: &Value,
        graph: &RunGraph,
        origin: RunOrigin<'_>,
    ) -> Result<Option<Uuid>, RunStoreError> {
        let policies: crate::ir::Policies = ir_snapshot
            .get("policies")
            .and_then(|p| serde_json::from_value(p.clone()).ok())
            .unwrap_or_default();

        let mut tx = self.db.begin().await.map_err(map_db)?;
        // 同一 workflow の create/promote を直列化（TX 終了で自動解放）。
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1 || ':' || $2, 42))")
            .bind(tenant_id)
            .bind(workflow_id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
        // 同じイベントで作った run があればそれを返す（多重度の判定より先・見送りに化けさせない）。
        if let (Some(tid), Some(eid)) = (trigger_id, origin.trigger_event_id) {
            let existing: Option<Uuid> = sqlx::query_scalar(
                "SELECT run_id FROM workflow_run \
                 WHERE tenant_id = $1 AND trigger_id = $2 AND trigger_event_id = $3",
            )
            .bind(tenant_id)
            .bind(tid)
            .bind(eid)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_db)?;
            if existing.is_some() {
                tx.rollback().await.map_err(map_db)?;
                return Ok(existing);
            }
        }
        let active: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM workflow_run r \
             WHERE r.tenant_id = $1 AND r.workflow_id = $2 \
               AND (r.status = 'queued' OR (r.status = 'running' AND {ACTIVE_RUN}))"
        ))
        .bind(tenant_id)
        .bind(workflow_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db)?;
        let overflow = active >= i64::from(policies.max_parallel_runs.max(1));
        if overflow && policies.on_trigger_overflow == crate::ir::TriggerOverflow::Skip {
            // occurrence/配送記録は呼び出し側で済んでいる（二重発火しない）・run は作らない。
            tx.rollback().await.map_err(map_db)?;
            return Ok(None);
        }
        let queued = overflow; // queue（既定）: 滞留させ tick が promote する。

        // 親の鎖と予定の日時（workflow.start の子）。親が見つからなければ鎖は空で、予定の日時も引き継がない（由来だけ
        // 残す）。
        let (chain, parent_scheduled_at): (Vec<Uuid>, Option<chrono::DateTime<chrono::Utc>>) =
            match origin.parent_run_id {
                Some(parent) => {
                    sqlx::query_as::<_, (Uuid, Vec<Uuid>, Option<chrono::DateTime<chrono::Utc>>)>(
                        "SELECT workflow_id, trigger_chain, scheduled_at FROM workflow_run \
                     WHERE tenant_id = $1 AND run_id = $2",
                    )
                    .bind(tenant_id)
                    .bind(parent)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(map_db)?
                    .map(|(wf, mut chain, at)| {
                        if !chain.contains(&wf) {
                            chain.push(wf);
                        }
                        (chain, at)
                    })
                    .unwrap_or_default()
                }
                None => (Vec::new(), None),
            };
        // 渡された予定の日時（スケジュールの発火・手動の実行で指定した値）が先。無ければ親から引き継ぐ（GAP-90）。
        let scheduled_at = origin.scheduled_at.or(parent_scheduled_at);

        let status = if queued { "queued" } else { "running" };
        let run_id: Uuid = sqlx::query_scalar(
            "INSERT INTO workflow_run \
             (tenant_id, org, workflow_id, version, trigger_kind, trigger_id, principal, \
              principal_kind, input, ir_snapshot, status, started_at, timeout_at, \
              trigger_event_id, trigger_chain, cause_run_id, reply_nodes, trigger_payload, \
              step_order, scheduled_at, rerun_of) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, \
                     CASE WHEN $11 = 'running' THEN now() END, \
                     CASE WHEN $11 = 'running' \
                          THEN now() + ($12 || ' seconds')::interval END, \
                     $13, $14, $15, $16, $17, $18, $19, $20) \
             RETURNING run_id",
        )
        .bind(tenant_id)
        .bind(org)
        .bind(workflow_id)
        .bind(version)
        .bind(trigger_kind)
        .bind(trigger_id)
        .bind(principal)
        .bind(principal_kind)
        .bind(Json(input))
        .bind(Json(ir_snapshot))
        .bind(status)
        .bind(i64::from(policies.run_timeout_sec))
        .bind(origin.trigger_event_id)
        .bind(&chain)
        .bind(origin.parent_run_id)
        // 結果を画面に返すノード（実行順）。読取口が ir_snapshot を開かずに引く（GAP-09）。
        .bind(graph.reply_nodes())
        .bind(origin.trigger_payload.map(Json))
        // 実行の詳細で同じ時刻の step を並べる順（ir_snapshot を開かない・BUG-117）。
        .bind(graph.step_order())
        // 予定の日時（run を作る Tx で書く。発火の記録の run_id は後から書くので、ワーカーが最初の step を取る時点では
        // 発火の記録から読めないことがある・GAP-15）。
        .bind(scheduled_at)
        .bind(origin.rerun_of)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db)?;
        // OTel 相関 id（engine.md §11.1）: run_id の 32-hex を trace_id として採番・記録する
        //（worker 実行時の trace_id 導出と同一規約 → 監査↔OTel↔Langfuse が run 単位で相関する）。
        sqlx::query(
            "UPDATE workflow_run SET trace_id = replace(run_id::text, '-', '') \
             WHERE tenant_id = $1 AND run_id = $2",
        )
        .bind(tenant_id)
        .bind(run_id)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;

        for node_id in graph.root_body_nodes() {
            // 入エッジ 0 本の本体ノードは ready（queued run では pending 留め置き）、それ以外は pending。
            let status = if !queued && graph.is_root_source(node_id) {
                StepStatus::Ready
            } else {
                StepStatus::Pending
            };
            let idem = idempotency_key(tenant_id, run_id, node_id);
            sqlx::query(
                "INSERT INTO step_execution \
                 (tenant_id, run_id, step_path, node_id, status, idempotency_key) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(tenant_id)
            .bind(run_id)
            .bind(node_id)
            .bind(node_id)
            .bind(status.as_str())
            .bind(&idem)
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
        }

        if !queued {
            // run.started は実行開始時のみ（queued は promote 時に追記）。
            append_event(
                &mut tx,
                tenant_id,
                run_id,
                RunEventKind::RunStarted,
                &Value::Null,
            )
            .await?;
        }
        tx.commit().await.map_err(map_db)?;
        Ok(Some(run_id))
    }

    /// step のリースを延長し cancel_requested を返す（heartbeat）。
    pub async fn heartbeat(
        &self,
        tenant_id: &str,
        run_id: Uuid,
        step_path: &str,
        fencing_token: i64,
        lease_secs: i64,
    ) -> Result<Option<bool>, RunStoreError> {
        let kv = [
            KeyValue::Text(tenant_id),
            KeyValue::Uuid(run_id),
            KeyValue::Text(step_path),
        ];
        durable::heartbeat(
            &self.db,
            &STEP_SPEC,
            &Key::new(STEP_KEY, &kv),
            fencing_token,
            lease_secs,
            "(SELECT cancel_requested FROM workflow_run r \
                 WHERE r.tenant_id = step_execution.tenant_id AND r.run_id = step_execution.run_id)",
        )
        .await
        .map_err(map_db)
    }

    /// step の実行結果を checkpoint し DAG を一段前進させる（単一 TX・engine.md §4.1）。
    ///
    /// `taken_ports`/`output`/`error` を書いて terminal 化 → 後続 readiness 化 → run 終了判定。
    /// fencing 不一致（ゾンビ）は false を返し no-op。
    pub async fn checkpoint_and_advance(
        &self,
        claimed: &ClaimedStep,
        result: &super::NodeResult,
        graph: &RunGraph,
        max_attempts: i32,
        on_error: crate::ir::OnError,
    ) -> Result<bool, RunStoreError> {
        advance::checkpoint_and_advance(&self.db, claimed, result, graph, max_attempts, on_error)
            .await
    }

    /// run の状態を取得する。
    pub async fn run_status(
        &self,
        tenant_id: &str,
        run_id: Uuid,
    ) -> Result<Option<RunStatus>, RunStoreError> {
        let s: Option<String> = sqlx::query_scalar(
            "SELECT status FROM workflow_run WHERE tenant_id = $1 AND run_id = $2",
        )
        .bind(tenant_id)
        .bind(run_id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        Ok(s.and_then(|s| RunStatus::parse(&s)))
    }

    /// 指定ワークフロー配下の run 状態のみ取得する（run→workflow を束ねて認可バイパスを防ぐ）。
    ///
    /// `(tenant_id, workflow_id, run_id)` で引き、run が別ワークフローのものなら `None`（存在秘匿）。
    pub async fn run_status_for_workflow(
        &self,
        tenant_id: &str,
        workflow_id: Uuid,
        run_id: Uuid,
    ) -> Result<Option<RunStatus>, RunStoreError> {
        let s: Option<String> = sqlx::query_scalar(
            "SELECT status FROM workflow_run \
             WHERE tenant_id = $1 AND workflow_id = $2 AND run_id = $3",
        )
        .bind(tenant_id)
        .bind(workflow_id)
        .bind(run_id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        Ok(s.and_then(|s| RunStatus::parse(&s)))
    }

    /// run の全 step 状態を取得する（実行履歴・テスト検証）。
    pub async fn step_statuses(
        &self,
        tenant_id: &str,
        run_id: Uuid,
    ) -> Result<Vec<(String, StepStatus)>, RunStoreError> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT step_path, status FROM step_execution \
             WHERE tenant_id = $1 AND run_id = $2 ORDER BY step_path",
        )
        .bind(tenant_id)
        .bind(run_id)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(rows
            .into_iter()
            .filter_map(|(p, s)| StepStatus::parse(&s).map(|st| (p, st)))
            .collect())
    }

    /// 実行対象 step から見える `node_id → output` を集める（`$from nodes.<id>.output` 解決用）。
    ///
    /// ワーカーが次ノード実行前に呼び、[`NodeContext`](super::NodeContext) の `node_outputs` を組む。
    /// `step_path` が map 要素（`<map>[i].<node>`）なら **同一要素スコープの兄弟出力**と、それを囲む要素
    /// スコープ（入れ子の map の外側の要素 `<m1>[i]`）の出力と、静的 body 出力を返す。静的ノードなら静的 body
    /// 出力のみを返す（要素どうしを混同しない）。保存時検証 V5 が繰り返しの中のブロックの祖先に、囲む繰り返しの
    /// 祖先を数えるのと同じ範囲（D-123）。ノード id は IR 内で一意で、囲むスコープの連鎖の中では 1 つの id に
    /// 1 つの step しか無いので、キーは衝突しない。on_error=continue で error ポートを取った failed step も
    /// 含める（`$from nodes.<id>.output.error.*` で参照できるように）。
    ///
    /// 見せないスコープ（ほかの要素）の行は DB から読まない（要素の数だけ step が増えても、読む出力は見える分に
    /// とどめる）。
    pub async fn step_outputs(
        &self,
        tenant_id: &str,
        run_id: Uuid,
        step_path: &str,
    ) -> Result<Vec<(String, Value)>, RunStoreError> {
        let (scope_prefix, _) = map_region::split_step_path(step_path);
        let scopes = map_region::enclosing_scopes(scope_prefix);
        // 要素の step（`[` を含む）のスコープは最後の `.` より前（ノード id は `.` を含まない）。
        let rows: Vec<(String, Option<Json<Value>>)> = sqlx::query_as(
            "SELECT step_path, output FROM step_execution \
             WHERE tenant_id = $1 AND run_id = $2 \
               AND (status = 'succeeded' \
                    OR (status = 'failed' AND 'error' = ANY(taken_ports))) \
               AND (strpos(step_path, '[') = 0 \
                    OR regexp_replace(step_path, '\\.[^.]*$', '') = ANY($3))",
        )
        .bind(tenant_id)
        .bind(run_id)
        .bind(&scopes)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(rows
            .into_iter()
            .filter_map(|(sp, out)| {
                let (sp_scope, sp_node) = map_region::split_step_path(&sp);
                // 静的 body 出力（scope 空）＋自要素とそれを囲む要素のスコープの出力のみを見せる。
                (sp_scope.is_empty() || scopes.contains(&sp_scope))
                    .then(|| (sp_node.to_string(), out.map_or(Value::Null, |j| j.0)))
            })
            .collect())
    }
}

/// 源 step の taken_ports から入エッジ状態を導出する（純関数・readiness の入力を組む）。
pub(crate) fn edge_state(
    from_port: &str,
    source_terminal: bool,
    source_taken_ports: &[String],
) -> EdgeState {
    if !source_terminal {
        EdgeState::Unresolved
    } else if source_taken_ports.iter().any(|p| p == from_port) {
        EdgeState::Live
    } else {
        EdgeState::Dead
    }
}

/// run_event を追記する（tenant 複合キー・(tenant,run,seq) exactly-once）。
pub(crate) async fn append_event(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: &str,
    run_id: Uuid,
    kind: RunEventKind,
    payload: &Value,
) -> Result<(), RunStoreError> {
    // run_event は fencing を持たない追記（fencing 検証は step 側・ここは直接 INSERT）。
    let seq: i64 = sqlx::query_scalar(
        "INSERT INTO run_event (tenant_id, run_id, seq, kind, payload) \
         SELECT $1, $2, coalesce((SELECT max(seq) FROM run_event WHERE tenant_id = $1 AND run_id = $2), 0) + 1, \
                $3, $4 RETURNING seq",
    )
    .bind(tenant_id)
    .bind(run_id)
    .bind(kind.as_str())
    .bind(Json(payload))
    .fetch_one(&mut **tx)
    .await
    .map_err(map_db)?;
    let _ = seq;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::readiness::Readiness;
    use std::collections::HashMap;

    #[test]
    fn edge_state_derivation() {
        assert_eq!(edge_state("out", false, &[]), EdgeState::Unresolved);
        assert_eq!(edge_state("out", true, &["out".into()]), EdgeState::Live);
        assert_eq!(edge_state("out", true, &["true".into()]), EdgeState::Dead);
    }

    #[test]
    fn scoped_readiness_static_linear() {
        use crate::ir::WorkflowIr;
        use serde_json::json;
        let ir = WorkflowIr::from_json(&json!({
            "ir_version": 1, "name": "wf",
            "nodes": [
                { "id": "a", "type": "storage.read", "params": {} },
                { "id": "b", "type": "storage.write", "params": {} }
            ],
            "edges": [{ "from": "a", "to": "b" }]
        }))
        .unwrap();
        let graph = RunGraph::build(&ir);
        // 静的スコープ（scope 空）は step_path==node_id。a 未 terminal → b は NotYet。
        let mut ports: HashMap<String, Vec<String>> = HashMap::new();
        let r = |ports: &HashMap<String, Vec<String>>| {
            map_region::scoped_readiness("", "b", &graph, ports)
        };
        assert_eq!(r(&ports), Readiness::NotYet);
        // a が out を出した → b は Ready。
        ports.insert("a".into(), vec!["out".into()]);
        assert_eq!(r(&ports), Readiness::Ready);
        // a が別ポート（out 以外）→ b は Skip。
        ports.insert("a".into(), vec!["error".into()]);
        assert_eq!(r(&ports), Readiness::Skip);
    }
}

/// run の状態だけを読む口（run を作らない側が使う・会話の図のボタンで起動した run の表示など）。
///
/// `AuthContext` のテナントで必ず絞る。run 単位の ReBAC は見ないので、**呼び出し側が
/// 自分の境界で認可を済ませてから**呼ぶこと（チャットは会話の閲覧者の確認の後、その会話から
/// 起動した run だけを渡す）。返すのは状態（queued〜cancelled）だけで、入力・出力・履歴は
/// 返さない（それらは run の閲覧権限で引く API の役目）。
#[derive(Clone)]
pub struct RunStatusReader {
    db: PgPool,
}

impl RunStatusReader {
    pub fn new(db: PgPool) -> Self {
        RunStatusReader { db }
    }

    /// 複数の run の状態をまとめて引く（無い run は結果に含めない）。
    pub async fn statuses(
        &self,
        ctx: &authz::AuthContext,
        run_ids: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, RunStatus>, RunStoreError> {
        let tenant_id = ctx.tenant_id();
        if run_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let rows: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT run_id, status FROM workflow_run WHERE tenant_id = $1 AND run_id = ANY($2)",
        )
        .bind(tenant_id)
        .bind(run_ids)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(rows
            .into_iter()
            .filter_map(|(id, s)| RunStatus::parse(&s).map(|st| (id, st)))
            .collect())
    }
}

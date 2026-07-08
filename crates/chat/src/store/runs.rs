//! `ChatStore`: 接続非依存生成の generation_run / generation_event 操作（Task 3.11）。
//!
//! 整合性の不変条件（design §4.4.1）:
//! - **Transactional Outbox**: [`post_message`](ChatStore::post_message) は message 保存＋run 行＋
//!   jobq enqueue を**単一 Postgres TX**で行う（202 で run_id を即返す・同期実行しない）。
//! - **Idempotent Consumer ＋ Lease/Fencing**: [`claim_run`](ChatStore::claim_run) は queued か
//!   リース失効 running を claim し `fencing_token` を +1。以降の追記/確定は fencing 一致時のみ
//!   通す（クラッシュ takeover ＋ゾンビ書込拒否）。
//! - **Append-only Event Log**: [`append_stream_event`](ChatStore::append_stream_event) は
//!   `(run_id, seq)` 単調 seq を真実のソースへ追記（exactly-once）し、Redis へ best-effort publish。
//!
//! claim/リース/fencing/追記のプリミティブは `crates/durable`（Task 10.0 で共通化）に
//! 委譲する。SQL 意味は #82 の先行実装と同値であり、キュー・レーン・状態機械（queued/
//! running/done/…の語彙）はチャット所有のまま（engine.md §1.2 の分担表）。

#[allow(clippy::wildcard_imports)]
use super::*;

use authz::{AuthContext, Relation};
use durable::{EventTableSpec, Key, KeyValue, RunTableSpec};
use serde_json::json;
use sqlx::types::Json;
use uuid::Uuid;

use crate::model::{Attachment, ContentBlock, RunStatus, StreamEvent, StreamEventKind};

/// チャット生成ジョブのキュー名（jobq・専用レーン）。
pub const CHAT_GENERATION_QUEUE: &str = "chat_generation";

/// `generation_run` の durable テーブル記述子（migrations/0012_chat.sql の列に対応）。
const RUN_SPEC: RunTableSpec = RunTableSpec {
    table: "generation_run",
    status_column: "status",
    fencing_column: "fencing_token",
    lease_column: "lease_until",
    worker_column: "worker_id",
    attempt_column: Some("attempt"),
    updated_at_column: Some("updated_at"),
    queued_status: "queued",
    running_status: "running",
};

/// `generation_event` の durable テーブル記述子。
const EVENT_SPEC: EventTableSpec = EventTableSpec {
    table: "generation_event",
    seq_column: "seq",
    kind_column: "type",
    payload_column: "payload",
};

/// run 行のキーカラム（chat は run_id 単独キー。workflow は tenant 複合キーで同じ形に乗る）。
const RUN_KEY_COLUMNS: &[&str] = &["run_id"];

/// `post_message` の結果（202 で返す）。
#[derive(Debug, Clone)]
pub struct PostResult {
    pub run_id: Uuid,
    pub user_message_id: Uuid,
    pub assistant_message_id: Uuid,
    pub agent_mode: bool,
}

/// ワーカーが claim した run（生成に必要な材料一式）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ClaimedRun {
    pub run_id: Uuid,
    pub thread_id: Uuid,
    pub message_id: Uuid,
    pub tenant_id: String,
    pub org: String,
    /// 発話ユーザーの subject local id（この本人の権限で生成する）。
    pub actor: String,
    pub agent_mode: bool,
    pub fencing_token: i64,
    pub cancel_requested: bool,
    /// 生成の trace_id（Langfuse/OTel/監査の相関・Task 5.9）。
    pub trace_id: Option<String>,
    /// 自律プロファイル（長ホライズン・フルツール・予算・計画・承認・Task 5.1）。
    pub autonomous: bool,
}

impl ChatStore {
    /// ユーザー発話を投入する（**単一 TX**で user/assistant message 保存＋run 行＋jobq enqueue）。
    ///
    /// editor 権限を要求する（viewer は投稿不可）。同期実行はせず run_id を返す（202・Task 3.5）。
    #[allow(clippy::too_many_arguments)] // ctx＋thread/text/attachments/agent_mode/autonomous/trace は本質的。
    pub async fn post_message(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        text: &str,
        attachments: &[Attachment],
        agent_mode_override: Option<bool>,
        autonomous: bool,
        trace_id: Option<&str>,
    ) -> Result<PostResult, ChatError> {
        self.require_thread(ctx, thread_id, Relation::Editor, "thread.post", trace_id)
            .await?;
        let text = text.trim();
        if text.is_empty() && attachments.is_empty() {
            return Err(ChatError::Invalid("empty message".into()));
        }

        // 実効エージェントモード（メッセージ上書き or スレッド既定）。
        let thread_default: bool =
            sqlx::query_scalar("SELECT agent_mode FROM thread WHERE id = $1 AND tenant_id = $2")
                .bind(thread_id)
                .bind(&ctx.tenant_id)
                .fetch_optional(&self.db)
                .await
                .map_err(map_db)?
                .ok_or(ChatError::NotFound)?;
        // 自律プロファイルはエージェントモードを含意する（ツールループが前提）。
        let agent_mode = agent_mode_override.unwrap_or(thread_default) || autonomous;

        // user メッセージ content: 添付（file_ref）＋text。
        let mut user_content: Vec<ContentBlock> = attachments
            .iter()
            .map(|a| ContentBlock::FileRef {
                node_id: a.node_id.clone(),
                name: a.name.clone(),
            })
            .collect();
        if !text.is_empty() {
            user_content.push(ContentBlock::Text {
                text: text.to_string(),
            });
        }

        let mut tx = self.db.begin().await.map_err(map_db)?;
        let user_id: Uuid = sqlx::query_scalar(
            "INSERT INTO message (thread_id, org, tenant_id, role, content, agent_mode) \
             VALUES ($1, $2, $3, 'user', $4, $5) RETURNING id",
        )
        .bind(thread_id)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .bind(Json(&user_content))
        .bind(agent_mode)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db)?;

        let asst_id: Uuid = sqlx::query_scalar(
            "INSERT INTO message (thread_id, org, tenant_id, role, content, agent_mode, parent_id) \
             VALUES ($1, $2, $3, 'assistant', '[]'::jsonb, $4, $5) RETURNING id",
        )
        .bind(thread_id)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .bind(agent_mode)
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db)?;

        let run_id: Uuid = sqlx::query_scalar(
            "INSERT INTO generation_run (message_id, thread_id, org, tenant_id, actor, agent_mode, status, trace_id, autonomous) \
             VALUES ($1, $2, $3, $4, $5, $6, 'queued', $7, $8) RETURNING run_id",
        )
        .bind(asst_id)
        .bind(thread_id)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .bind(&ctx.principal.id)
        .bind(agent_mode)
        .bind(trace_id)
        .bind(autonomous)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db)?;

        sqlx::query("UPDATE thread SET updated_at = now() WHERE id = $1 AND tenant_id = $2")
            .bind(thread_id)
            .bind(&ctx.tenant_id)
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;

        // outbox: 同一 TX で jobq へ生成ジョブを enqueue（run_id を payload）。
        jobq::enqueue_on(
            &mut tx,
            jobq::NewJob {
                queue: CHAT_GENERATION_QUEUE,
                tenant_id: &ctx.tenant_id,
                payload: &json!({ "run_id": run_id }),
                trace_id,
                max_attempts: 3,
            },
        )
        .await
        .map_err(|e| ChatError::Internal(format!("enqueue: {e}")))?;

        tx.commit().await.map_err(map_db)?;
        Ok(PostResult {
            run_id,
            user_message_id: user_id,
            assistant_message_id: asst_id,
            agent_mode,
        })
    }

    /// run を claim する（queued かリース失効 running を running へ・fencing_token +1）。
    ///
    /// 既に done/cancelled、または有効リースを他ワーカーが保持中なら `None`。
    pub async fn claim_run(
        &self,
        run_id: Uuid,
        worker_id: &str,
        lease_secs: i64,
    ) -> Result<Option<ClaimedRun>, ChatError> {
        let kv = [KeyValue::Uuid(run_id)];
        durable::claim(
            &self.db,
            &RUN_SPEC,
            &Key::new(RUN_KEY_COLUMNS, &kv),
            worker_id,
            lease_secs,
            "run_id, thread_id, message_id, tenant_id, org, actor, agent_mode, \
             fencing_token, cancel_requested, trace_id, autonomous",
        )
        .await
        .map_err(map_db)
    }

    /// リースを延長し、最新の cancel_requested を返す（ハートビート・ゾンビは fencing で弾く）。
    /// 戻り値 `None` = fencing 不一致 or 非 running（リースを失った＝停止すべき）。
    pub async fn heartbeat(
        &self,
        run_id: Uuid,
        fencing_token: i64,
        lease_secs: i64,
    ) -> Result<Option<bool>, ChatError> {
        // durable::heartbeat は status='running' 限定だが、**承認待ち（waiting_approval）中も
        // リースを延長し続ける**必要がある（承認ブロック中にリース失効→誤キャンセルを防ぐ・Task 5.6）。
        // よって chat 専用 SQL で running / waiting_approval の両方を受ける。fencing でゾンビは弾く。
        let cancel: Option<bool> = sqlx::query_scalar(
            "UPDATE generation_run \
                SET lease_until = now() + ($3 || ' seconds')::interval, updated_at = now() \
             WHERE run_id = $1 AND fencing_token = $2 \
                AND status IN ('running', 'waiting_approval') \
             RETURNING cancel_requested",
        )
        .bind(run_id)
        .bind(fencing_token)
        .bind(lease_secs)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        Ok(cancel)
    }

    /// 生成イベントを append-only で追記する（単調 seq・exactly-once）＋Redis publish。
    ///
    /// **fencing 一致（＝現リース保持ワーカー）時のみ**追記する（ゾンビ書込拒否）。fencing 不一致
    /// は seq を返さず `None`（呼び出し側はリース喪失として停止する）。
    pub async fn append_stream_event(
        &self,
        run_id: Uuid,
        fencing_token: i64,
        event: &StreamEventKind,
    ) -> Result<Option<i64>, ChatError> {
        let payload = serde_json::to_value(event)
            .map_err(|e| ChatError::Internal(format!("event serialize: {e}")))?;
        let kv = [KeyValue::Uuid(run_id)];
        let seq = durable::append_event(
            &self.db,
            &RUN_SPEC,
            &EVENT_SPEC,
            &Key::new(RUN_KEY_COLUMNS, &kv),
            event.tag(),
            &payload,
            fencing_token,
        )
        .await
        .map_err(map_db)?;

        if let Some(seq) = seq {
            // 真実のソースへ書けたときのみ publish（DB=truth・Redis=best-effort 起床）。
            let se = StreamEvent {
                seq,
                event: event.clone(),
            };
            if let Ok(s) = serde_json::to_string(&se) {
                self.publish_event(run_id, &s).await;
            }
        }
        Ok(seq)
    }

    /// run を確定する（message.content を projection として書き＋status を端末状態へ）。
    /// fencing 一致時のみ。戻り `false` = fencing 不一致（ゾンビ）で no-op。
    pub async fn finalize_run(
        &self,
        run_id: Uuid,
        fencing_token: i64,
        status: RunStatus,
        content: &[ContentBlock],
        last_error: Option<&str>,
    ) -> Result<bool, ChatError> {
        let mut tx = self.db.begin().await.map_err(map_db)?;
        let kv = [KeyValue::Uuid(run_id)];
        let message_id: Option<Uuid> = durable::fenced_finalize(
            &mut *tx,
            &RUN_SPEC,
            &Key::new(RUN_KEY_COLUMNS, &kv),
            fencing_token,
            status.as_str(),
            &[("last_error", KeyValue::OptText(last_error))],
            "message_id",
        )
        .await
        .map_err(map_db)?;
        let Some(message_id) = message_id else {
            tx.rollback().await.map_err(map_db)?;
            return Ok(false);
        };
        sqlx::query("UPDATE message SET content = $2 WHERE id = $1")
            .bind(message_id)
            .bind(Json(content))
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
        tx.commit().await.map_err(map_db)?;
        Ok(true)
    }

    /// ユーザー明示停止（editor 権限）。cancel_requested を立てる（ページ離脱≠キャンセル）。
    pub async fn request_cancel(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        run_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(), ChatError> {
        self.require_thread(
            ctx,
            thread_id,
            Relation::Editor,
            "thread.run.cancel",
            trace_id,
        )
        .await?;
        let updated = sqlx::query(
            "UPDATE generation_run SET cancel_requested = true, updated_at = now() \
             WHERE run_id = $1 AND thread_id = $2 AND tenant_id = $3",
        )
        .bind(run_id)
        .bind(thread_id)
        .bind(&ctx.tenant_id)
        .execute(&self.db)
        .await
        .map_err(map_db)?;
        if updated.rows_affected() == 0 {
            return Err(ChatError::NotFound);
        }
        Ok(())
    }

    /// このスレッドの最新 run（SSE 購読対象）を返す。
    pub async fn latest_run(
        &self,
        thread_id: Uuid,
        tenant_id: &str,
    ) -> Result<Option<(Uuid, RunStatus)>, ChatError> {
        let row: Option<(Uuid, String)> = sqlx::query_as(
            "SELECT run_id, status FROM generation_run \
             WHERE thread_id = $1 AND tenant_id = $2 ORDER BY created_at DESC, run_id DESC LIMIT 1",
        )
        .bind(thread_id)
        .bind(tenant_id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        Ok(row.and_then(|(id, s)| RunStatus::parse(&s).map(|st| (id, st))))
    }

    /// run の現在状態を引く（SSE の端末判定・crash safety）。
    pub async fn run_status(&self, run_id: Uuid) -> Result<Option<RunStatus>, ChatError> {
        let s: Option<String> =
            sqlx::query_scalar("SELECT status FROM generation_run WHERE run_id = $1")
                .bind(run_id)
                .fetch_optional(&self.db)
                .await
                .map_err(map_db)?;
        Ok(s.and_then(|s| RunStatus::parse(&s)))
    }

    /// `from_seq` より後のイベントを replay する（真実のソース・SSE の補填/復元）。
    pub async fn replay_events(
        &self,
        run_id: Uuid,
        from_seq: i64,
    ) -> Result<Vec<StreamEvent>, ChatError> {
        let kv = [KeyValue::Uuid(run_id)];
        let rows: Vec<(i64, StreamEventKind)> = durable::replay_events(
            &self.db,
            &EVENT_SPEC,
            &Key::new(RUN_KEY_COLUMNS, &kv),
            from_seq,
        )
        .await
        .map_err(map_db)?;
        Ok(rows
            .into_iter()
            .map(|(seq, event)| StreamEvent { seq, event })
            .collect())
    }

    /// run を強制 failed 化し、Error イベントを追記する（fencing 無視）。
    ///
    /// 生成の最終試行失敗（jobq DLQ 行き）と孤児回収 sweeper が使う。既に端末状態なら no-op。
    /// UI に失敗を明示するため Error イベントを 1 件足してから status を failed にする。
    pub async fn force_fail_run(&self, run_id: Uuid, message: &str) -> Result<bool, ChatError> {
        let event = StreamEventKind::Error {
            message: message.to_string(),
        };
        let payload = serde_json::to_value(&event)
            .map_err(|e| ChatError::Internal(format!("event serialize: {e}")))?;
        // 端末でない run にだけ Error を追記（次 seq・fencing 無視の backstop）。
        let kv = [KeyValue::Uuid(run_id)];
        let seq = durable::append_event_unfenced(
            &self.db,
            &RUN_SPEC,
            &EVENT_SPEC,
            &Key::new(RUN_KEY_COLUMNS, &kv),
            event.tag(),
            &payload,
            &["queued", "running"],
        )
        .await
        .map_err(map_db)?;

        let Some(seq) = seq else {
            return Ok(false); // 既に端末状態
        };
        sqlx::query(
            "UPDATE generation_run SET status = 'failed', last_error = $2, \
             lease_until = NULL, updated_at = now() WHERE run_id = $1",
        )
        .bind(run_id)
        .bind(message)
        .execute(&self.db)
        .await
        .map_err(map_db)?;

        let se = StreamEvent { seq, event };
        if let Ok(s) = serde_json::to_string(&se) {
            self.publish_event(run_id, &s).await;
        }
        Ok(true)
    }

    /// 孤児回収 sweeper（backstop）: リースが大きく失効した running run を failed 化する。
    /// 主経路は jobq の再配信＋[`claim_run`] takeover。ここはジョブが失われた場合の保険。
    /// 各孤児へ Error イベントを追記して UI にも失敗を反映する。
    pub async fn reap_orphaned_runs(&self, grace_secs: i64) -> Result<u64, ChatError> {
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT run_id FROM generation_run \
             WHERE status = 'running' AND lease_until IS NOT NULL \
               AND lease_until < now() - ($1 || ' seconds')::interval \
             LIMIT 100",
        )
        .bind(grace_secs)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        let mut n = 0;
        for id in ids {
            if self
                .force_fail_run(id, "orphaned (lease expired)")
                .await
                .unwrap_or(false)
            {
                n += 1;
            }
        }
        Ok(n)
    }
}

#[allow(clippy::needless_pass_by_value)]
fn map_db(e: sqlx::Error) -> ChatError {
    ChatError::Internal(format!("db: {e}"))
}

/// SSE ストリームの端末イベントか（Done / Error / cancelled Status）。
pub(crate) fn is_terminal_event(ev: &StreamEventKind) -> bool {
    matches!(
        ev,
        StreamEventKind::Done { .. }
            | StreamEventKind::Error { .. }
            | StreamEventKind::Status {
                status: RunStatus::Cancelled | RunStatus::Failed | RunStatus::Done
            }
    )
}

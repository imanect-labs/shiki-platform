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
pub(super) const RUN_SPEC: RunTableSpec = RunTableSpec {
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
pub(super) const EVENT_SPEC: EventTableSpec = EventTableSpec {
    table: "generation_event",
    seq_column: "seq",
    kind_column: "type",
    payload_column: "payload",
};

/// run 行のキーカラム（chat は run_id 単独キー。workflow は tenant 複合キーで同じ形に乗る）。
pub(super) const RUN_KEY_COLUMNS: &[&str] = &["run_id"];

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
    /// 適用する skill のバージョンピン（Task 6.7/6.9・thread から post 時にコピー）。
    pub skill_id: Option<Uuid>,
    pub skill_version: Option<i64>,
    /// ミニアプリ経由のセッション（Task 6.10・skill はバンドル権限で読む）。
    pub mini_app_id: Option<Uuid>,
    pub mini_app_version: Option<i64>,
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

        // 実効エージェントモード（メッセージ上書き or スレッド既定）＋skill/ミニアプリのピン
        // （thread 作成時に固定した版を run へコピーする・0027 の autonomous と同パターン）。
        /// thread の生成材料（agent_mode 既定＋skill/mini_app のピン）。
        type ThreadDefaults = (bool, Option<Uuid>, Option<i64>, Option<Uuid>, Option<i64>);
        let thread_row: Option<ThreadDefaults> = sqlx::query_as(
            "SELECT agent_mode, skill_id, skill_version, mini_app_id, mini_app_version \
             FROM thread WHERE id = $1 AND tenant_id = $2",
        )
        .bind(thread_id)
        .bind(&ctx.tenant_id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        let (thread_default, skill_id, skill_version, mini_app_id, mini_app_version) =
            thread_row.ok_or(ChatError::NotFound)?;
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
            "INSERT INTO generation_run (message_id, thread_id, org, tenant_id, actor, agent_mode, status, trace_id, autonomous, \
                                         skill_id, skill_version, mini_app_id, mini_app_version) \
             VALUES ($1, $2, $3, $4, $5, $6, 'queued', $7, $8, $9, $10, $11, $12) RETURNING run_id",
        )
        .bind(asst_id)
        .bind(thread_id)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .bind(&ctx.principal.id)
        .bind(agent_mode)
        .bind(trace_id)
        .bind(autonomous)
        .bind(skill_id)
        .bind(skill_version)
        .bind(mini_app_id)
        .bind(mini_app_version)
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
             fencing_token, cancel_requested, trace_id, autonomous, \
             skill_id, skill_version, mini_app_id, mini_app_version",
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

    /// run を確定する（message.content projection＋端末 status＋終端イベントを**単一 TX**で）。
    /// fencing 一致時のみ。戻り `false` = fencing 不一致（ゾンビ）で no-op。
    ///
    /// `terminal_event`（Done / cancelled Status）は status 更新と**同一 TX**でコミットする。
    /// 分割コミットだと (a) status が先: SSE 側が「端末 status＋残イベント無し」を観測して
    /// 終端イベント未配信のままストリームを閉じる、(b) イベントが先: Done 配信時点で
    /// projection 未確定、のどちらかの race が生じるため（worker_it の flake の根本原因）。
    pub async fn finalize_run(
        &self,
        run_id: Uuid,
        fencing_token: i64,
        status: RunStatus,
        content: &[ContentBlock],
        last_error: Option<&str>,
        terminal_event: Option<&StreamEventKind>,
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
        // 終端イベントも同一 TX で追記し、publish は commit 後（DB=truth・Redis=起床通知）。
        let mut appended: Option<StreamEvent> = None;
        if let Some(event) = terminal_event {
            let payload = serde_json::to_value(event)
                .map_err(|e| ChatError::Internal(format!("event serialize: {e}")))?;
            let seq = durable::append_event(
                &mut *tx,
                &RUN_SPEC,
                &EVENT_SPEC,
                &Key::new(RUN_KEY_COLUMNS, &kv),
                event.tag(),
                &payload,
                fencing_token,
            )
            .await
            .map_err(map_db)?;
            appended = seq.map(|seq| StreamEvent {
                seq,
                event: event.clone(),
            });
        }
        tx.commit().await.map_err(map_db)?;
        if let Some(se) = appended {
            if let Ok(s) = serde_json::to_string(&se) {
                self.publish_event(run_id, &s).await;
            }
        }
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
}

// 強制失敗・孤児回収（fencing 無視の backstop）は [`super::reaper`] に分離。

#[allow(clippy::needless_pass_by_value)]
pub(super) fn map_db(e: sqlx::Error) -> ChatError {
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

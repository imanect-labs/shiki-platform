//! 会話の AI の試運転の台帳（`workflow_trial_run`・migration 0151・T04・D-197）。
//!
//! 試運転の道具（[`crate::workflow_try_tool`]）だけが使う。どの SQL もテナントで絞る。
//!
//! - **前の行を引く鍵**は (会話・話した人・ワークフロー・版・入力の hash)。次の発話は新しい会話の run になる
//!   （`worker/mod.rs` の claim）ので、会話の run で引くと「結果は？」の次の発話で同じものがもう一度走る。
//!   共有した会話で別の人が起動した run には合流しない（話した人で絞る）。
//! - **上限**（5 本）は会話の run（`chat_run_id`）の行の数で数える。
//! - **行は起動の前に入れ**（[`ChatStore::reserve_trial_run`]）、起動に失敗したら消す（[`ChatStore::release_trial_run`]・
//!   `ui_action_run`・0070 と同じ形・PIT-57 ②）。予約は話した本人の会話の run の中だけに効くので、閲覧者が正規の
//!   編集者の枠を取ることはない（PIT-57 ⑥）。
//! - **起動の途中の行**（run_id が null）は、作ってから [`STALE_RESERVATION`] を過ぎたら古い予約として消す
//!   （[`ChatStore::purge_stale_trial_runs`]）。それより新しいものは「起動中」として合流する。

use authz::AuthContext;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::ChatStore;
use crate::ChatError;

/// 起動の途中の行（run_id が null）を古い予約とみなすまでの時間（秒）。起動は OpenFGA の確かめと run の作成だけ
/// なので、これだけ経っても run_id が無ければ、起動の途中でプロセスが落ちたもの。
pub(crate) const STALE_RESERVATION_SECS: i64 = 120;

/// [`STALE_RESERVATION_SECS`] を SQL の interval で書いたもの（文字を組まずに埋め込む）。
const STALE_RESERVATION: &str = "interval '120 seconds'";

/// 予約で seq がぶつかったときに取り直す回数。
const RESERVE_ATTEMPTS: usize = 4;

/// 台帳の鍵（前の行を引く）。
#[derive(Debug, Clone, Copy)]
pub(crate) struct TrialKey<'a> {
    pub thread_id: Uuid,
    pub workflow_id: Uuid,
    pub version: i64,
    pub input_hash: &'a str,
}

/// 台帳の 1 行（道具が使う列だけ）。
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub(crate) struct TrialRow {
    pub chat_run_id: Uuid,
    pub seq: i32,
    /// 起動した run（`None` は起動の途中）。
    pub run_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

fn db(e: &sqlx::Error) -> ChatError {
    ChatError::Internal(format!("db: {e}"))
}

impl ChatStore {
    /// この会話で本人が予約したまま古くなった行（run_id が null のまま [`STALE_RESERVATION_SECS`] を過ぎた）を消す。
    /// 消した行の数を返す。
    pub(crate) async fn purge_stale_trial_runs(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
    ) -> Result<u64, ChatError> {
        let done = sqlx::query(&format!(
            "DELETE FROM workflow_trial_run \
             WHERE tenant_id = $1 AND thread_id = $2 AND invoked_by = $3 AND run_id IS NULL \
               AND created_at < now() - {STALE_RESERVATION}"
        ))
        .bind(ctx.tenant_id())
        .bind(thread_id)
        .bind(&ctx.principal().id)
        .execute(&self.db)
        .await
        .map_err(|e| db(&e))?;
        Ok(done.rows_affected())
    }

    /// 同じ鍵の前の行（新しいものから 1 つ）。
    pub(crate) async fn find_latest_trial_run(
        &self,
        ctx: &AuthContext,
        key: TrialKey<'_>,
    ) -> Result<Option<TrialRow>, ChatError> {
        sqlx::query_as(
            "SELECT chat_run_id, seq, run_id, created_at FROM workflow_trial_run \
             WHERE tenant_id = $1 AND org = $2 AND thread_id = $3 AND invoked_by = $4 \
               AND workflow_id = $5 AND version = $6 AND input_hash = $7 \
             ORDER BY created_at DESC, seq DESC LIMIT 1",
        )
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(key.thread_id)
        .bind(&ctx.principal().id)
        .bind(key.workflow_id)
        .bind(key.version)
        .bind(key.input_hash)
        .fetch_optional(&self.db)
        .await
        .map_err(|e| db(&e))
    }

    /// 会話の run の行の数（上限の数え上げ・起動の途中も数える）。
    pub(crate) async fn count_trial_runs(
        &self,
        ctx: &AuthContext,
        chat_run_id: Uuid,
    ) -> Result<i64, ChatError> {
        sqlx::query_scalar(
            "SELECT count(*) FROM workflow_trial_run WHERE tenant_id = $1 AND chat_run_id = $2",
        )
        .bind(ctx.tenant_id())
        .bind(chat_run_id)
        .fetch_one(&self.db)
        .await
        .map_err(|e| db(&e))
    }

    /// 起動の前に行を入れる（run_id は null）。seq は会話の run の中の最大＋1 で、ぶつかったら取り直す。
    pub(crate) async fn reserve_trial_run(
        &self,
        ctx: &AuthContext,
        chat_run_id: Uuid,
        chat_trace_id: Option<&str>,
        key: TrialKey<'_>,
    ) -> Result<TrialRow, ChatError> {
        for _ in 0..RESERVE_ATTEMPTS {
            let row: Option<TrialRow> = sqlx::query_as(
                "INSERT INTO workflow_trial_run \
                   (tenant_id, org, thread_id, chat_run_id, seq, invoked_by, workflow_id, version, \
                    input_hash, chat_trace_id) \
                 SELECT $1, $2, $3, $4, COALESCE(MAX(seq), 0) + 1, $5, $6, $7, $8, $9 \
                 FROM workflow_trial_run WHERE tenant_id = $1 AND chat_run_id = $4 \
                 ON CONFLICT (tenant_id, chat_run_id, seq) DO NOTHING \
                 RETURNING chat_run_id, seq, run_id, created_at",
            )
            .bind(ctx.tenant_id())
            .bind(ctx.org())
            .bind(key.thread_id)
            .bind(chat_run_id)
            .bind(&ctx.principal().id)
            .bind(key.workflow_id)
            .bind(key.version)
            .bind(key.input_hash)
            .bind(chat_trace_id)
            .fetch_optional(&self.db)
            .await
            .map_err(|e| db(&e))?;
            if let Some(row) = row {
                return Ok(row);
            }
        }
        Err(ChatError::Internal(
            "試運転の台帳の番号を取れませんでした".into(),
        ))
    }

    /// 起動した run を行に書く（起動の途中の行だけ）。
    pub(crate) async fn attach_trial_run(
        &self,
        ctx: &AuthContext,
        row: &TrialRow,
        run_id: Uuid,
    ) -> Result<(), ChatError> {
        sqlx::query(
            "UPDATE workflow_trial_run SET run_id = $4 \
             WHERE tenant_id = $1 AND chat_run_id = $2 AND seq = $3 AND run_id IS NULL",
        )
        .bind(ctx.tenant_id())
        .bind(row.chat_run_id)
        .bind(row.seq)
        .bind(run_id)
        .execute(&self.db)
        .await
        .map_err(|e| db(&e))?;
        Ok(())
    }

    /// 起動しなかった行を消す（起動に失敗した・起動されなかった）。起動した行は消さない。
    pub(crate) async fn release_trial_run(
        &self,
        ctx: &AuthContext,
        row: &TrialRow,
    ) -> Result<(), ChatError> {
        sqlx::query(
            "DELETE FROM workflow_trial_run \
             WHERE tenant_id = $1 AND chat_run_id = $2 AND seq = $3 AND run_id IS NULL",
        )
        .bind(ctx.tenant_id())
        .bind(row.chat_run_id)
        .bind(row.seq)
        .execute(&self.db)
        .await
        .map_err(|e| db(&e))?;
        Ok(())
    }

    /// この行の run について、いまの合計（トークン・マイクロ USD）を報告し、まだ報告していなかった差分を返す。
    ///
    /// 報告済みの値は合計の大きい方に進める（同じ run を何度観測しても、同じ費用を二重に積まない）。行が無ければ
    /// `(0, 0)`。
    pub(crate) async fn report_trial_usage(
        &self,
        ctx: &AuthContext,
        row: &TrialRow,
        total_tokens: i64,
        total_cost_micros: i64,
    ) -> Result<(i64, i64), ChatError> {
        let delta: Option<(i64, i64)> = sqlx::query_as(
            "WITH old AS ( \
                 SELECT reported_tokens, reported_cost_micros FROM workflow_trial_run \
                 WHERE tenant_id = $1 AND chat_run_id = $2 AND seq = $3 FOR UPDATE) \
             UPDATE workflow_trial_run t \
             SET reported_tokens = GREATEST(t.reported_tokens, $4), \
                 reported_cost_micros = GREATEST(t.reported_cost_micros, $5) \
             FROM old WHERE t.tenant_id = $1 AND t.chat_run_id = $2 AND t.seq = $3 \
             RETURNING GREATEST($4 - old.reported_tokens, 0), \
                       GREATEST($5 - old.reported_cost_micros, 0)",
        )
        .bind(ctx.tenant_id())
        .bind(row.chat_run_id)
        .bind(row.seq)
        .bind(total_tokens.max(0))
        .bind(total_cost_micros.max(0))
        .fetch_optional(&self.db)
        .await
        .map_err(|e| db(&e))?;
        Ok(delta.unwrap_or((0, 0)))
    }

    /// 試運転の許可を監査に残す（承認を通して起動した後・`workflow.try_run` の allow・D-197）。
    ///
    /// 対象は拒否の行と同じ（object_type `artifact`・object_id はワークフローの id・relation `editor`）。会話の run・
    /// 会話・版・試運転の run・入力の hash は `metadata` に入れる（呼び出し側）。
    pub(crate) async fn record_try_audit(
        &self,
        ctx: &AuthContext,
        workflow_id: Uuid,
        trace_id: Option<&str>,
        metadata: serde_json::Value,
    ) -> Result<(), ChatError> {
        workflow_engine::run::record_try_run_allowed(
            &self.audit,
            ctx,
            workflow_id,
            trace_id,
            metadata,
        )
        .await
        .map_err(|e| ChatError::Internal(format!("audit: {e}")))
    }
}

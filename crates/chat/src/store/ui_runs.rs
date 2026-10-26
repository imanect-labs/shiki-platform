//! 会話の図のボタンで起動した run の状態と結果（#604・GAP-09・D-28）。
//!
//! - 状態は run の今の状態を優先する（待機中は `waiting`・失敗の後に再開された run は「実行中」に戻る）。
//!   終わった run の状態は記録へ書き戻す（以後 run が消えても状態は残る）。
//! - **結果（workflow.reply の節）と失敗の理由は押した本人にだけ付ける**。共有した会話の他の閲覧者・編集者には
//!   状態だけを見せる（ツール結果の本文を実行主体にしか見せない `message_view` と同じ方針）。エンジンの読取口も
//!   principal = 本人 ∧ interactive で絞る（二重の関門）。
//! - 次の回答の履歴（モデル）には結果を入れない（状態の語だけ・`worker/history.rs`）。押した人の結果が、次に
//!   話しかけた別の編集者の回答に混ざらないように（PIT-56）。
//! - 応答を小さく保つため、結果を付けるのは未確定の run とメッセージごとの最新の終わった 1 件だけ。

use std::collections::{HashMap, HashSet};

use authz::{AuthContext, Relation};
use storage::audit::AuditAction;
use uuid::Uuid;
use workflow_engine::{LiveState, RunOutcomeReader, RunStatus};

use super::yui_actions::RunRow;
use super::ChatStore;
use crate::model::{UiActionRun, UiActionRunStatus};
use crate::ChatError;

/// 結果の節を付ける行。
pub(super) enum ReplyFor {
    /// 未確定の run と、メッセージごとの最新の終わった 1 件（スレッドの表示）。
    OpenAndLatest,
    /// すべて（1 件を開いたとき）。
    All,
}

fn live_status(s: LiveState) -> UiActionRunStatus {
    match s.status {
        RunStatus::Queued => UiActionRunStatus::Queued,
        RunStatus::Running if s.waiting_for.is_some() => UiActionRunStatus::Waiting,
        RunStatus::Running => UiActionRunStatus::Running,
        RunStatus::Succeeded => UiActionRunStatus::Succeeded,
        RunStatus::Failed => UiActionRunStatus::Failed,
        RunStatus::Cancelled => UiActionRunStatus::Cancelled,
    }
}

impl ChatStore {
    /// 本人が会話の図のボタンで起動した 1 件（結果の節を必ず付ける・古い行を開いたとき）。
    ///
    /// 会話の閲覧者で、その run を押した本人のときだけ。他人が押した run・別の会話の run は見つからない扱い。
    pub async fn my_ui_run(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        run_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<UiActionRun, ChatError> {
        self.require_thread(
            ctx,
            thread_id,
            Relation::Viewer,
            AuditAction::ThreadMessages,
            trace_id,
        )
        .await?;
        let rows: Vec<RunRow> = sqlx::query_as(
            "SELECT message_id, action_id, label, workflow_name, run_id, status, summary, \
                    invoked_by, invoked_at, idempotency_key \
             FROM ui_action_run \
             WHERE tenant_id = $1 AND thread_id = $2 AND run_id = $3 AND invoked_by = $4 \
               AND org = $5 LIMIT 1",
        )
        .bind(&ctx.tenant_id)
        .bind(thread_id)
        .bind(run_id)
        .bind(&ctx.principal.id)
        .bind(&ctx.org)
        .fetch_all(&self.db)
        .await
        .map_err(|e| ChatError::Internal(format!("db: {e}")))?;
        self.ui_runs_of(ctx, thread_id, rows, &ReplyFor::All)
            .await
            .pop()
            .map(|(_, r)| r)
            .ok_or(ChatError::NotFound)
    }

    /// 起動の記録の行を、状態・結果つきの [`UiActionRun`] にする（行の順はそのまま・メッセージ id を添える）。
    pub(super) async fn ui_runs_of(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        rows: Vec<RunRow>,
        reply_for: &ReplyFor,
    ) -> Vec<(Uuid, UiActionRun)> {
        let reader = RunOutcomeReader::new(self.db.clone());
        let ids: Vec<Uuid> = rows.iter().filter_map(|r| r.4).collect();
        let live = reader.live_states(ctx, &ids).await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "図のボタンで起動した run の状態を読めない");
            HashMap::new()
        });
        let statuses: Vec<UiActionRunStatus> = rows
            .iter()
            .map(
                |r| match (r.4.and_then(|id| live.get(&id)), r.5.as_deref()) {
                    (Some(s), _) => live_status(*s),
                    (None, Some(s)) => {
                        UiActionRunStatus::parse(s).unwrap_or(UiActionRunStatus::Queued)
                    }
                    (None, None) if r.4.is_some() => UiActionRunStatus::Queued,
                    (None, None) => UiActionRunStatus::Starting,
                },
            )
            .collect();
        // 結果を読むのは押した本人の run だけ（他人の run は読取口に渡さない・読取口も本人で絞る）。
        let mine: Vec<Uuid> = rows
            .iter()
            .filter(|r| r.7 == ctx.principal.id)
            .filter_map(|r| r.4)
            .collect();
        let with_reply: Vec<Uuid> = match reply_for {
            ReplyFor::All => mine.clone(),
            ReplyFor::OpenAndLatest => {
                let mut latest_done: HashMap<Uuid, Uuid> = HashMap::new();
                for (r, s) in rows.iter().zip(&statuses) {
                    if let (Some(id), true) = (r.4, s.is_terminal() && r.7 == ctx.principal.id) {
                        // 行は古い順なので、後に来たものが最新。
                        latest_done.insert(r.0, id);
                    }
                }
                let latest: HashSet<Uuid> = latest_done.into_values().collect();
                rows.iter()
                    .zip(&statuses)
                    .filter(|(r, s)| {
                        r.7 == ctx.principal.id
                            && (!s.is_terminal() || r.4.is_some_and(|id| latest.contains(&id)))
                    })
                    .filter_map(|(r, _)| r.4)
                    .collect()
            }
        };
        let mut own = if mine.is_empty() {
            HashMap::new()
        } else {
            reader
                .own_outcomes(ctx, &mine, &with_reply)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "図のボタンで起動した run の結果を読めない");
                    HashMap::new()
                })
        };
        self.write_back_ui_runs(ctx, thread_id, &rows, &live).await;
        rows.into_iter()
            .zip(statuses)
            .map(
                |(
                    (m, action_id, label, workflow_name, run_id, row_status, summary, by, at, _),
                    status,
                )| {
                    let outcome = if by == ctx.principal.id {
                        run_id.and_then(|id| own.remove(&id)).unwrap_or_default()
                    } else {
                        workflow_engine::OwnOutcome::default()
                    };
                    let live_state = run_id.and_then(|id| live.get(&id));
                    let run = UiActionRun {
                        action_id,
                        label,
                        workflow_name,
                        run_id,
                        status,
                        values: summary.0,
                        invoked_by: by,
                        invoked_at: at,
                        waiting_for: live_state.and_then(|s| s.waiting_for),
                        failure: (status == UiActionRunStatus::Failed)
                            .then_some(outcome.failure)
                            .flatten(),
                        reply: outcome.reply.map(gui::displayable_reply),
                        has_reply: outcome.has_reply,
                        result_expired: run_id.is_some()
                            && live_state.is_none()
                            && row_status.is_some(),
                    };
                    (m, run)
                },
            )
            .collect()
    }

    /// 終わった run の状態を記録へ書き戻す（best-effort・失敗しても次の表示でまた読む）。
    async fn write_back_ui_runs(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        rows: &[RunRow],
        live: &HashMap<Uuid, LiveState>,
    ) {
        let open: HashSet<Uuid> = rows
            .iter()
            .filter(|r| r.5.is_none())
            .filter_map(|r| r.4)
            .collect();
        let done: Vec<(Uuid, &'static str)> = live
            .iter()
            .filter(|(id, s)| open.contains(id) && s.status.is_terminal())
            .map(|(id, s)| (*id, s.status.as_str()))
            .collect();
        if done.is_empty() {
            return;
        }
        let ids: Vec<Uuid> = done.iter().map(|d| d.0).collect();
        let sts: Vec<&str> = done.iter().map(|d| d.1).collect();
        if let Err(e) = sqlx::query(
            "UPDATE ui_action_run r SET status = x.status, finished_at = now() \
             FROM unnest($3::uuid[], $4::text[]) AS x(run_id, status) \
             WHERE r.tenant_id = $1 AND r.thread_id = $2 AND r.run_id = x.run_id \
               AND r.status IS NULL",
        )
        .bind(&ctx.tenant_id)
        .bind(thread_id)
        .bind(&ids)
        .bind(&sts)
        .execute(&self.db)
        .await
        {
            tracing::warn!(error = %e, "図のボタンで起動した run の状態の書き戻しに失敗");
        }
    }
}

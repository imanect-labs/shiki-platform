//! ミニアプリの画面のボタンで起動した記録の読取（本人の分だけ・#604・GAP-09）。
//!
//! 起動の記録（`miniapp_action_run`）に、run の今の状態（待機中を含む）・ワークフローが画面に返した結果
//! （`workflow.reply` の節）・失敗の理由を添えて返す。
//!
//! - **二重の関門**: ここでアプリの viewer ∧ `invoked_by = 本人` ∧ org を確かめ、エンジンの読取口
//!   （[`workflow_engine::RunOutcomeReader::own_outcomes`]）が principal = 本人 ∧ principal_kind = user ∧ org ∧
//!   interactive で絞る。ここが誤った run_id を渡しても、他人の結果は返らない。
//! - ワークフローの viewer は求めない。interactive の run の値は押した本人の権限で読んだもので、作成者に由来
//!   するのは固定の文言だけ（アプリを共有した時点で利用者に見せるつもりのもの）。
//! - 結果の Markdown の中の図・表のフェンスは表示専用（` ```text `）に書き換える（[`displayable_reply`]）。
//! - 応答を小さく保つため、一覧で結果を付けるのは未確定の run と最新の終わった 1 件だけ（他は `has_reply`）。

use std::collections::{HashMap, HashSet};

use authz::AuthContext;
use chrono::{DateTime, Utc};
use sqlx::types::Json;
use uuid::Uuid;
use workflow_engine::{LiveState, OwnOutcome, ReplyBody, RunOutcomeReader, RunReply, RunStatus};

use crate::miniapp_actions::{MiniAppRun, MiniAppRunStatus, MiniAppRunValue, MAX_RUNS};
use crate::miniapp_store::MiniAppStore;
use crate::store::GuiError;

type RunRow = (
    String,
    i64,
    String,
    String,
    Option<Uuid>,
    Option<String>,
    Json<Vec<MiniAppRunValue>>,
    DateTime<Utc>,
);

/// 結果の節を付ける行。
enum ReplyFor {
    /// 未確定の run と、最新の終わった 1 件（一覧）。
    OpenAndLatest,
    /// すべて（1 件を開いたとき）。
    All,
}

/// 画面に出す前の結果（Markdown の中の操作を持つ図・表のフェンスを表示専用にする・D-28）。
///
/// 結果には LLM の出力や外部のデータが入るので、注入された「承認する -> workflow …」のボタンや束ねていない
/// 表を、画面の操作として見せない。会話の図のボタンの結果（chat）も同じ関数を通す。
#[must_use]
pub fn displayable_reply(mut reply: RunReply) -> RunReply {
    for s in &mut reply.sections {
        if let ReplyBody::Markdown { text } = &mut s.body {
            *text = yui::neutralize_fences(text);
        }
    }
    reply
}

/// run の今の状態を記録の状態にする（running で wait に止まっているものは待機中）。
#[must_use]
fn live_status(s: LiveState) -> MiniAppRunStatus {
    match s.status {
        RunStatus::Queued => MiniAppRunStatus::Queued,
        RunStatus::Running if s.waiting_for.is_some() => MiniAppRunStatus::Waiting,
        RunStatus::Running => MiniAppRunStatus::Running,
        RunStatus::Succeeded => MiniAppRunStatus::Succeeded,
        RunStatus::Failed => MiniAppRunStatus::Failed,
        RunStatus::Cancelled => MiniAppRunStatus::Cancelled,
    }
}

fn is_open(s: MiniAppRunStatus) -> bool {
    matches!(
        s,
        MiniAppRunStatus::Starting
            | MiniAppRunStatus::Queued
            | MiniAppRunStatus::Running
            | MiniAppRunStatus::Waiting
    )
}

impl MiniAppStore {
    /// 本人が画面のボタンで起動した記録（新しい [`MAX_RUNS`] 件を古い順に）。
    ///
    /// 本体の viewer を要求する（共有を外された人は自分の記録も読めない）。状態は run を読み（再開された run
    /// も「実行中」に戻る）、終わっていたら書き戻す。
    pub async fn my_runs(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Vec<MiniAppRun>, GuiError> {
        self.require_app(ctx, app_id, trace_id).await?;
        let rows: Vec<RunRow> = sqlx::query_as(
            "SELECT action_id, app_version, label, workflow_name, run_id, status, summary, invoked_at \
             FROM miniapp_action_run \
             WHERE tenant_id = $1 AND app_id = $2 AND invoked_by = $3 AND org = $4 \
             ORDER BY invoked_at DESC, idempotency_key DESC LIMIT $5",
        )
        .bind(ctx.tenant_id())
        .bind(app_id)
        .bind(&ctx.principal().id)
        .bind(ctx.org())
        .bind(MAX_RUNS)
        .fetch_all(&self.db)
        .await
        .map_err(|e| GuiError::Internal(format!("db: {e}")))?;
        let mut runs = self
            .with_outcomes(ctx, app_id, rows, &ReplyFor::OpenAndLatest)
            .await;
        runs.reverse();
        Ok(runs)
    }

    /// 本人が起動した 1 件（結果の節を必ず付ける・古い行を開いたとき）。
    ///
    /// 一覧と同じ関門を通る。本人の記録に無い run（他人の run・別のアプリの run）は見つからない扱い。
    pub async fn my_run(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
        run_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<MiniAppRun, GuiError> {
        self.require_app(ctx, app_id, trace_id).await?;
        let rows: Vec<RunRow> = sqlx::query_as(
            "SELECT action_id, app_version, label, workflow_name, run_id, status, summary, invoked_at \
             FROM miniapp_action_run \
             WHERE tenant_id = $1 AND app_id = $2 AND invoked_by = $3 AND org = $4 AND run_id = $5 \
             LIMIT 1",
        )
        .bind(ctx.tenant_id())
        .bind(app_id)
        .bind(&ctx.principal().id)
        .bind(ctx.org())
        .bind(run_id)
        .fetch_all(&self.db)
        .await
        .map_err(|e| GuiError::Internal(format!("db: {e}")))?;
        self.with_outcomes(ctx, app_id, rows, &ReplyFor::All)
            .await
            .pop()
            .ok_or(GuiError::Artifact(artifact::ArtifactError::NotFound))
    }

    /// 本体（mini_app）の viewer を確かめる。
    async fn require_app(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(), GuiError> {
        let meta = self.artifacts.get(ctx, app_id, trace_id).await?;
        if meta.kind != artifact::ArtifactKind::MiniApp {
            return Err(GuiError::Artifact(artifact::ArtifactError::NotFound));
        }
        Ok(())
    }

    /// 記録の行に、run の今の状態・結果・失敗の理由を添える（行の順はそのまま）。
    async fn with_outcomes(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
        rows: Vec<RunRow>,
        reply_for: &ReplyFor,
    ) -> Vec<MiniAppRun> {
        let reader = RunOutcomeReader::new(self.db.clone());
        let ids: Vec<Uuid> = rows.iter().filter_map(|r| r.4).collect();
        let live = reader.live_states(ctx, &ids).await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "画面のボタンで起動した run の状態を読めない");
            HashMap::new()
        });
        // 状態は生きている run を優先し、写した状態はその後に使う（失敗の後に再開された run を戻す）。
        let statuses: Vec<MiniAppRunStatus> = rows
            .iter()
            .map(
                |r| match (r.4.and_then(|id| live.get(&id)), r.5.as_deref()) {
                    (Some(s), _) => live_status(*s),
                    (None, Some(s)) => {
                        MiniAppRunStatus::parse(s).unwrap_or(MiniAppRunStatus::Queued)
                    }
                    (None, None) if r.4.is_some() => MiniAppRunStatus::Queued,
                    (None, None) => MiniAppRunStatus::Starting,
                },
            )
            .collect();
        let with_reply: Vec<Uuid> = match reply_for {
            ReplyFor::All => ids.clone(),
            ReplyFor::OpenAndLatest => {
                let latest_done = rows
                    .iter()
                    .zip(&statuses)
                    .find(|(r, s)| r.4.is_some() && !is_open(**s))
                    .and_then(|(r, _)| r.4);
                rows.iter()
                    .zip(&statuses)
                    .filter(|(r, s)| is_open(**s) || r.4 == latest_done)
                    .filter_map(|(r, _)| r.4)
                    .collect()
            }
        };
        let mut own = reader
            .own_outcomes(ctx, &ids, &with_reply)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "画面のボタンで起動した run の結果を読めない");
                HashMap::new()
            });
        self.write_back(ctx, app_id, &rows, &live).await;
        rows.into_iter()
            .zip(statuses)
            .map(
                |(
                    (action_id, app_version, label, workflow_name, run_id, row_status, summary, at),
                    status,
                )| {
                    let outcome: OwnOutcome =
                        run_id.and_then(|id| own.remove(&id)).unwrap_or_default();
                    let live_state = run_id.and_then(|id| live.get(&id));
                    MiniAppRun {
                        action_id,
                        app_version,
                        label,
                        workflow_name,
                        run_id,
                        status,
                        values: summary.0,
                        invoked_at: at,
                        waiting_for: live_state.and_then(|s| s.waiting_for),
                        failure: (status == MiniAppRunStatus::Failed)
                            .then_some(outcome.failure)
                            .flatten(),
                        reply: outcome.reply.map(displayable_reply),
                        has_reply: outcome.has_reply,
                        reply_tone: outcome.reply_tone,
                        // run が消えた（保持期間の GC）なら、写した状態だけが残っている。
                        result_expired: run_id.is_some()
                            && live_state.is_none()
                            && row_status.is_some(),
                    }
                },
            )
            .collect()
    }

    /// 終わった run の状態を記録へ書き戻す（best-effort・失敗しても次の表示でまた読む）。
    async fn write_back(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
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
            "UPDATE miniapp_action_run r SET status = x.status, finished_at = now() \
             FROM unnest($3::uuid[], $4::text[]) AS x(run_id, status) \
             WHERE r.tenant_id = $1 AND r.app_id = $2 AND r.run_id = x.run_id \
               AND r.status IS NULL",
        )
        .bind(ctx.tenant_id())
        .bind(app_id)
        .bind(&ids)
        .bind(&sts)
        .execute(&self.db)
        .await
        {
            tracing::warn!(error = %e, "画面のボタンで起動した run の状態の書き戻しに失敗");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use workflow_engine::{ReplySection, ReplyTone, WaitingFor};

    #[test]
    fn markdown_results_cannot_carry_buttons_or_grids() {
        let reply = RunReply {
            sections: vec![
                ReplySection {
                    label: None,
                    tone: ReplyTone::Neutral,
                    truncated: false,
                    body: ReplyBody::Markdown {
                        text: "結果\n```ui\napp x\nbutton 承認 -> workflow y\n```\n```data_grid\na\n```"
                            .into(),
                    },
                },
                ReplySection {
                    label: None,
                    tone: ReplyTone::Neutral,
                    truncated: false,
                    body: ReplyBody::Text {
                        text: "```ui\nそのまま見せる\n```".into(),
                    },
                },
            ],
            omitted: 0,
        };
        let out = displayable_reply(reply);
        let ReplyBody::Markdown { text } = &out.sections[0].body else {
            panic!("markdown");
        };
        assert!(
            !text.contains("```ui") && !text.contains("```data_grid"),
            "{text}"
        );
        assert!(yui::extract_action_blocks(text).is_empty());
        // 文章（text）は文字のまま描くので変えない。
        assert_eq!(
            out.sections[1].body,
            ReplyBody::Text {
                text: "```ui\nそのまま見せる\n```".into()
            }
        );
    }

    #[test]
    fn waiting_runs_are_not_shown_as_running() {
        let s = |status, waiting_for| {
            live_status(LiveState {
                status,
                waiting_for,
            })
        };
        assert_eq!(s(RunStatus::Running, None), MiniAppRunStatus::Running);
        assert_eq!(
            s(RunStatus::Running, Some(WaitingFor::Approval)),
            MiniAppRunStatus::Waiting
        );
        assert_eq!(s(RunStatus::Queued, None), MiniAppRunStatus::Queued);
    }
}

//! ミニアプリの画面のボタンの実行と、起動の記録（#604）。
//!
//! 画面は Yui の ```ui ブロックを含む Markdown。押されるたびに**保存済みの版の画面から**
//! ボタンを読み直し、値を部品の定義で検証してから、束ねたワークフローのピン版を
//! **押した本人の権限**で起動する（IR はバンドル権限で読む・[`WorkflowStarter::start_pinned_via_bundle`]）。
//! 画面から送れるのは id と値と冪等キーだけで、実行先は送れない。
//!
//! 同じ冪等キーは 1 回だけ起動する（`miniapp_action_run`）。起動の記録は押した本人だけが読み、
//! 終わった状態は run の終端でトリガが写す（表示のときも run を読んで書き戻す）。
//!
//! [`WorkflowStarter::start_pinned_via_bundle`]: crate::dispatch::WorkflowStarter::start_pinned_via_bundle

use std::collections::HashMap;

use authz::AuthContext;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sqlx::types::Json;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::action::{ActionBinding, WorkflowBinding, WorkflowPin};
use crate::dispatch::{ActionDispatcher, ActionError, ActionSource};
use crate::miniapp_store::MiniAppStore;
use crate::store::GuiError;

/// 本人に返す起動の記録の件数（新しいものから）。
pub const MAX_RUNS: i64 = 20;

/// 画面のボタンの実行リクエスト。
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct MiniAppActionRequest {
    /// 実行対象のミニアプリ版（**必須**・解決済み画面の版と一致させる。current 暗黙参照だと
    /// 画面を開いたまま新版が保存されたとき、別の実行先へ同じ id で到達し得る）。
    pub version: i64,
    /// ボタンの id（`{ブロックの識別子}:{a1}`）。
    pub action_id: String,
    /// 部品の値（key は部品の `as` 名）。
    #[serde(default)]
    #[schema(value_type = Object)]
    pub params: Map<String, Value>,
    /// 送信ごとの冪等キー（英数字・`-`・`_` の 8〜64 文字）。
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

/// 送った値の 1 項目（部品の表示名と表示用の値）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MiniAppRunValue {
    pub label: String,
    pub value: String,
}

/// 起動の状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MiniAppRunStatus {
    /// 起動の途中（run はまだ無い）。
    Starting,
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    /// 同時実行の上限で起動を見送った。
    Skipped,
}

impl MiniAppRunStatus {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "skipped" => Self::Skipped,
            _ => return None,
        })
    }
}

/// 画面のボタンで起動したワークフロー 1 件（押した本人の分だけ返す）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MiniAppRun {
    /// ボタンの id（`{ブロックの識別子}:{a1}`）。
    pub action_id: String,
    /// 押した版。
    pub app_version: i64,
    /// ボタンの文言（押した時点）。
    pub label: String,
    pub workflow_name: String,
    #[serde(default)]
    pub run_id: Option<Uuid>,
    pub status: MiniAppRunStatus,
    #[serde(default)]
    pub values: Vec<MiniAppRunValue>,
    pub invoked_at: DateTime<Utc>,
}

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

/// 本人の 1 送信を指す鍵（アプリ・ボタン・冪等キー）。
struct RunKey<'a> {
    app_id: Uuid,
    action_id: &'a str,
    idempotency_key: &'a str,
}

fn map_gui_err(e: GuiError) -> ActionError {
    match e {
        GuiError::Artifact(artifact::ArtifactError::NotFound) => ActionError::NotFound,
        GuiError::Artifact(artifact::ArtifactError::Forbidden) => ActionError::Forbidden,
        GuiError::Validation(errors) => ActionError::Invalid(
            errors
                .into_iter()
                .map(|e| e.message)
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        other => ActionError::Internal(other.to_string()),
    }
}

fn map_db(e: &sqlx::Error) -> ActionError {
    ActionError::Internal(format!("db: {e}"))
}

impl MiniAppStore {
    /// 画面のボタンを実行する（本体 viewer・画面から読み直し・値の検証・冪等キー・記録）。
    pub async fn invoke_yui_action(
        &self,
        ctx: &AuthContext,
        dispatcher: &ActionDispatcher,
        app_id: Uuid,
        req: MiniAppActionRequest,
        trace_id: Option<&str>,
    ) -> Result<Value, ActionError> {
        // 1. 本体の viewer（解決が認可する）。部品は読まない（ワークフローは起動時にバンドル権限で読む）。
        let resolved = self
            .resolve(ctx, app_id, Some(req.version), trace_id)
            .await
            .map_err(map_gui_err)?;
        let source = ActionSource::MiniApp {
            artifact_id: resolved.id,
            version: resolved.version,
        };
        let key = req.action_id.as_str();
        // 2. 保存済みの版の画面からボタンを読み直す（画面の図は信用しない）。
        let Some((block, action)) = yui::find_action([resolved.body.screen.as_str()], key) else {
            dispatcher
                .deny(ctx, &source, key, "undeclared_action", trace_id)
                .await;
            return Err(ActionError::NotFound);
        };
        let yui::ActionTarget::Workflow { name } = &action.target else {
            // 保存時に拒んでいる（会話に送るボタンはミニアプリに無い）。
            dispatcher
                .deny(ctx, &source, key, "chat_target", trace_id)
                .await;
            return Err(ActionError::NotFound);
        };
        let Some(pin) = resolved.body.workflows.iter().find(|w| &w.alias == name) else {
            dispatcher
                .deny(ctx, &source, key, "unbundled_workflow", trace_id)
                .await;
            return Err(ActionError::NotFound);
        };
        let check = yui::validate_action_values(&block.figure, &action.id, &req.params, "ja");
        if !check.ok {
            dispatcher
                .deny(ctx, &source, key, "invalid_values", trace_id)
                .await;
            return Err(ActionError::Invalid(yui::error_text(
                &block.figure,
                &check.errors,
            )));
        }
        let Some(idem) = req
            .idempotency_key
            .as_deref()
            .filter(|k| yui::valid_idempotency_key(k))
        else {
            dispatcher
                .deny(ctx, &source, key, "missing_idempotency_key", trace_id)
                .await;
            return Err(ActionError::Invalid(
                "idempotency_key（英数字・-・_ の 8〜64 文字）が必要です".into(),
            ));
        };
        let run_key = RunKey {
            app_id,
            action_id: key,
            idempotency_key: idem,
        };
        let summary: Vec<MiniAppRunValue> = yui::summary(&block.figure, &action.id, &check.values)
            .into_iter()
            .map(|v| MiniAppRunValue {
                label: v.label,
                value: v.value,
            })
            .collect();
        // 3. 同じ冪等キーは 1 回だけ起動する。先に確保し、取れなければ前の結果を返す。
        if !self
            .claim_run(
                ctx,
                &run_key,
                resolved.version,
                &action.label,
                &pin.alias,
                &summary,
            )
            .await?
        {
            return match self.run_of(ctx, &run_key).await? {
                Some((run_id, status)) if run_id.is_some() || status.is_some() => {
                    dispatcher
                        .record_replay(ctx, &source, key, run_id, trace_id)
                        .await;
                    Ok(json!({ "kind": "workflow", "run_id": run_id, "duplicate": true }))
                }
                _ => {
                    dispatcher
                        .deny(ctx, &source, key, "in_progress", trace_id)
                        .await;
                    Err(ActionError::AlreadyInvoked)
                }
            };
        }
        let binding = ActionBinding::Workflow(WorkflowBinding {
            id: key.to_string(),
            workflow: WorkflowPin {
                name: Some(pin.alias.clone()),
                artifact_id: Some(pin.artifact_id),
                version: Some(pin.version),
            },
        });
        let result = dispatcher
            .dispatch_binding(
                ctx,
                &source,
                &binding,
                key,
                Value::Object(check.values),
                trace_id,
            )
            .await;
        match &result {
            Ok(out) => {
                let run_id = out
                    .get("run_id")
                    .and_then(Value::as_str)
                    .and_then(|s| Uuid::parse_str(s).ok());
                if let Err(e) = self.mark_started(ctx, &run_key, run_id).await {
                    // 起動はできている。確保は残るので同じキーで二度は起動しない。
                    tracing::error!(error = %e, action_id = key, "画面のボタンの起動の記録に失敗");
                }
            }
            Err(_) => {
                // 起動できなかった送信は残さない（同じ値で押し直せる）。
                if let Err(e) = self.drop_run(ctx, &run_key).await {
                    tracing::warn!(error = %e, action_id = key, "画面のボタンの確保の解除に失敗");
                }
            }
        }
        result
    }

    /// 本人が画面のボタンで起動した記録（新しい [`MAX_RUNS`] 件を古い順に）。
    ///
    /// 本体の viewer を要求する（共有を外された人は自分の記録も読めない）。未確定の状態は run を
    /// 読み、終わっていたら書き戻す。
    pub async fn my_runs(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Vec<MiniAppRun>, GuiError> {
        let meta = self.artifacts.get(ctx, app_id, trace_id).await?;
        if meta.kind != artifact::ArtifactKind::MiniApp {
            return Err(GuiError::Artifact(artifact::ArtifactError::NotFound));
        }
        let mut rows: Vec<RunRow> = sqlx::query_as(
            "SELECT action_id, app_version, label, workflow_name, run_id, status, summary, invoked_at \
             FROM miniapp_action_run \
             WHERE tenant_id = $1 AND app_id = $2 AND invoked_by = $3 \
             ORDER BY invoked_at DESC, idempotency_key DESC LIMIT $4",
        )
        .bind(&ctx.tenant_id)
        .bind(app_id)
        .bind(&ctx.principal.id)
        .bind(MAX_RUNS)
        .fetch_all(&self.db)
        .await
        .map_err(|e| GuiError::Internal(format!("db: {e}")))?;
        rows.reverse();
        let open: Vec<Uuid> = rows
            .iter()
            .filter(|r| r.5.is_none())
            .filter_map(|r| r.4)
            .collect();
        let live = self.live_statuses(ctx, app_id, &open).await;
        Ok(rows
            .into_iter()
            .map(
                |(action_id, app_version, label, workflow_name, run_id, status, summary, at)| {
                    let status = match (status.as_deref().and_then(MiniAppRunStatus::parse), run_id)
                    {
                        (Some(s), _) => s,
                        (None, Some(id)) => {
                            live.get(&id).copied().unwrap_or(MiniAppRunStatus::Queued)
                        }
                        (None, None) => MiniAppRunStatus::Starting,
                    };
                    MiniAppRun {
                        action_id,
                        app_version,
                        label,
                        workflow_name,
                        run_id,
                        status,
                        values: summary.0,
                        invoked_at: at,
                    }
                },
            )
            .collect())
    }

    /// 未確定の run の今の状態（読めなければ空・表示は「待ち」のまま）。終わったものは書き戻す。
    async fn live_statuses(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
        run_ids: &[Uuid],
    ) -> HashMap<Uuid, MiniAppRunStatus> {
        if run_ids.is_empty() {
            return HashMap::new();
        }
        let reader = workflow_engine::RunStatusReader::new(self.db.clone());
        let statuses = match reader.statuses(ctx, run_ids).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "画面のボタンで起動した run の状態を読めない");
                return HashMap::new();
            }
        };
        let mut out = HashMap::new();
        let mut done: Vec<(Uuid, &'static str)> = Vec::new();
        for (id, s) in statuses {
            let (status, terminal) = match s {
                workflow_engine::RunStatus::Queued => (MiniAppRunStatus::Queued, None),
                workflow_engine::RunStatus::Running => (MiniAppRunStatus::Running, None),
                workflow_engine::RunStatus::Succeeded => {
                    (MiniAppRunStatus::Succeeded, Some("succeeded"))
                }
                workflow_engine::RunStatus::Failed => (MiniAppRunStatus::Failed, Some("failed")),
                workflow_engine::RunStatus::Cancelled => {
                    (MiniAppRunStatus::Cancelled, Some("cancelled"))
                }
            };
            out.insert(id, status);
            if let Some(t) = terminal {
                done.push((id, t));
            }
        }
        if !done.is_empty() {
            let ids: Vec<Uuid> = done.iter().map(|d| d.0).collect();
            let sts: Vec<&str> = done.iter().map(|d| d.1).collect();
            // 書き戻しは best-effort（失敗しても次の表示でまた読む）。
            if let Err(e) = sqlx::query(
                "UPDATE miniapp_action_run r SET status = x.status, finished_at = now() \
                 FROM unnest($3::uuid[], $4::text[]) AS x(run_id, status) \
                 WHERE r.tenant_id = $1 AND r.app_id = $2 AND r.run_id = x.run_id \
                   AND r.status IS NULL",
            )
            .bind(&ctx.tenant_id)
            .bind(app_id)
            .bind(&ids)
            .bind(&sts)
            .execute(&self.db)
            .await
            {
                tracing::warn!(error = %e, "画面のボタンで起動した run の状態の書き戻しに失敗");
            }
        }
        out
    }

    async fn claim_run(
        &self,
        ctx: &AuthContext,
        k: &RunKey<'_>,
        app_version: i64,
        label: &str,
        workflow_name: &str,
        summary: &[MiniAppRunValue],
    ) -> Result<bool, ActionError> {
        let done = sqlx::query(
            "INSERT INTO miniapp_action_run \
                 (tenant_id, app_id, invoked_by, action_id, idempotency_key, org, app_version, \
                  label, workflow_name, summary) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
             ON CONFLICT DO NOTHING",
        )
        .bind(&ctx.tenant_id)
        .bind(k.app_id)
        .bind(&ctx.principal.id)
        .bind(k.action_id)
        .bind(k.idempotency_key)
        .bind(&ctx.org)
        .bind(app_version)
        .bind(label)
        .bind(workflow_name)
        .bind(Json(summary))
        .execute(&self.db)
        .await
        .map_err(|e| map_db(&e))?;
        Ok(done.rows_affected() > 0)
    }

    async fn run_of(
        &self,
        ctx: &AuthContext,
        k: &RunKey<'_>,
    ) -> Result<Option<(Option<Uuid>, Option<String>)>, ActionError> {
        sqlx::query_as(
            "SELECT run_id, status FROM miniapp_action_run \
             WHERE tenant_id = $1 AND app_id = $2 AND invoked_by = $3 AND action_id = $4 \
               AND idempotency_key = $5",
        )
        .bind(&ctx.tenant_id)
        .bind(k.app_id)
        .bind(&ctx.principal.id)
        .bind(k.action_id)
        .bind(k.idempotency_key)
        .fetch_optional(&self.db)
        .await
        .map_err(|e| map_db(&e))
    }

    async fn mark_started(
        &self,
        ctx: &AuthContext,
        k: &RunKey<'_>,
        run_id: Option<Uuid>,
    ) -> Result<(), ActionError> {
        // run が無い＝同時実行の上限で見送り（起動しなかった事実も残す）。
        sqlx::query(
            "UPDATE miniapp_action_run SET run_id = $6, \
                 status = CASE WHEN $6::uuid IS NULL THEN 'skipped' END, \
                 finished_at = CASE WHEN $6::uuid IS NULL THEN now() END \
             WHERE tenant_id = $1 AND app_id = $2 AND invoked_by = $3 AND action_id = $4 \
               AND idempotency_key = $5",
        )
        .bind(&ctx.tenant_id)
        .bind(k.app_id)
        .bind(&ctx.principal.id)
        .bind(k.action_id)
        .bind(k.idempotency_key)
        .bind(run_id)
        .execute(&self.db)
        .await
        .map_err(|e| map_db(&e))?;
        Ok(())
    }

    async fn drop_run(&self, ctx: &AuthContext, k: &RunKey<'_>) -> Result<(), ActionError> {
        sqlx::query(
            "DELETE FROM miniapp_action_run \
             WHERE tenant_id = $1 AND app_id = $2 AND invoked_by = $3 AND action_id = $4 \
               AND idempotency_key = $5 AND run_id IS NULL AND status IS NULL",
        )
        .bind(&ctx.tenant_id)
        .bind(k.app_id)
        .bind(&ctx.principal.id)
        .bind(k.action_id)
        .bind(k.idempotency_key)
        .execute(&self.db)
        .await
        .map_err(|e| map_db(&e))?;
        Ok(())
    }
}

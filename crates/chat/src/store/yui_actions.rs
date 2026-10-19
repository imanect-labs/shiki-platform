//! 回答の本文の ```ui（Yui）のボタンの実行と記録（#604）。
//!
//! 押せる人・送れる値・押せる回数・結果の残し方を、ここ 1 か所で決める。
//!
//! 1. **押せる人**: 会話の**編集者**だけ（閲覧者は押せない）。実行は押した人の権限で、
//!    ワークフローは本人の ReBAC ∩ declared_scopes（launcher 側）。
//! 2. **値**: 保存済みの本文からブロックを読み直し、部品の定義（`yui::validate_action_values`・
//!    画面と同じ規則）で検証する。ワークフローは run を作る前に `input_schema` でも検証される。
//! 3. **回数**: 会話に送るボタン（ask・plan・`-> chat`）は 1 回だけ（`ui_action_invocation`）。
//!    ワークフローは送信ごとの冪等キーで 1 回だけ起動する（`ui_action_run`）。
//! 4. **結果**: 起動は `ui_action_run` に残り、メッセージと一緒に画面へ、次の回答の履歴へモデルに
//!    渡る。終わった状態は表示のときに run から読み、終わっていたら書き戻す（以後 run が消えても残る）。
//!
//! 束縛（どのワークフローのどの版か）は回答の確定時に**回答した主体**の権限で解決して
//! `ui_action_binding` に残したものだけを使う。画面から束縛は送れない。

use std::collections::HashMap;

use authz::{AuthContext, Relation};
use gui::{
    ActionBinding, ActionDispatcher, ActionError, ActionSource, HandlerBinding, HandlerKind,
    WorkflowBinding, WorkflowPin,
};
use serde_json::{json, Map, Value};
use sqlx::types::Json;
use uuid::Uuid;
use yui::ActionTarget;

use super::ChatStore;
use crate::gui_actions::map_chat_err;
use crate::model::{Message, Role, UiActionRun, UiActionRunStatus, UiActionValue};
use crate::yui_actions::{self as ya, UiWorkflowBinding};
use crate::ChatError;

/// 1 メッセージについて返す起動の記録の上限（新しい順）。同じボタンは何度でも押せるので、
/// 応答と、それを読むモデルの履歴が際限なく伸びないようにする。
pub(crate) const MAX_RUNS_PER_MESSAGE: i64 = 20;

fn map_db(e: &sqlx::Error) -> ChatError {
    ChatError::Internal(format!("db: {e}"))
}

/// 回答の本文の ```ui `app` を「アプリとして保存」するための材料（#604）。
#[derive(Debug, Clone)]
pub struct YuiAppDraft {
    /// ブロックの中身（フェンスを除く）。
    pub source: String,
    /// 図の見出し。
    pub title: String,
    /// ボタンの実行先（別名＝ボタンが参照するワークフローの名前・解決した版）。重複は除く。
    pub workflows: Vec<(String, Uuid, i64)>,
}

/// 図のボタンの実行の依頼（画面から送れるのはこれだけ）。
#[derive(Debug, Clone)]
pub struct YuiActionRequest {
    /// `{ブロックの識別子}:{a1}`。
    pub action_id: String,
    pub values: Map<String, Value>,
    /// ワークフローのボタンでは必須（送信ごとに画面が作る）。
    pub idempotency_key: Option<String>,
}

type RunRow = (
    Uuid,
    String,
    String,
    String,
    Option<Uuid>,
    Option<String>,
    Json<Vec<UiActionValue>>,
    String,
    chrono::DateTime<chrono::Utc>,
    String,
);

/// 回答の確定と同じトランザクションで束縛を残す（確定した直後に押されても束縛が見える）。
pub(super) async fn insert_bindings(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    message_id: Uuid,
    bindings: &[UiWorkflowBinding],
) -> Result<(), ChatError> {
    if bindings.is_empty() {
        return Ok(());
    }
    let ids: Vec<&str> = bindings.iter().map(|b| b.action_id.as_str()).collect();
    let workflows: Vec<Uuid> = bindings.iter().map(|b| b.workflow_id).collect();
    let versions: Vec<i64> = bindings.iter().map(|b| b.version).collect();
    let names: Vec<&str> = bindings.iter().map(|b| b.workflow_name.as_str()).collect();
    sqlx::query(
        "INSERT INTO ui_action_binding \
             (tenant_id, thread_id, message_id, action_id, workflow_id, workflow_version, workflow_name) \
         SELECT m.tenant_id, m.thread_id, m.id, x.action_id, x.workflow_id, x.version, x.name \
         FROM message m, \
              unnest($2::text[], $3::uuid[], $4::bigint[], $5::text[]) AS x(action_id, workflow_id, version, name) \
         WHERE m.id = $1 \
         ON CONFLICT DO NOTHING",
    )
    .bind(message_id)
    .bind(&ids)
    .bind(&workflows)
    .bind(&versions)
    .bind(&names)
    .execute(&mut **tx)
    .await
    .map_err(|e| map_db(&e))?;
    Ok(())
}

impl ChatStore {
    /// UI の操作（図のボタン・generative_ui のアクション）を押せるか＝会話の**編集者**か（#604）。
    ///
    /// 閲覧者に共有した会話から、ワークフローの起動や検索を走らせない。
    pub async fn require_ui_action_editor(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(), ChatError> {
        self.require_thread(
            ctx,
            thread_id,
            Relation::Editor,
            "thread.ui_action",
            trace_id,
        )
        .await
        .map(|_| ())
    }

    /// 図のボタンを実行する（編集者・値の検証・回数・記録）。結果は dispatcher の結果
    /// （workflow: `run_id`、会話に送る: `result.run_id`／`result.message_id`）。
    pub async fn invoke_yui_action(
        &self,
        ctx: &AuthContext,
        dispatcher: &ActionDispatcher,
        thread_id: Uuid,
        message_id: Uuid,
        req: YuiActionRequest,
        trace_id: Option<&str>,
    ) -> Result<Value, ActionError> {
        let source = ActionSource::ChatMessage {
            thread_id,
            message_id,
        };
        let key = req.action_id.as_str();
        // 1. 押せるのは会話の編集者だけ。値の検証より先に通す（閲覧者に検証の結果も返さない）。
        if let Err(e) = self
            .require_thread(
                ctx,
                thread_id,
                Relation::Editor,
                "thread.ui_action",
                trace_id,
            )
            .await
        {
            dispatcher
                .deny(ctx, &source, key, "not_editor", trace_id)
                .await;
            return Err(map_chat_err(e));
        }
        let message = self
            .get_message(ctx, thread_id, message_id, trace_id)
            .await
            .map_err(map_chat_err)?;
        // 2. 保存済みの本文からボタンを読み直す（画面の図は信用しない）。
        let found = (message.role == Role::Assistant)
            .then(|| ya::find_action(&message.content, key))
            .flatten();
        let Some((block, action)) = found else {
            dispatcher
                .deny(ctx, &source, key, "undeclared_action", trace_id)
                .await;
            return Err(ActionError::NotFound);
        };
        let check = yui::validate_action_values(&block.figure, &action.id, &req.values, "ja");
        if !check.ok {
            dispatcher
                .deny(ctx, &source, key, "invalid_values", trace_id)
                .await;
            return Err(ActionError::Invalid(ya::error_text(
                &block.figure,
                &check.errors,
            )));
        }
        match &action.target {
            ActionTarget::Chat => {
                // 3. 会話に送るボタンは 1 回だけ（dispatcher が台帳で確保する）。
                let text = ya::chat_text(&block.figure, &action, &check.values);
                let binding = ActionBinding::Handler(HandlerBinding {
                    id: key.to_string(),
                    handler: HandlerKind::ChatSubmit,
                });
                dispatcher
                    .dispatch_binding(ctx, &source, &binding, key, Value::String(text), trace_id)
                    .await
            }
            ActionTarget::Workflow { .. } => {
                let summary = ya::summary(&block.figure, &action.id, &check.values);
                self.invoke_yui_workflow(
                    ctx,
                    dispatcher,
                    &source,
                    (thread_id, message_id),
                    &req,
                    (&action.label, summary),
                    check.values,
                    trace_id,
                )
                .await
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn invoke_yui_workflow(
        &self,
        ctx: &AuthContext,
        dispatcher: &ActionDispatcher,
        source: &ActionSource,
        (thread_id, message_id): (Uuid, Uuid),
        req: &YuiActionRequest,
        (label, summary): (&str, Vec<UiActionValue>),
        values: Map<String, Value>,
        trace_id: Option<&str>,
    ) -> Result<Value, ActionError> {
        let key = req.action_id.as_str();
        let Some(idem) = req
            .idempotency_key
            .as_deref()
            .filter(|k| ya::valid_idempotency_key(k))
        else {
            dispatcher
                .deny(ctx, source, key, "missing_idempotency_key", trace_id)
                .await;
            return Err(ActionError::Invalid(
                "idempotency_key（英数字・-・_ の 8〜64 文字）が必要です".into(),
            ));
        };
        // 束縛は回答の確定時に解決したものだけ（無い＝解決できなかった・権限が無かった）。
        let Some((workflow_id, version, workflow_name)) = self
            .ui_binding(ctx, thread_id, message_id, key)
            .await
            .map_err(map_chat_err)?
        else {
            dispatcher
                .deny(ctx, source, key, "unresolved_workflow", trace_id)
                .await;
            return Err(ActionError::NotFound);
        };
        // 3. 同じ冪等キーは 1 回だけ起動する。先に確保し、取れなければ前の結果を返す。
        let claimed = self
            .claim_ui_run(
                ctx,
                (thread_id, message_id, key, idem),
                label,
                &workflow_name,
                &summary,
            )
            .await
            .map_err(map_chat_err)?;
        if !claimed {
            return match self
                .ui_run_id(ctx, (thread_id, message_id, key, idem))
                .await
                .map_err(map_chat_err)?
            {
                // 同じ送信の再送: 前の結果を返す（run を増やさない）。起動はしないが監査には残す。
                Some((run_id, status)) if run_id.is_some() || status.is_some() => {
                    dispatcher
                        .record_replay(ctx, source, key, run_id, trace_id)
                        .await;
                    Ok(json!({ "kind": "workflow", "run_id": run_id, "duplicate": true }))
                }
                // まだ起動の途中（先の送信が実行中）。
                _ => {
                    dispatcher
                        .deny(ctx, source, key, "in_progress", trace_id)
                        .await;
                    Err(ActionError::AlreadyInvoked)
                }
            };
        }
        let binding = ActionBinding::Workflow(WorkflowBinding {
            id: key.to_string(),
            workflow: WorkflowPin {
                name: Some(workflow_name),
                artifact_id: Some(workflow_id),
                version: Some(version),
            },
        });
        let result = dispatcher
            .dispatch_binding(ctx, source, &binding, key, Value::Object(values), trace_id)
            .await;
        let ids = (thread_id, message_id, key, idem);
        match &result {
            Ok(out) => {
                let run_id = out
                    .get("run_id")
                    .and_then(Value::as_str)
                    .and_then(|s| Uuid::parse_str(s).ok());
                if let Err(e) = self.mark_ui_run_started(ctx, ids, run_id).await {
                    // 起動はできている。確保は残るので同じキーで二度は起動しない。
                    tracing::error!(error = %e, action_id = key, "図のボタンの起動の記録に失敗");
                }
            }
            Err(_) => {
                // 起動できなかった送信は残さない（同じ値で押し直せる）。
                if let Err(e) = self.drop_ui_run(ctx, ids).await {
                    tracing::warn!(error = %e, action_id = key, "図のボタンの確保の解除に失敗");
                }
            }
        }
        result
    }

    /// 回答の本文の ```ui `app`（識別子 `block_hash`）と、そのボタンの実行先を返す
    /// （「アプリとして保存」の材料）。会話を読める人（viewer）なら使える。
    ///
    /// 実行先は回答の確定時に解決した束縛（`ui_action_binding`）だけ。解決できなかったボタンは
    /// 実行先が無いまま返り、保存時の検証（束ねたワークフローに無い）で弾かれる。保存は保存する人の
    /// 権限でピンを検証し直す（`MiniAppStore::create`）。
    pub async fn yui_app_for_saving(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        message_id: Uuid,
        block_hash: &str,
        trace_id: Option<&str>,
    ) -> Result<Option<YuiAppDraft>, ChatError> {
        self.require_thread(
            ctx,
            thread_id,
            Relation::Viewer,
            "thread.yui_save_as_app",
            trace_id,
        )
        .await?;
        let message = self
            .get_message(ctx, thread_id, message_id, trace_id)
            .await?;
        if message.role != Role::Assistant {
            return Ok(None);
        }
        let Some(block) = ya::action_blocks(&message.content)
            .into_iter()
            .find(|b| b.hash == block_hash && matches!(b.figure, yui::Figure::App { .. }))
        else {
            return Ok(None);
        };
        let rows: Vec<(String, Uuid, i64, String)> = sqlx::query_as(
            "SELECT action_id, workflow_id, workflow_version, workflow_name FROM ui_action_binding \
             WHERE tenant_id = $1 AND thread_id = $2 AND message_id = $3 AND action_id LIKE $4 \
             ORDER BY action_id",
        )
        .bind(&ctx.tenant_id)
        .bind(thread_id)
        .bind(message_id)
        .bind(format!("{block_hash}:%"))
        .fetch_all(&self.db)
        .await
        .map_err(|e| map_db(&e))?;
        let mut workflows: Vec<(String, Uuid, i64)> = Vec::new();
        for (_, id, version, name) in rows {
            if !workflows.iter().any(|(n, _, _)| *n == name) {
                workflows.push((name, id, version));
            }
        }
        let title = match &block.figure {
            yui::Figure::App { base, .. }
            | yui::Figure::Ask { base, .. }
            | yui::Figure::Plan { base, .. } => base.title.clone(),
        };
        Ok(Some(YuiAppDraft {
            source: block.source,
            title,
            workflows,
        }))
    }

    async fn ui_binding(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        message_id: Uuid,
        action_id: &str,
    ) -> Result<Option<(Uuid, i64, String)>, ChatError> {
        sqlx::query_as(
            "SELECT workflow_id, workflow_version, workflow_name FROM ui_action_binding \
             WHERE tenant_id = $1 AND thread_id = $2 AND message_id = $3 AND action_id = $4",
        )
        .bind(&ctx.tenant_id)
        .bind(thread_id)
        .bind(message_id)
        .bind(action_id)
        .fetch_optional(&self.db)
        .await
        .map_err(|e| map_db(&e))
    }

    async fn claim_ui_run(
        &self,
        ctx: &AuthContext,
        (thread_id, message_id, action_id, key): (Uuid, Uuid, &str, &str),
        label: &str,
        workflow_name: &str,
        summary: &[UiActionValue],
    ) -> Result<bool, ChatError> {
        let done = sqlx::query(
            "INSERT INTO ui_action_run \
                 (tenant_id, thread_id, message_id, action_id, idempotency_key, org, invoked_by, \
                  label, workflow_name, summary) \
             SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10 \
             FROM message m \
             WHERE m.id = $3 AND m.thread_id = $2 AND m.tenant_id = $1 \
             ON CONFLICT DO NOTHING",
        )
        .bind(&ctx.tenant_id)
        .bind(thread_id)
        .bind(message_id)
        .bind(action_id)
        .bind(key)
        .bind(&ctx.org)
        .bind(&ctx.principal.id)
        .bind(label)
        .bind(workflow_name)
        .bind(Json(summary))
        .execute(&self.db)
        .await
        .map_err(|e| map_db(&e))?;
        Ok(done.rows_affected() > 0)
    }

    /// 確保済みの送信の run と終わった状態（行が無ければ `None`）。
    async fn ui_run_id(
        &self,
        ctx: &AuthContext,
        (thread_id, message_id, action_id, key): (Uuid, Uuid, &str, &str),
    ) -> Result<Option<(Option<Uuid>, Option<String>)>, ChatError> {
        sqlx::query_as(
            "SELECT run_id, status FROM ui_action_run \
             WHERE tenant_id = $1 AND thread_id = $2 AND message_id = $3 AND action_id = $4 \
               AND idempotency_key = $5",
        )
        .bind(&ctx.tenant_id)
        .bind(thread_id)
        .bind(message_id)
        .bind(action_id)
        .bind(key)
        .fetch_optional(&self.db)
        .await
        .map_err(|e| map_db(&e))
    }

    async fn mark_ui_run_started(
        &self,
        ctx: &AuthContext,
        (thread_id, message_id, action_id, key): (Uuid, Uuid, &str, &str),
        run_id: Option<Uuid>,
    ) -> Result<(), ChatError> {
        // run が無い＝同時実行の上限で見送り（起動しなかった事実も会話に残す）。
        sqlx::query(
            "UPDATE ui_action_run SET run_id = $6, \
                 status = CASE WHEN $6::uuid IS NULL THEN 'skipped' END, \
                 finished_at = CASE WHEN $6::uuid IS NULL THEN now() END \
             WHERE tenant_id = $1 AND thread_id = $2 AND message_id = $3 AND action_id = $4 \
               AND idempotency_key = $5",
        )
        .bind(&ctx.tenant_id)
        .bind(thread_id)
        .bind(message_id)
        .bind(action_id)
        .bind(key)
        .bind(run_id)
        .execute(&self.db)
        .await
        .map_err(|e| map_db(&e))?;
        Ok(())
    }

    async fn drop_ui_run(
        &self,
        ctx: &AuthContext,
        (thread_id, message_id, action_id, key): (Uuid, Uuid, &str, &str),
    ) -> Result<(), ChatError> {
        sqlx::query(
            "DELETE FROM ui_action_run \
             WHERE tenant_id = $1 AND thread_id = $2 AND message_id = $3 AND action_id = $4 \
               AND idempotency_key = $5 AND run_id IS NULL AND status IS NULL",
        )
        .bind(&ctx.tenant_id)
        .bind(thread_id)
        .bind(message_id)
        .bind(action_id)
        .bind(key)
        .execute(&self.db)
        .await
        .map_err(|e| map_db(&e))?;
        Ok(())
    }

    /// スレッドの図のボタンの状態（押せるワークフローのボタン・起動の記録）をメッセージへ載せる。
    ///
    /// 起動の記録で状態が未確定のものは run の状態を読み、終わっていたら書き戻す。
    pub(super) async fn attach_yui_state(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        messages: &mut [Message],
    ) -> Result<(), ChatError> {
        let bound: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT message_id, action_id FROM ui_action_binding \
             WHERE tenant_id = $1 AND thread_id = $2 ORDER BY message_id, action_id",
        )
        .bind(&ctx.tenant_id)
        .bind(thread_id)
        .fetch_all(&self.db)
        .await
        .map_err(|e| map_db(&e))?;
        let rows: Vec<RunRow> = sqlx::query_as(
            "SELECT message_id, action_id, label, workflow_name, run_id, status, summary, \
                    invoked_by, invoked_at, idempotency_key \
             FROM ( \
                 SELECT *, row_number() OVER ( \
                     PARTITION BY message_id ORDER BY invoked_at DESC, idempotency_key DESC \
                 ) AS rn \
                 FROM ui_action_run WHERE tenant_id = $1 AND thread_id = $2 \
             ) r WHERE rn <= $3 \
             ORDER BY invoked_at, idempotency_key",
        )
        .bind(&ctx.tenant_id)
        .bind(thread_id)
        .bind(MAX_RUNS_PER_MESSAGE)
        .fetch_all(&self.db)
        .await
        .map_err(|e| map_db(&e))?;

        let open: Vec<Uuid> = rows
            .iter()
            .filter(|r| r.5.is_none())
            .filter_map(|r| r.4)
            .collect();
        let live = self.live_run_statuses(ctx, thread_id, &open).await;

        let mut bound_by: HashMap<Uuid, Vec<String>> = HashMap::new();
        for (m, a) in bound {
            bound_by.entry(m).or_default().push(a);
        }
        let mut runs_by: HashMap<Uuid, Vec<UiActionRun>> = HashMap::new();
        for (m, action_id, label, workflow_name, run_id, status, summary, by, at, _) in rows {
            let status = match (status.as_deref().and_then(UiActionRunStatus::parse), run_id) {
                (Some(s), _) => s,
                (None, Some(id)) => live.get(&id).copied().unwrap_or(UiActionRunStatus::Queued),
                (None, None) => UiActionRunStatus::Starting,
            };
            runs_by.entry(m).or_default().push(UiActionRun {
                action_id,
                label,
                workflow_name,
                run_id,
                status,
                values: summary.0,
                invoked_by: by,
                invoked_at: at,
            });
        }
        for m in messages.iter_mut() {
            if let Some(ids) = bound_by.remove(&m.id) {
                m.bound_actions = ids;
            }
            if let Some(runs) = runs_by.remove(&m.id) {
                m.ui_runs = runs;
            }
        }
        Ok(())
    }

    /// 未確定の run の今の状態（読めなければ空・表示は「待ち」のまま）。終わったものは書き戻す。
    async fn live_run_statuses(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        run_ids: &[Uuid],
    ) -> HashMap<Uuid, UiActionRunStatus> {
        if run_ids.is_empty() {
            return HashMap::new();
        }
        let reader = workflow_engine::RunStatusReader::new(self.db.clone());
        let statuses = match reader.statuses(ctx, run_ids).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "図のボタンで起動した run の状態を読めない");
                return HashMap::new();
            }
        };
        let mut out = HashMap::new();
        for (id, s) in statuses {
            let status = match s {
                workflow_engine::RunStatus::Queued => UiActionRunStatus::Queued,
                workflow_engine::RunStatus::Running => UiActionRunStatus::Running,
                workflow_engine::RunStatus::Succeeded => UiActionRunStatus::Succeeded,
                workflow_engine::RunStatus::Failed => UiActionRunStatus::Failed,
                workflow_engine::RunStatus::Cancelled => UiActionRunStatus::Cancelled,
            };
            out.insert(id, status);
        }
        let done: Vec<(Uuid, &str)> = out
            .iter()
            .filter(|(_, s)| s.is_terminal())
            .map(|(id, s)| (*id, s.as_str()))
            .collect();
        if !done.is_empty() {
            let ids: Vec<Uuid> = done.iter().map(|d| d.0).collect();
            let sts: Vec<&str> = done.iter().map(|d| d.1).collect();
            // 書き戻しは best-effort（失敗しても次の表示でまた読む）。
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
        out
    }
}

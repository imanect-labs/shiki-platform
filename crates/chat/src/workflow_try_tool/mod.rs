//! 会話の AI の試運転 `try_workflow`（T04・D-10・D-197）。
//!
//! 保存したワークフローの**版を固定して**、承認カード（`requires_confirmation`）を経て、話した本人の権限で試しに
//! 実行し、結果を観測として返す。起動の入口は [`WorkflowRunLauncher::start_version_as_editor`]（editor 以上・
//! 版は呼び出し側が必ず渡す）で、run の主体は本人・trigger_kind は interactive のまま。
//!
//! - **id だけで起動する。** `workflow_id` に名前が来たら起動せず、解決した id と版を示して「workflow_id に id を
//!   入れて呼び直してください」と返す。承認カードで見たものと走るものを一致させるため（名前は承認を待つ間に付け
//!   替えられる・TOCTOU）。
//! - **台帳**（`workflow_trial_run`・[`crate::store`] の trial_runs）: 同じ会話・同じ話した人・同じ版・同じ入力の
//!   前の run が実行中・順番待ちなら合流し（新しい run を作らない）、終わっていれば `rerun` が無い限り前回の結果を
//!   返す。起動する run の id は予約のときに決め（`planned_run_id`）、run はその id で作る。run_id が無い行でも、
//!   その id の run があれば起動したものとして付け直して合流する（run を作った後に台帳へ書けなかった・2 本目を
//!   立てない）。run の無い起動の途中の行は 2 分までは「起動中」として合流し、過ぎたら古い予約として消す。新しい
//!   run は会話の 1 回の生成あたり [`MAX_TRIALS_PER_ANSWER`] 本まで。
//! - **待ち方**（[`wait`]）: 待つ間は生成の枠を返す。上限（[`TryWaitLimits`]・既定 30 秒）まで待って、その時点の
//!   状態を返す。
//! - **観測**（[`observe`]）: 作成者や外の相手が書いた文字は `<run_output>` の封筒の中にだけ置く（PIT-56）。
//! - **共有の相手がいる会話**（[`shared`]・PIT-84・D-214）: 状態・件数・画面の道だけを返す（出力の先頭・誤りの文・
//!   IR の中身・レコードの id・費用の数字を外す）。相手の有無は応答の文を組む直前に、スレッドの直接のタプルで判定する
//!   （`ChatStore::thread_audience`・読めなければ相手がいる側）。1 回の生成の中で一度でも相手がいると判定したら、その
//!   生成の終わりまで絞ったままにする（道具は生成ごとに作られる）。run が無い道（名前で呼んだ・版が無い・起動され
//!   なかった・入力と止める設定の誤り）も、名前の解決・最新の版・設定名・理由の文を返さない。絞った観測をそのまま
//!   tool_result として保存するので、本人と相手の画面・次の回答の文脈・分岐・書き出しのどれにも、この形だけが届く。
//! - **内部の誤り**は中身をログに回し、固定の文だけを返す（保存済みの IR のパースの誤りの文が、作成者の文字を封筒の
//!   外に出さないように・PIT-56）。保存済みの版を読めないときは「何度呼んでも同じ」と言い分ける。
//! - **費用**: run の trace（`run_id.simple()`）の LLM の利用量を集計し、台帳に報告した分との差分だけを
//!   [`ToolOutcome::usage`] に入れる（親の予算に積む・合流や前回の結果で同じ費用を二重に積まない）。集計は予約の時刻
//!   から、終わった run なら終わった時刻の少し後まで（[`USAGE_TAIL`]）に閉じる（前回の結果を返すたびに、その後の
//!   組織の利用量を読まない）。`workflow.start` の子の run（別の trace）と、待つのをやめた後の分は入らない。
//! - `is_error` は起動できなかったとき（見つからない・閲覧のみ・版が無い・保存済みの版を読み込めない・入力・実行の
//!   前に止める設定・上限・起動されなかった）だけ。run が失敗しても観測としては成功で、失敗の中身を返す（共有の相手が
//!   いる会話では失敗したことだけ）。

mod launch;
mod observe;
mod shared;
mod wait;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use agent_core::{Tool, ToolError, ToolName, ToolOutcome, ToolUsage};
use async_trait::async_trait;
use authz::AuthContext;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use sqlx::PgPool;
use uuid::Uuid;
use workflow_engine::run::{LauncherError, RunDigest};
use workflow_engine::{
    RunOutcomeReader, RunStore, RunTrial, WorkflowIr, WorkflowRunLauncher, WorkflowStore,
    WorkflowStoreError,
};

use crate::store::{ChatStore, TrialKey, TrialRow, STALE_RESERVATION_SECS};
use crate::tool_args::{accept_object_args, ObjectArg};
use crate::worker::slot::RunSlot;
use crate::worker::TryWaitLimits;
use observe::{Observed, Origin, RunCost};
use shared::SharedFacts;
use wait::Waiter;

/// 会話の 1 回の生成（chat の run）で新しく起動できる試運転の数。
pub const MAX_TRIALS_PER_ANSWER: i64 = 5;

/// 終わった run の費用を、終わった時刻からどれだけ後まで数えるか（利用量の行は呼び出しの終わりに書くので、取り消しと
/// 重なった呼び出しの分も入るよう少し余裕を持たせる）。
const USAGE_TAIL: chrono::Duration = chrono::Duration::minutes(5);

/// 試運転の入力（`input`・object の引数の受け口は道具で共通・GAP-130）。
const INPUT_ARG: ObjectArg = ObjectArg {
    key: "input",
    shape: "入力の項目のキー → 値",
    sql_arg: None,
};

/// 試すきっかけの内容（`trigger`・`$from trigger` の源）。
const TRIGGER_ARG: ObjectArg = ObjectArg {
    key: "trigger",
    shape: "きっかけの内容（record・file など、`$from trigger` で読む値を持つオブジェクト）",
    sql_arg: None,
};

const DESCRIPTION: &str = "保存したワークフローの版を、ユーザーの承認を得てからユーザー本人の権限で試しに実行し、結果\
（ブロックごとの状態・出力の先頭・誤り・作ったレコード）を返す。workflow_id には list_workflows・emit_workflow・\
read_workflow が返した id（UUID）を、version には試す版を必ず渡す（名前では起動しない）。実行中なら最大 30 秒ほど\
待ってその時点の状態を返す。同じ版・同じ入力の前回の結果があればそれを返すので、直した後に試し直すときは rerun: \
true を付ける。試運転でも書き込み・送信・レコードの作成は本当に起きる。文字の入力には【試運転】を前置する。共有の相手が\
いる会話では、状態・件数・画面の道だけを返す。";

/// 道具が使う依存（ワーカーが配線する）。
pub struct TryWorkflowDeps {
    pub workflows: Arc<WorkflowStore>,
    pub launcher: Arc<WorkflowRunLauncher>,
    pub runs: Arc<RunStore>,
    pub chat: ChatStore,
    /// 状態の読み口（[`RunOutcomeReader`]）と LLM の利用量（`llm_usage`）を読む DB。
    pub db: PgPool,
}

/// この道具が効く範囲（会話の 1 回の生成）。
pub struct TryScope {
    /// 会話の生成の run（`generation_run.run_id`・上限の数え上げ）。
    pub chat_run_id: Uuid,
    pub thread_id: Uuid,
    /// 会話の生成の trace_id（台帳に残す）。
    pub chat_trace_id: Option<String>,
    /// 生成の枠（待つ間は返す）。
    pub slot: Arc<RunSlot>,
    /// 会話の停止の旗（立ったら待つのをやめる・run は取り消さない）。
    pub cancel: Arc<AtomicBool>,
    pub wait: TryWaitLimits,
}

/// `try_workflow`（上のモジュールの説明）。
pub struct TryWorkflowTool {
    deps: TryWorkflowDeps,
    scope: TryScope,
    outcomes: RunOutcomeReader,
    usage: llm_gateway::accounting::UsageRecorder,
    /// この生成の中で、一度でも会話に共有の相手がいると判定したか（立ったら生成の終わりまで絞る・[`Self::is_shared`]）。
    shared_seen: AtomicBool,
}

impl TryWorkflowTool {
    #[must_use]
    pub fn new(deps: TryWorkflowDeps, scope: TryScope) -> Self {
        let outcomes = RunOutcomeReader::new(deps.db.clone());
        let usage = llm_gateway::accounting::UsageRecorder::new(deps.db.clone());
        TryWorkflowTool {
            deps,
            scope,
            outcomes,
            usage,
            shared_seen: AtomicBool::new(false),
        }
    }

    /// 会話に、話した人のほかに読める人がいるか（PIT-84・D-214）。応答の文を組む直前に呼ぶ。
    ///
    /// 一度でも相手がいると判定したら、この道具の実体（＝1 回の生成）の終わりまで、読み直さずに相手がいるとする
    /// （待つ間や承認を待つ間に共有を外しても、同じ生成の中では絞ったまま）。読めなければ相手がいる側
    /// （`ChatStore::thread_audience` の fail-closed）。
    async fn is_shared(&self, ctx: &AuthContext) -> bool {
        if self.shared_seen.load(Ordering::Relaxed) {
            return true;
        }
        let shared = self
            .deps
            .chat
            .thread_audience(ctx, self.scope.thread_id)
            .await
            .is_shared();
        if shared {
            self.shared_seen.store(true, Ordering::Relaxed);
        }
        shared
    }
}

/// 読んだ入力。
struct TryRequest {
    workflow: String,
    version: i64,
    input: Value,
    trigger: Option<Value>,
    scheduled_at: Option<DateTime<Utc>>,
    rerun: bool,
}

impl TryRequest {
    fn parse(mut raw: Value) -> Result<Self, ToolError> {
        accept_object_args(&mut raw, &[INPUT_ARG, TRIGGER_ARG]).map_err(ToolError::Invalid)?;
        let workflow = raw
            .get("workflow_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ToolError::Invalid(
                    "workflow_id（list_workflows・emit_workflow・read_workflow が返した id）は必須です".into(),
                )
            })?
            .to_string();
        let version = match raw.get("version") {
            // 2.0 のような整数の小数も受ける（JSON の 2.0 は承認カードの JS では 2 と区別できない）。
            Some(Value::Number(n)) => n.as_i64().or_else(|| {
                n.as_f64()
                    .filter(|f| f.fract() == 0.0 && (1.0..=9.0e15).contains(f))
                    .map(|f| f as i64)
            }),
            Some(Value::String(s)) => s.trim().trim_start_matches(['v', 'V']).parse().ok(),
            _ => None,
        }
        .filter(|v| *v >= 1)
        .ok_or_else(|| {
            ToolError::Invalid(
                "version（試す版の番号・1 以上の整数）は必須です。read_workflow か list_workflows が返した版を渡して\
                 ください"
                    .into(),
            )
        })?;
        let input = match raw.get("input") {
            None | Some(Value::Null) => json!({}),
            Some(v @ Value::Object(_)) => v.clone(),
            Some(_) => return Err(ToolError::Invalid(INPUT_ARG.not_object())),
        };
        let trigger = match raw.get("trigger") {
            None | Some(Value::Null) => None,
            Some(v @ Value::Object(_)) => Some(v.clone()),
            Some(_) => return Err(ToolError::Invalid(TRIGGER_ARG.not_object())),
        };
        let scheduled_at = match raw.get("scheduled_at") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if s.trim().is_empty() => None,
            Some(Value::String(s)) => Some(
                workflow_engine::run::parse_trial_scheduled_at(s).ok_or_else(|| {
                    ToolError::Invalid(
                        workflow_engine::run::planned::TRIAL_SCHEDULED_AT_INVALID.into(),
                    )
                })?,
            ),
            Some(_) => {
                return Err(ToolError::Invalid(
                    workflow_engine::run::planned::TRIAL_SCHEDULED_AT_INVALID.into(),
                ))
            }
        };
        let rerun = raw.get("rerun").and_then(Value::as_bool).unwrap_or(false);
        Ok(TryRequest {
            workflow,
            version,
            input,
            trigger,
            scheduled_at,
            rerun,
        })
    }
}

/// キーを並べ直した JSON（serde_json の preserve_order の有無に依らない形）。
fn canonical(v: &Value, out: &mut String) {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(k.clone()).to_string());
                out.push(':');
                canonical(&m[k], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// 試運転の入力の hash（sha256 の 16 進・キーを並べ直した `{input, trigger, scheduled_at}`）。
fn input_hash(
    input: &Value,
    trigger: Option<&Value>,
    scheduled_at: Option<DateTime<Utc>>,
) -> String {
    let doc = json!({
        "input": input,
        "trigger": trigger,
        "scheduled_at": scheduled_at.map(|t| t.to_rfc3339()),
    });
    let mut text = String::new();
    canonical(&doc, &mut text);
    let digest = Sha256::digest(text.as_bytes());
    digest.iter().fold(String::with_capacity(64), |mut s, b| {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// 内部の誤り（DB・authz・artifact）の固定の文。
const UNAVAILABLE: &str =
    "試運転の準備に失敗しました（一時的な問題）。少し待ってからやり直してください。";

/// 内部の誤りを写す。中身はログにだけ残し、モデルには固定の文を返す（Debug の文には、保存済みの IR のパースの誤りの
/// ような作成者の文字が入りうる・PIT-56）。
fn unavailable(e: impl std::fmt::Display) -> ToolError {
    tracing::warn!(error = %e, "試運転の準備に失敗");
    ToolError::Unavailable(UNAVAILABLE.into())
}

/// 版の IR を読めなかったときの写し方。見つからない・見えないは「見つからない」、保存済みの定義を読めない
/// （[`WorkflowStoreError::Validation`]）は何度呼んでも同じなので一時的とは言わずに `is_error` で返し、ほかは内部の誤り。
fn version_read_error(
    e: WorkflowStoreError,
    workflow: &str,
    workflow_id: Uuid,
    version: i64,
) -> Result<ToolOutcome, ToolError> {
    match e {
        WorkflowStoreError::Artifact(
            artifact::ArtifactError::NotFound | artifact::ArtifactError::Forbidden,
        ) => Ok(not_found(workflow)),
        WorkflowStoreError::Validation(errors) => {
            tracing::warn!(%workflow_id, version, count = errors.len(), "保存済みのワークフローの版を読み込めない");
            Ok(ToolOutcome::error(format!(
                "v{version} の保存済みの定義を読み込めません（何度呼んでも同じです）。別の版を試すか、ユーザーにワークフローの\
                 画面で確かめてもらってください。"
            )))
        }
        e @ WorkflowStoreError::Artifact(_) => Err(unavailable(format!("{e:?}"))),
    }
}

/// 見つからない（無い・見えない・ワークフローでない・版が無いを言い分けない）。
fn not_found(workflow: &str) -> ToolOutcome {
    ToolOutcome::error(format!(
        "ワークフローが見つからないか、見る権限がありません（workflow_id: {}）。list_workflows で id と版を確かめて\
         ください。",
        observe::sanitize(workflow, 80)
    ))
}

#[async_trait]
impl Tool for TryWorkflowTool {
    fn name(&self) -> &str {
        ToolName::TryWorkflow.as_str()
    }

    fn description(&self) -> &str {
        DESCRIPTION
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "workflow_id": {
                    "type": "string",
                    "description": "試すワークフローの id（UUID）。list_workflows・emit_workflow・read_workflow が返した id。名前では起動しない"
                },
                "version": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "試す版（必須）。直して保存した後は、保存で返った新しい版を渡す"
                },
                "input": {
                    "type": "object",
                    "description": "入力の項目のキー → 値（入力の項目が無ければ省く）。文字の値は先頭に【試運転】を付ける"
                },
                "trigger": {
                    "type": "object",
                    "description": "スケジュール・できごとをきっかけにするワークフローで試すきっかけの内容（$from trigger で読む値）。省けば入力と同じ"
                },
                "scheduled_at": {
                    "type": "string",
                    "description": "試す予定の日時（2026-11-09T09:00 など・時差を書かなければ日本時間）。省けば今"
                },
                "rerun": {
                    "type": "boolean",
                    "description": "同じ版・同じ入力の前回の結果を返さずに、新しく実行し直す（テーブルや設定を直した後に）"
                }
            },
            "required": ["workflow_id", "version"]
        })
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let req = TryRequest::parse(input)?;
        let Ok(workflow_id) = Uuid::parse_str(&req.workflow) else {
            return self.named(ctx, &req, trace_id).await;
        };
        // (1) 見えるか（viewer・無い・見えない・ワークフローでないは同じ「見つからない」）と、版の IR。
        let meta = match self
            .deps
            .workflows
            .resolve_ref(ctx, &workflow_id.to_string(), trace_id)
            .await
        {
            Ok(m) => m,
            Err(WorkflowStoreError::Artifact(artifact::ArtifactError::NotFound)) => {
                return Ok(not_found(&req.workflow))
            }
            Err(e) => return Err(unavailable(format!("{e:?}"))),
        };
        if req.version > meta.current_version {
            if self.is_shared(ctx).await {
                return Ok(ToolOutcome::error(shared::no_such_version(req.version)));
            }
            return Ok(ToolOutcome::error(format!(
                "v{} はありません（今の最新は v{}）。read_workflow で版を確かめてください。",
                req.version, meta.current_version
            )));
        }
        let ir = match self
            .deps
            .workflows
            .get_version(ctx, workflow_id, req.version, trace_id)
            .await
        {
            Ok((_, ir)) => ir,
            Err(e) => return version_read_error(e, &req.workflow, workflow_id, req.version),
        };
        // (2) 入力の hash と台帳の鍵。
        let hash = input_hash(&req.input, req.trigger.as_ref(), req.scheduled_at);
        let key = TrialKey {
            thread_id: self.scope.thread_id,
            workflow_id,
            version: req.version,
            input_hash: &hash,
        };
        // (3) 前の行（合流・前回の結果・起動中）。
        if let Some(out) = self.previous(ctx, &key, &ir, req.rerun).await? {
            return Ok(out);
        }
        // (4) 上限。
        let used = self
            .deps
            .chat
            .count_trial_runs(ctx, self.scope.chat_run_id)
            .await
            .map_err(unavailable)?;
        if used >= MAX_TRIALS_PER_ANSWER {
            return Ok(ToolOutcome::error(format!(
                "この回答の中で新しく試運転できるのは {MAX_TRIALS_PER_ANSWER} 回までです。ここまでの結果をユーザーに伝え、\
                 続けるかを聞いてください。"
            )));
        }
        // (5) 起動の前に予約する。(6) 起動する。
        let row = self
            .deps
            .chat
            .reserve_trial_run(
                ctx,
                self.scope.chat_run_id,
                self.scope.chat_trace_id.as_deref(),
                key,
            )
            .await
            .map_err(unavailable)?;
        let trial = RunTrial {
            trigger: req.trigger.as_ref(),
            scheduled_at: req.scheduled_at,
            rerun_of: None,
        };
        let started = self
            .deps
            .launcher
            .start_version_as_editor(
                ctx,
                workflow_id,
                req.version,
                &req.input,
                trial,
                row.planned_run_id,
                trace_id,
            )
            .await;
        match started {
            Ok(Some(run_id)) => {
                self.record_start(ctx, &row, workflow_id, req.version, run_id, &hash, trace_id)
                    .await;
                Ok(self
                    .follow(
                        ctx,
                        &ir,
                        &row,
                        workflow_id,
                        req.version,
                        run_id,
                        Origin::Started,
                    )
                    .await)
            }
            Ok(None) => {
                self.release(ctx, &row).await;
                if self.is_shared(ctx).await {
                    return Ok(ToolOutcome::error(shared::NOT_LAUNCHED));
                }
                Ok(ToolOutcome::error(
                    "起動されませんでした。同時に動かせる数の上限に達していて、このワークフローは上限を超えたら起動しない\
                     設定です（policies.on_trigger_overflow: skip）。前の実行が終わってから試してください。",
                ))
            }
            Err(e) => {
                self.release(ctx, &row).await;
                // 文が共有の相手で変わるのは入力と止める設定だけ（ほかは固定の文なので判定しない）。
                let shared = matches!(e, LauncherError::Input(_) | LauncherError::NotReady(_))
                    && self.is_shared(ctx).await;
                Ok(launch::launch_error(&e, workflow_id, req.version, shared))
            }
        }
    }
}

impl TryWorkflowTool {
    /// `workflow_id` に名前が来た（起動せず、解決した id と版を示して呼び直してもらう）。共有の相手がいる会話では名前を
    /// 解決せず、id も最新の版も言わない。
    async fn named(
        &self,
        ctx: &AuthContext,
        req: &TryRequest,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        if self.is_shared(ctx).await {
            return Ok(ToolOutcome::error(shared::named()));
        }
        match self
            .deps
            .workflows
            .resolve_ref(ctx, &req.workflow, trace_id)
            .await
        {
            Ok(meta) => Ok(ToolOutcome::error(format!(
                "起動していません。workflow_id には名前ではなく id（UUID）を入れてください。「{}」の id は {}、最新の版は \
                 v{} です。workflow_id に {} を、version に試す版を入れて呼び直してください（承認したものと動かすものを\
                 一致させるため、名前では起動しません）。",
                observe::sanitize(&req.workflow, 80),
                meta.id,
                meta.current_version,
                meta.id
            ))),
            Err(WorkflowStoreError::Artifact(artifact::ArtifactError::NotFound)) => {
                Ok(not_found(&req.workflow))
            }
            Err(e) => Err(unavailable(format!("{e:?}"))),
        }
    }

    /// 同じ鍵の前の行を見る。合流・前回の結果・起動中なら観測を返す。前の行が無い・run が消えた・終わっていて
    /// `rerun` なら `None`（新しく起動する）。
    async fn previous(
        &self,
        ctx: &AuthContext,
        key: &TrialKey<'_>,
        ir: &WorkflowIr,
        rerun: bool,
    ) -> Result<Option<ToolOutcome>, ToolError> {
        self.settle_stale(ctx, key.thread_id).await?;
        let Some(prev) = self
            .deps
            .chat
            .find_latest_trial_run(ctx, *key)
            .await
            .map_err(unavailable)?
        else {
            return Ok(None);
        };
        // run_id の無い行も、予約で決めた id の run があれば起動している（run を作った後に台帳へ書けなかった）。
        let run_id = prev.run_id.unwrap_or(prev.planned_run_id);
        let state = self
            .outcomes
            .live_states(ctx, &[run_id])
            .await
            .map_err(unavailable)?
            .remove(&run_id);
        if prev.run_id.is_none() {
            if state.is_none() {
                return Ok(Some(ToolOutcome::ok(format!(
                    "試運転（起動中）: 版: v{} / workflow_id: {}\n同じ版・同じ入力の試運転を、いま起動しているところです（新しく\
                     起動していません）。少し後に同じ引数でもう一度呼ぶと結果を返します（{STALE_RESERVATION_SECS} 秒たっても\
                     起動しなければ、新しく起動し直します）。",
                    key.version, key.workflow_id
                ))));
            }
            self.attach(ctx, &prev).await;
        }
        let origin = match state {
            // 実行履歴の GC で消えた run は、行が無いものとして扱う。
            None => return Ok(None),
            Some(s) if !s.status.is_terminal() => Origin::Joined,
            Some(_) if rerun => return Ok(None),
            Some(_) => Origin::Previous,
        };
        Ok(Some(
            self.follow(ctx, ir, &prev, key.workflow_id, key.version, run_id, origin)
                .await,
        ))
    }

    /// 起動の途中のまま古くなった行を片付ける。予約で決めた id の run があれば起動したものとして付け直し（消すと同じ
    /// 入力で 2 本目を立てる）、無ければ古い予約として消す。
    async fn settle_stale(&self, ctx: &AuthContext, thread_id: Uuid) -> Result<(), ToolError> {
        let stale = self
            .deps
            .chat
            .stale_trial_runs(ctx, thread_id)
            .await
            .map_err(unavailable)?;
        if stale.is_empty() {
            return Ok(());
        }
        let planned: Vec<Uuid> = stale.iter().map(|r| r.planned_run_id).collect();
        let started = self
            .outcomes
            .live_states(ctx, &planned)
            .await
            .map_err(unavailable)?;
        for row in &stale {
            if started.contains_key(&row.planned_run_id) {
                self.attach(ctx, row).await;
            } else {
                tracing::warn!(%thread_id, seq = row.seq, "起動の途中のまま古くなった試運転の予約を消す");
                self.release(ctx, row).await;
            }
        }
        Ok(())
    }

    /// 起動した run（予約で決めた id）を台帳に書く。書けなくても、次の呼び出しが予約の id から付け直す。
    async fn attach(&self, ctx: &AuthContext, row: &TrialRow) {
        if let Err(e) = self.deps.chat.attach_trial_run(ctx, row).await {
            tracing::warn!(run_id = %row.planned_run_id, error = %e, "試運転の run を台帳に書けない");
        }
    }

    /// 起動した run を台帳に書き、許可を監査に残す（どちらも失敗しても run は動いているので、観測は返す）。
    #[allow(clippy::too_many_arguments)]
    async fn record_start(
        &self,
        ctx: &AuthContext,
        row: &TrialRow,
        workflow_id: Uuid,
        version: i64,
        run_id: Uuid,
        hash: &str,
        trace_id: Option<&str>,
    ) {
        if run_id == row.planned_run_id {
            self.attach(ctx, row).await;
        } else {
            // 起動の入口は渡した id で作るので、ここへは来ない（来たら台帳に載せず、監査には残す）。
            tracing::error!(%run_id, planned = %row.planned_run_id, "試運転の run が予約の id と違う");
        }
        let metadata = json!({
            "chat_run_id": self.scope.chat_run_id.to_string(),
            "thread_id": self.scope.thread_id.to_string(),
            "version": version,
            "run_id": run_id.to_string(),
            "input_hash": hash,
        });
        if let Err(e) = self
            .deps
            .chat
            .record_try_audit(ctx, workflow_id, trace_id, metadata)
            .await
        {
            tracing::error!(%run_id, error = %e, "試運転の許可を監査に残せない");
        }
    }

    /// 起動しなかった行を消す（消せなくても古い予約として後で消える）。
    async fn release(&self, ctx: &AuthContext, row: &TrialRow) {
        if let Err(e) = self.deps.chat.release_trial_run(ctx, row).await {
            tracing::warn!(error = %e, "起動しなかった試運転の予約を消せない");
        }
    }

    /// run を待ち（前回の結果は待たない）、要約と費用を読んで観測にする。共有の相手の有無は、待ち終えて要約と費用を
    /// 読んだ後（文を組む直前）に判定する（待つ間の共有も拾う）。費用はどちらでも会話の予算に積む。
    #[allow(clippy::too_many_arguments)]
    async fn follow(
        &self,
        ctx: &AuthContext,
        ir: &WorkflowIr,
        row: &TrialRow,
        workflow_id: Uuid,
        version: i64,
        run_id: Uuid,
        origin: Origin,
    ) -> ToolOutcome {
        let end = if origin == Origin::Previous {
            None
        } else {
            let waiter = Waiter {
                outcomes: &self.outcomes,
                slot: &self.scope.slot,
                cancel: &self.scope.cancel,
                limits: self.scope.wait,
            };
            Some(waiter.wait(ctx, run_id).await.0)
        };
        let digest = match self.deps.runs.run_digest(ctx, run_id, ir).await {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(%run_id, error = %e, "試運転の run の要約を読めない");
                None
            }
        };
        let (cost, usage) = self.cost(ctx, row, run_id, digest.as_ref()).await;
        let observed = Observed {
            origin,
            end,
            max_wait: self.scope.wait.max_wait,
            workflow_id,
            version,
            run_id,
            ir,
            digest: digest.as_ref(),
            cost,
        };
        let content = if self.is_shared(ctx).await {
            shared::render(&SharedFacts::from_observed(&observed))
        } else {
            observe::render(&observed)
        };
        let mut out = ToolOutcome::ok(content);
        out.usage = usage;
        out
    }

    /// run の LLM の費用の合計と、まだ会話の予算に積んでいない差分（[`ToolUsage`]）。
    async fn cost(
        &self,
        ctx: &AuthContext,
        row: &TrialRow,
        run_id: Uuid,
        digest: Option<&RunDigest>,
    ) -> (RunCost, Option<ToolUsage>) {
        // 予約は run を作る前なので、予約の時刻から数えれば run の分はすべて入る（trace は run ごとに一意）。終わった
        // run は終わった時刻の少し後までで閉じる（前回の結果を何日も後に返すときに、その間の組織の利用量を読まない）。
        let since = row.created_at - chrono::Duration::minutes(1);
        let until = digest
            .filter(|d| d.status.is_terminal())
            .and_then(|d| d.finished_at)
            .map(|at| at + USAGE_TAIL);
        let total = match self
            .usage
            .trace_usage(ctx, &run_id.simple().to_string(), since, until)
            .await
        {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(%run_id, error = %e, "試運転の run の費用を読めない");
                return (RunCost::default(), None);
            }
        };
        let cost = RunCost {
            tokens: total.prompt_tokens.saturating_add(total.completion_tokens),
            cost_usd_micros: total.cost_usd_micros,
        };
        let (tokens, micros) = match self
            .deps
            .chat
            .report_trial_usage(ctx, row, cost.tokens, cost.cost_usd_micros)
            .await
        {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(%run_id, error = %e, "試運転の費用の報告を台帳に書けない（積まない）");
                return (cost, None);
            }
        };
        let usage = (tokens > 0 || micros > 0).then(|| {
            let tokens = u64::try_from(tokens).unwrap_or(0);
            ToolUsage {
                tokens,
                fresh_tokens: tokens,
                cost_usd_micros: micros,
            }
        });
        (cost, usage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 入力の hash はキーの順に依らず、値・きっかけ・予定の日時が違えば変わる。
    #[test]
    fn input_hash_ignores_key_order() {
        let a = input_hash(
            &json!({ "a": 1, "b": { "x": [1, 2], "y": "z" } }),
            None,
            None,
        );
        let b = input_hash(
            &json!({ "b": { "y": "z", "x": [1, 2] }, "a": 1 }),
            None,
            None,
        );
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert_ne!(a, input_hash(&json!({ "a": 2 }), None, None));
        assert_ne!(
            a,
            input_hash(
                &json!({ "a": 1, "b": { "x": [2, 1], "y": "z" } }),
                None,
                None
            )
        );
        let t = json!({ "record": { "id": "r" } });
        let base = input_hash(&json!({}), None, None);
        assert_ne!(base, input_hash(&json!({}), Some(&t), None));
        let at = chrono::DateTime::parse_from_rfc3339("2026-11-02T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_ne!(base, input_hash(&json!({}), None, Some(at)));
    }

    /// 入力の読み方: version は必須（数・数の文字列）、input と trigger は object か JSON の文字列、予定の日時は読める
    /// ものだけ。
    #[test]
    fn request_parsing() {
        let ok = TryRequest::parse(json!({
            "workflow_id": " 3f1b6a4e-0000-4000-8000-000000000001 ",
            "version": "v2",
            "input": "{\"title\":\"【試運転】x\"}",
            "trigger": { "record": { "id": "r" } },
            "scheduled_at": "2026-11-09T09:00",
            "rerun": true
        }))
        .unwrap();
        assert_eq!(ok.workflow, "3f1b6a4e-0000-4000-8000-000000000001");
        assert_eq!(ok.version, 2);
        assert_eq!(ok.input, json!({ "title": "【試運転】x" }));
        assert!(ok.trigger.is_some() && ok.scheduled_at.is_some() && ok.rerun);
        // 整数の小数（2.0）は整数として受ける（承認カードの JS は 2 と読む）。
        let float = TryRequest::parse(json!({ "workflow_id": "x", "version": 2.0 })).unwrap();
        assert_eq!(float.version, 2);
        let bare = TryRequest::parse(json!({ "workflow_id": "x", "version": 1 })).unwrap();
        assert_eq!(bare.input, json!({}));
        assert!(bare.trigger.is_none() && bare.scheduled_at.is_none() && !bare.rerun);
        for bad in [
            json!({ "workflow_id": "x" }),
            json!({ "workflow_id": "x", "version": 0 }),
            json!({ "workflow_id": "x", "version": 0.0 }),
            json!({ "workflow_id": "x", "version": 2.5 }),
            json!({ "workflow_id": "x", "version": -1.0 }),
            json!({ "workflow_id": "x", "version": 1.0e300 }),
            json!({ "workflow_id": "x", "version": "最新" }),
            json!({ "version": 1 }),
            json!({ "workflow_id": "x", "version": 1, "input": [1] }),
            json!({ "workflow_id": "x", "version": 1, "trigger": "1" }),
            json!({ "workflow_id": "x", "version": 1, "scheduled_at": "あした" }),
            json!({ "workflow_id": "x", "version": 1, "scheduled_at": 3 }),
        ] {
            assert!(
                matches!(TryRequest::parse(bad.clone()), Err(ToolError::Invalid(_))),
                "{bad}"
            );
        }
    }

    /// 内部の誤りは中身を出さず、固定の文にする（Debug の文に作成者の文字が入りうる）。
    #[test]
    fn unavailable_hides_the_detail() {
        let err = unavailable("db: secret detail");
        let ToolError::Unavailable(text) = err else {
            panic!("{err:?}");
        };
        assert_eq!(text, UNAVAILABLE);
        assert!(!text.contains("secret"), "{text}");
    }

    /// 版の IR を読めなかったときの写し方。保存済みの定義を読めないのは「何度呼んでも同じ」の文で is_error になり、
    /// 誤りの文（作成者の文字を含みうる）は出さない。見つからない・見えないは「見つからない」、ほかは固定の内部の誤り。
    #[test]
    fn version_read_errors_are_mapped() {
        let wf = Uuid::from_u128(7);
        let invalid = WorkflowStoreError::Validation(vec![workflow_engine::ValidationError::new(
            "ir.schema_violation",
            "保存済み IR のパースに失敗しました: SECRET-author-text",
        )]);
        let out = version_read_error(invalid, &wf.to_string(), wf, 4).unwrap();
        assert!(out.is_error);
        assert!(
            out.content
                .starts_with("v4 の保存済みの定義を読み込めません（何度呼んでも同じです）"),
            "{}",
            out.content
        );
        assert!(
            !out.content.contains("SECRET") && !out.content.contains("一時的"),
            "{}",
            out.content
        );
        for e in [
            artifact::ArtifactError::NotFound,
            artifact::ArtifactError::Forbidden,
        ] {
            let nf = version_read_error(WorkflowStoreError::Artifact(e), &wf.to_string(), wf, 4)
                .unwrap();
            assert!(
                nf.is_error && nf.content.contains("見つからない"),
                "{}",
                nf.content
            );
        }
        let other = version_read_error(
            WorkflowStoreError::Artifact(artifact::ArtifactError::Internal(
                "db: secret detail".into(),
            )),
            &wf.to_string(),
            wf,
            4,
        )
        .unwrap_err();
        assert!(
            matches!(&other, ToolError::Unavailable(t) if t == UNAVAILABLE),
            "{other:?}"
        );
        assert!(!format!("{other}").contains("secret"), "{other}");
    }
}

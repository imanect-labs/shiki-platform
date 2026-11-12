//! 具体 `RunLauncher`（委譲チェック＋IR 取得＋run 作成を束ねる・Task 10.4a/10.3 結線・engine.md §6.2）。
//!
//! schedule/event トリガの発火（[`scheduler`](crate::scheduler)）と interactive 起動 API から呼ばれる。
//! run 開始前に **委譲チェック**（registration enabled・委譲有効・declared ⊆ consented）を通し、
//! 不成立なら run を作らない（fail-closed）。schedule/event は workflow プリンシパルで実行する。

use std::sync::Arc;

use async_trait::async_trait;
use authz::{AuthContext, Relation};
use serde_json::Value;
use storage::audit::{AuditAction, AuditEntry, AuditRecorder, Decision};
use uuid::Uuid;

use super::graph::RunGraph;
use super::launch_skip::{self, SkipCause, SkipWarnings};
use super::store::{RunOrigin, RunStore};
use crate::delegation::{DelegationStore, RunAdmission};
use crate::scheduler::{LaunchOutcome, RunLauncher, SkipReason};
use crate::store::WorkflowStore;
use crate::WorkflowIr;

/// 手動の実行で試す値（実行のダイアログ・実行履歴の「同じ入力でもう一度実行」）。どれも省けば今までどおり。
///
/// 利用者が書いた値で、入力と同じく本人の権限で使う（本人が読めないものには触れない）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunTrial<'a> {
    /// 試すきっかけの内容（`$from trigger` の源・D-93）。`None` なら、きっかけの内容は入力と同じ。
    pub trigger: Option<&'a Value>,
    /// 試す予定の日時（`$from run` の `scheduled_at` の源・GAP-88）。`None` なら、予定の日時は始めた日時と同じ。
    pub scheduled_at: Option<chrono::DateTime<chrono::Utc>>,
    /// やり直しの元の run（「同じ入力でもう一度実行」だけが渡す）。実行履歴は、元の run から引き継いだ予定の日時を、
    /// 実行のダイアログで指定した日時と言い分ける（GAP-89）。`None` なら今までどおり。
    pub rerun_of: Option<Uuid>,
}

impl<'a> RunTrial<'a> {
    /// run の由来（試す値だけを載せる・親の run やイベントは無い）。
    fn origin(self) -> RunOrigin<'a> {
        RunOrigin {
            trigger_payload: self.trigger,
            scheduled_at: self.scheduled_at,
            rerun_of: self.rerun_of,
            ..RunOrigin::default()
        }
    }
}

/// 委譲チェック付き run 起動。DelegationStore + WorkflowStore + RunStore を束ねる。
///
/// schedule/event では run の org は **registration.org** を使う（enable 時に固定した org）。
#[derive(Clone)]
pub struct WorkflowRunLauncher {
    delegation: DelegationStore,
    workflows: WorkflowStore,
    runs: RunStore,
    /// 見送りを警告した自動実行（理由・テナント・ワークフロー・有効化した版）。発火のたびに警告しない（BUG-146・BUG-51・
    /// BUG-170）。
    skip_warnings: Arc<SkipWarnings>,
}

impl WorkflowRunLauncher {
    pub fn new(delegation: DelegationStore, workflows: WorkflowStore, runs: RunStore) -> Self {
        WorkflowRunLauncher {
            delegation,
            workflows,
            runs,
            skip_warnings: Arc::default(),
        }
    }

    /// 自動実行の起動を見送る（再試行しない）。警告は (理由・テナント・ワークフロー・有効化した版) ごとにプロセスで
    /// 1 回（2 回目からは debug）。`reason` は詳しい理由（ログと戻り値の文・作成者には見せない）。
    ///
    /// 見送ったことを、理由の code と作成者向けの文で記録する（ワークフローの画面が出す・GAP-67）。記録できなくても
    /// 見送りは変えない（一時障害として再試行に回すと、同じ見送りを試す回数だけ繰り返す）。
    async fn skip(
        &self,
        cause: SkipCause,
        tenant_id: &str,
        workflow_id: Uuid,
        version: i64,
        reason: String,
    ) -> LaunchOutcome {
        self.skip_warnings
            .warn(cause, tenant_id, workflow_id, version, &reason);
        if let Err(e) =
            launch_skip::record(self.runs.db(), tenant_id, workflow_id, version, cause).await
        {
            tracing::warn!(tenant_id, %workflow_id, version, code = cause.code(), error = %e, "自動実行の見送りを記録できない");
        }
        LaunchOutcome::Skipped(SkipReason::DelegationInvalid(reason))
    }

    /// interactive 起動（本人の AuthContext で・委譲チェックは対話では不要＝本人権限で実行）。
    ///
    /// 本人が読める IR を本人権限で実行するため confused-deputy にならない（engine.md §6.1）。
    pub async fn start_interactive(
        &self,
        ctx: &AuthContext,
        workflow_id: Uuid,
        input: &Value,
    ) -> Result<Option<Uuid>, LauncherError> {
        self.start_interactive_trial(ctx, workflow_id, input, RunTrial::default())
            .await
    }

    /// interactive 起動で、試す値を渡す（実行のダイアログの「きっかけの内容」・D-93 と「予定の日時」・GAP-88）。
    ///
    /// スケジュールやファイル・テーブルの変更をきっかけにするワークフローを、手動で試すためのもの（[`RunTrial`]）。
    /// 試す値を省けば [`Self::start_interactive`] と同じ。
    pub async fn start_interactive_trial(
        &self,
        ctx: &AuthContext,
        workflow_id: Uuid,
        input: &Value,
        trial: RunTrial<'_>,
    ) -> Result<Option<Uuid>, LauncherError> {
        let (version, ir) = self
            .workflows
            .get_latest(ctx, workflow_id, None)
            .await
            .map_err(|e| LauncherError::Ir(format!("{e:?}")))?;
        let origin = trial.origin();
        let blockers = crate::run_blockers(&ir);
        self.create_interactive_run(ctx, workflow_id, version, &ir, input, origin, blockers)
            .await
    }

    /// run の中から子のワークフローを起動する（workflow.start・`Shiki.workflow.*`）。
    ///
    /// 権限・入力の検査は [`Self::start_interactive`] と同じ。子の run は親の run の因果の鎖
    /// （親の鎖 ∪ {親のワークフロー}）を引き継ぐ。子の書込が起こすイベントで親（祖先）のトリガが
    /// 起きる循環を、子が別の run でも見つけられるようにする（D-26）。親の予定の日時も引き継ぐ（スケジュールの
    /// run から始めた子が、同じ週・月を数える・GAP-90）。どちらも run を作る Tx で親の行から読む（[`RunOrigin`]）。
    pub async fn start_child(
        &self,
        ctx: &AuthContext,
        workflow_id: Uuid,
        input: &Value,
        parent_run_id: Option<Uuid>,
    ) -> Result<Option<Uuid>, LauncherError> {
        // 相手が無い・見えないときは、やり直さない失敗に分けられるよう NotVisible で返す（BUG-107）。
        let (version, ir) = self
            .workflows
            .get_latest(ctx, workflow_id, None)
            .await
            .map_err(|e| match e {
                crate::store::WorkflowStoreError::Artifact(
                    artifact::ArtifactError::NotFound | artifact::ArtifactError::Forbidden,
                ) => LauncherError::NotVisible,
                other => LauncherError::Ir(format!("{other:?}")),
            })?;
        let origin = RunOrigin {
            parent_run_id,
            ..RunOrigin::default()
        };
        // 子の run の結果（workflow.reply）はどの画面にも出ないので、返す値の空では止めない（BUG-168）。
        let blockers = crate::unattended_run_blockers(&ir);
        self.create_interactive_run(ctx, workflow_id, version, &ir, input, origin, blockers)
            .await
    }

    /// interactive 系共通の run 作成（IR 取得＝認可は呼び出し側で済んでいる前提）。
    ///
    /// `blockers` は呼び出し側が組んだ実行の前に止める理由（ボタン・会話・画面からの起動は [`crate::run_blockers`]、
    /// 子の実行は結果が画面に出ないので [`crate::unattended_run_blockers`]・BUG-168）。
    #[allow(clippy::too_many_arguments)] // 主体・ワークフロー・版・IR・入力・由来・止める理由は全て別物。
    async fn create_interactive_run(
        &self,
        ctx: &AuthContext,
        workflow_id: Uuid,
        version: i64,
        ir: &WorkflowIr,
        input: &Value,
        origin: RunOrigin<'_>,
        blockers: Vec<crate::ValidationError>,
    ) -> Result<Option<Uuid>, LauncherError> {
        // 入力の契約（input_schema）は run を作る前に必ず確かめる。対話起動の全経路
        // （API・会話の図のボタン・ミニアプリ）がここを通るので、画面の検証に頼らない。
        // 保存は通しても空のまま動かせない欄（必須の文字の欄が空・D-43）は、入力より先に断る。
        if !blockers.is_empty() {
            return Err(LauncherError::NotReady(blockers));
        }
        if let Some(schema) = &ir.input_schema {
            crate::ir::input_schema::validate_input_issues(schema, input)
                .map_err(LauncherError::Input)?;
        }
        let ir_json = serde_json::to_value(ir).map_err(|e| LauncherError::Ir(e.to_string()))?;
        let graph = RunGraph::build(ir);
        // principal_kind は呼び出し主体の種別に従う（ユーザー起動＝user・workflow.start＝workflow）。
        let principal_kind = run_principal_kind(ctx.principal().kind)?;
        let run_id = self
            .runs
            .create_run_from(
                ctx.tenant_id(),
                ctx.org(),
                workflow_id,
                version,
                "interactive",
                None,
                &ctx.principal().id,
                principal_kind,
                input,
                &ir_json,
                &graph,
                origin,
            )
            .await
            .map_err(|e| LauncherError::Run(e.to_string()))?;
        // None = policies.on_trigger_overflow=skip による多重度あふれ（run 不作成・正直に None）。
        Ok(run_id)
    }

    /// interactive 起動の**バージョンピン版**（generative UI / ミニアプリのアクション束縛・Task 6.5）。
    ///
    /// 検証時にピンした版を実行する（再現性）。認可は [`Self::start_interactive`] と同じく
    /// 本人の viewer 権限で IR を取得し、ノード実行時は scope_ceiling ∩ 本人 ReBAC の二重ゲート。
    pub async fn start_interactive_version(
        &self,
        ctx: &AuthContext,
        workflow_id: Uuid,
        version: i64,
        input: &Value,
    ) -> Result<Option<Uuid>, LauncherError> {
        self.start_interactive_version_trial(ctx, workflow_id, version, input, RunTrial::default())
            .await
    }

    /// [`Self::start_interactive_version`] で、試す値も渡す（実行履歴の「新しく実行し直す」が、元の run で試した
    /// きっかけの内容と予定の日時を引き継ぐ・D-93・GAP-88）。試す値を省けば今までどおり。
    pub async fn start_interactive_version_trial(
        &self,
        ctx: &AuthContext,
        workflow_id: Uuid,
        version: i64,
        input: &Value,
        trial: RunTrial<'_>,
    ) -> Result<Option<Uuid>, LauncherError> {
        let (version, ir) = self
            .workflows
            .get_version(ctx, workflow_id, version, None)
            .await
            .map_err(|e| LauncherError::Ir(format!("{e:?}")))?;
        let origin = trial.origin();
        let blockers = crate::run_blockers(&ir);
        self.create_interactive_run(ctx, workflow_id, version, &ir, input, origin, blockers)
            .await
    }

    /// 版を固定して、ワークフローの editor 以上の人だけが起動する（会話の AI の試運転・D-197）。
    ///
    /// 承認カードで見せた版と実行する版を一致させるため、版は呼び出し側が必ず渡す（最新版を引かない・R5）。順は次のとおり。
    ///
    /// 1. 本人の viewer 権限でワークフローを引き（ArtifactStore の `get`）、版の IR を読む（[`WorkflowStore::get_version`]）。
    ///    見えない・無い・ワークフローでない（汎用の artifact の口で作った別の種類・保存の検証を通っていない）・版が無いは
    ///    どれも [`LauncherError::Ir`]（呼び出し側は存在を秘匿して「見つからない」に写す）。種類を見るのは入口が pub で、
    ///    道具の `resolve_ref` を通らない呼び出し側でも同じに断るため。
    /// 2. editor の relation を確かめる（ArtifactStore の `require_relation`・HigherConsistency）。足りなければ
    ///    `workflow.try_run` の deny を監査に残し、[`LauncherError::ReadOnly`]。viewer で読めた後なので、閲覧のみと
    ///    答えても存在は漏れない。判定は既存の Editor の relation だけで、OpenFGA の schema は変えない。
    /// 3. 実行の前に止める設定（[`crate::run_blockers`]）→ 入力の契約 → run の作成（[`Self::start_interactive`] と同じ
    ///    入口の検証の順・run の主体は本人のまま）。
    ///
    /// 許可の監査はここでは書かない（承認を通した呼び出し側が [`record_try_run_allowed`] で、拒否と同じ対象に書く）。
    /// 名前に trial を使わないのは、[`RunTrial`]（手動の実行で試す値）と紛れるため。
    pub async fn start_version_as_editor(
        &self,
        ctx: &AuthContext,
        workflow_id: Uuid,
        version: i64,
        input: &Value,
        trial: RunTrial<'_>,
        trace_id: Option<&str>,
    ) -> Result<Option<Uuid>, LauncherError> {
        // 種類を確かめる（無い・見えないは get が NotFound・Forbidden で返す。どれも Ir）。
        let meta = self
            .workflows
            .artifacts
            .get(ctx, workflow_id, trace_id)
            .await
            .map_err(|e| LauncherError::Ir(format!("{e:?}")))?;
        if meta.kind != artifact::ArtifactKind::Workflow {
            return Err(LauncherError::Ir("not found".into()));
        }
        let (version, ir) = self
            .workflows
            .get_version(ctx, workflow_id, version, trace_id)
            .await
            .map_err(|e| LauncherError::Ir(format!("{e:?}")))?;
        self.workflows
            .artifacts
            .require_relation(
                ctx,
                workflow_id,
                Relation::Editor,
                AuditAction::WorkflowTryRun,
                trace_id,
            )
            .await
            .map_err(|e| match e {
                artifact::ArtifactError::Forbidden => LauncherError::ReadOnly,
                other => LauncherError::Ir(format!("{other:?}")),
            })?;
        let origin = trial.origin();
        let blockers = crate::run_blockers(&ir);
        self.create_interactive_run(ctx, workflow_id, version, &ir, input, origin, blockers)
            .await
    }

    /// interactive 起動の**バンドル権限版**（ミニアプリのワークフロー束縛・Task 6.10）。
    ///
    /// ミニアプリ本体（バンドル）だけを共有された利用者でも、部品 workflow を個別共有される
    /// ことなくピン版を起動できる。IR の読取のみバンドル viewer で認可し
    /// （`artifact.read_via_bundle` 監査）、**実行主体は押した本人のまま** — run 内の
    /// データアクセスは本人 ReBAC ∩ 宣言スコープ ∩ ノード設定の二重ゲート（engine.md §6.1）。
    pub async fn start_interactive_via_bundle(
        &self,
        ctx: &AuthContext,
        bundle_id: Uuid,
        workflow_id: Uuid,
        version: i64,
        input: &Value,
    ) -> Result<Option<Uuid>, LauncherError> {
        let (version, ir) = self
            .workflows
            .get_version_via_bundle(ctx, bundle_id, workflow_id, version, None)
            .await
            .map_err(|e| LauncherError::Ir(format!("{e:?}")))?;
        let blockers = crate::run_blockers(&ir);
        let origin = RunOrigin::default();
        self.create_interactive_run(ctx, workflow_id, version, &ir, input, origin, blockers)
            .await
    }

    /// schedule/event の run を起動する（委譲チェック→workflow プリンシパルで create_run）。
    ///
    /// `Err` は一時的な失敗（IR・DB・OpenFGA）。起動しないと決まったものは `Ok(Skipped)`。
    async fn launch_delegated(
        &self,
        tenant_id: &str,
        workflow_id: Uuid,
        trigger_kind: &str,
        trigger_id: &str,
        payload: &Value,
        event_id: Option<i64>,
    ) -> Result<LaunchOutcome, LauncherError> {
        // registration から **有効化バージョンと org** を取る（enable 時に固定した版・org で実行する）。
        // 未登録/未有効化なら run を作らない。
        let Some((org, enabled_version)) = self
            .delegation
            .registration_info(tenant_id, workflow_id)
            .await
            .map_err(|e| LauncherError::Delegation(format!("{e:?}")))?
        else {
            return Ok(LaunchOutcome::Skipped(SkipReason::NotEnabled));
        };

        // workflow プリンシパルの AuthContext（registration の org）で **enabled_version の IR** を読む。
        // 最新版でなく有効化した版を実行し、未同意の新版が schedule/event で走るのを防ぐ。
        let tenant = authz::TenantId::parse(tenant_id)
            .map_err(|e| LauncherError::Run(format!("tenant_id が不正です: {e}")))?;
        let wf_ctx = AuthContext::for_workflow(tenant, org.clone(), &workflow_id.to_string());
        let (version, ir) = match self
            .workflows
            .get_version(&wf_ctx, workflow_id, enabled_version, None)
            .await
        {
            Ok(found) => found,
            // ワークフロー（か有効化した版）が消えた。何度起こしても同じなので、一時障害として再試行しない
            // （消したワークフローへの発火を、試す回数を使い切るまで繰り返さない・BUG-128）。ふつうは削除のトリガが
            // 自動実行をオフにしているので、ここへ来るのは削除と発火が重なったときだけ。
            Err(crate::store::WorkflowStoreError::Artifact(artifact::ArtifactError::NotFound)) => {
                tracing::warn!(
                    tenant_id,
                    %workflow_id,
                    "ワークフローの定義が見つからないため run を開始しません"
                );
                return Ok(LaunchOutcome::Skipped(SkipReason::NotEnabled));
            }
            // 自動実行の主体（workflow プリンシパル）が、自分の定義を読めない（登録と認可の食い違い。HigherConsistency
            // の判定で、OpenFGA の障害は Forbidden にならない）。何度起こしても同じなので、一時障害として再試行しない。
            // 警告は版ごとに 1 回（発火のたびに出さない・BUG-146）。きっかけは止めない（オンにし直せば直る）。
            Err(crate::store::WorkflowStoreError::Artifact(artifact::ArtifactError::Forbidden)) => {
                return Ok(self
                    .skip(
                        SkipCause::DefinitionUnreadable,
                        tenant_id,
                        workflow_id,
                        enabled_version,
                        "自動実行の主体がワークフローの定義を読めません".to_string(),
                    )
                    .await);
            }
            Err(e) => return Err(LauncherError::Ir(format!("{e:?}"))),
        };

        // 委譲チェック（fail-closed・不成立なら run を作らない）。
        let declared = ir.declared_scopes.clone();
        match self
            .delegation
            .check_run_start(tenant_id, workflow_id, &declared)
            .await
            .map_err(|e| LauncherError::Delegation(format!("{e:?}")))?
        {
            RunAdmission::Ok => {}
            RunAdmission::DelegationInvalid(reason) => {
                // 委譲失効・未同意スコープ・未登録などの切り分けができるよう理由を残す（警告は版ごとに 1 回・BUG-170）。
                return Ok(self
                    .skip(
                        SkipCause::DelegationInvalid,
                        tenant_id,
                        workflow_id,
                        enabled_version,
                        reason,
                    )
                    .await);
            }
        }

        // 保存は通しても空のまま動かせない欄（必須の欄が空・中にブロックの無い繰り返し・D-43・D-83・D-85）のある版は、
        // 自動実行でも起動しない。有効化の前の確認より前に有効化した版や、確認の欄が後から増えた版でも、空のまま
        // 動かさない（BUG-51）。何度起こしても同じなので見送り（BUG-146 の定義を読めない見送りと同じ形）、警告は版ごと
        // に 1 回。きっかけは止めない（欄を埋めて自動実行をオンにし直せば直る）。結果を画面に返すの返す値の空では
        // 止めない（自動実行の結果はどの画面にも出ない・前から動いていた自動実行を止めない・BUG-168）。
        let blockers = crate::unattended_run_blockers(&ir);
        if let Some(first) = blockers.first() {
            let reason = format!(
                "実行の前に止める設定があります（{} 件）: {}",
                blockers.len(),
                first.message
            );
            return Ok(self
                .skip(
                    SkipCause::NotReady,
                    tenant_id,
                    workflow_id,
                    enabled_version,
                    reason,
                )
                .await);
        }

        let ir_json = serde_json::to_value(&ir).map_err(|e| LauncherError::Ir(e.to_string()))?;
        let graph = RunGraph::build(&ir);
        // event ペイロードを run 入力に載せる（$from trigger/$from input で参照可能・schedule は Null）。schedule の
        // 予定の時刻は、発火が起動の間だけ持たせたものを run の行に書く（実行情報の予定の日時・GAP-15）。
        let scheduled_at = if trigger_kind == "schedule" {
            crate::scheduler::current_scheduled_at()
        } else {
            None
        };
        let run_id = self
            .runs
            .create_run_from(
                tenant_id,
                &org,
                workflow_id,
                version,
                trigger_kind,
                Some(trigger_id),
                &workflow_id.to_string(),
                "workflow",
                payload,
                &ir_json,
                &graph,
                RunOrigin {
                    trigger_event_id: event_id,
                    scheduled_at,
                    ..RunOrigin::default()
                },
            )
            .await
            .map_err(|e| LauncherError::Run(e.to_string()))?;
        // None = policies.on_trigger_overflow=skip による多重度あふれ（再試行しても同じ）。
        let Some(run_id) = run_id else {
            return Ok(LaunchOutcome::Skipped(SkipReason::Overflow));
        };
        // 自動実行が動いたので、見送りの記録を消す（画面は見送り続けているものだけを出す・GAP-67）。消せなくても起動は
        // 済んでいる（次に起動できたときか、オンにし直したときに消える）。
        if let Err(e) = launch_skip::clear(self.runs.db(), tenant_id, workflow_id).await {
            tracing::warn!(tenant_id, %workflow_id, error = %e, "自動実行の見送りの記録を消せない");
        }
        Ok(LaunchOutcome::Started(run_id))
    }
}

#[async_trait]
impl RunLauncher for WorkflowRunLauncher {
    async fn launch(
        &self,
        tenant_id: &str,
        workflow_id: Uuid,
        trigger_kind: &str,
        trigger_id: &str,
        payload: &Value,
        event_id: Option<i64>,
    ) -> LaunchOutcome {
        match self
            .launch_delegated(
                tenant_id,
                workflow_id,
                trigger_kind,
                trigger_id,
                payload,
                event_id,
            )
            .await
        {
            Ok(outcome) => outcome,
            // 一時障害（IR 取得/DB/OpenFGA）。イベントの発火は呼び出し側が再試行する（trigger_firing の
            // pending・D-21）。委譲の不成立・多重度あふれは Skipped で返り、再試行しない。
            Err(e) => {
                tracing::warn!(error = %e, tenant = tenant_id, "run 起動に失敗");
                LaunchOutcome::Failed(e.to_string())
            }
        }
    }
}

/// 対話の run の実行主体の種類（`workflow_run.principal_kind`・ユーザーの起動は user・workflow.start は workflow）。
///
/// ミニアプリの主体（B2 のサービス・Task 9.6）が run を作る経路はいま無い。ミニアプリの関数は `Shiki.workflow.*` を
/// 持たず、API・会話の図のボタン・ミニアプリの画面は利用者の主体で起動する。ワーカーは user・workflow の主体しか
/// 組めず（`principal_kind` の check も同じ 2 つ・migration 0022）、ミニアプリの主体を user として組むと別人の権限に
/// なるので、届いても run を作らずに断る（BUG-145）。
fn run_principal_kind(kind: authz::PrincipalKind) -> Result<&'static str, LauncherError> {
    match kind {
        authz::PrincipalKind::Workflow => Ok("workflow"),
        authz::PrincipalKind::User => Ok("user"),
        authz::PrincipalKind::MiniApp => Err(LauncherError::UnsupportedPrincipal),
    }
}

/// Wrapper を `Arc<dyn RunLauncher>` として scheduler へ渡すためのヘルパ。
#[must_use]
pub fn into_dyn(launcher: WorkflowRunLauncher) -> Arc<dyn RunLauncher> {
    Arc::new(launcher)
}

#[derive(Debug, thiserror::Error)]
pub enum LauncherError {
    #[error("IR 取得エラー: {0}")]
    Ir(String),
    #[error("委譲チェックエラー: {0}")]
    Delegation(String),
    #[error("run 作成エラー: {0}")]
    Run(String),
    /// 入力が `input_schema` に合わない（呼び出し側は 400 に写す）。誤りは場所（項目のキー）と理由に分けて持つ
    /// （実行のダイアログが欄の下に出す・BUG-109）。文が要る呼び出し側は [`LauncherError::input_message`]。
    #[error("入力が正しくありません: {}", LauncherError::input_message(.0))]
    Input(Vec<crate::ir::input_schema::InputIssue>),
    /// IR が実行できる状態にない（必須の文字の欄が空・D-43）。理由は [`crate::run_blockers`]（子の実行は
    /// [`crate::unattended_run_blockers`]）の全件（呼び出し側は 400 に写し、欄に写せるよう node_id・path を保つ）。
    #[error("実行できない設定があります: {}", LauncherError::messages(.0))]
    NotReady(Vec<crate::ValidationError>),
    /// 子のワークフローが無いか、実行主体から見えない（[`WorkflowRunLauncher::start_child`] だけが返す・
    /// 無いと見えないを言い分けない・BUG-107）。何度やっても通らないので、呼び出し側はやり直さない。
    #[error("ワークフローが見つからないか、見る権限がありません")]
    NotVisible,
    /// 実行主体の種類が run を作れないもの（ミニアプリのサービスの主体・[`run_principal_kind`]・BUG-145）。
    #[error("この主体ではワークフローを実行できません")]
    UnsupportedPrincipal,
    /// 閲覧だけ共有されたワークフロー（editor 以上でない・[`WorkflowRunLauncher::start_version_as_editor`] だけが
    /// 返す・D-197）。会話の AI はこの文をそのまま返す。
    #[error("閲覧のみのワークフローは試運転できません。画面から実行してください")]
    ReadOnly,
}

impl LauncherError {
    /// 入力の誤りを「場所: 理由」の文にまとめる（構造を持てない呼び出し側・会話の図のボタン・workflow.start）。
    #[must_use]
    pub fn input_message(issues: &[crate::ir::input_schema::InputIssue]) -> String {
        issues
            .iter()
            .map(|i| i.describe("入力"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 理由の文言を 1 つにまとめる（構造を持てない呼び出し側・ログ用）。
    #[must_use]
    pub fn messages(errors: &[crate::ValidationError]) -> String {
        errors
            .iter()
            .map(|e| e.message.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// 試運転の監査の対象の種類（拒否は [`WorkflowRunLauncher::start_version_as_editor`] の中で ArtifactStore が、この
/// 種類とワークフローの id で書く）。許可の行も同じ対象で書き、監査ログの画面で対象を絞って突き合わせられるようにする。
pub const TRY_RUN_AUDIT_OBJECT_TYPE: &str = "artifact";

/// 試運転の監査の 1 行（[`record_try_run_allowed`] の中身・呼び出し側の Tx で `storage::audit::record_on` に渡すとき）。
///
/// `object_id` はワークフローの id の文字列（[`Uuid::to_string`]・拒否の行と同じ形）。`metadata` がオブジェクトなら、
/// 拒否の行と同じ `relation: "editor"` を足す（呼び出し側が書いた `relation` は上書きする）。
#[must_use]
pub fn try_run_audit_entry<'a>(
    object_id: &'a str,
    decision: Decision,
    trace_id: Option<&'a str>,
    metadata: Value,
) -> AuditEntry<'a> {
    let metadata = match metadata {
        Value::Object(mut m) => {
            m.insert("relation".into(), Value::from(Relation::Editor.as_str()));
            Value::Object(m)
        }
        Value::Null => serde_json::json!({ "relation": Relation::Editor.as_str() }),
        other => other,
    };
    AuditEntry {
        action: AuditAction::WorkflowTryRun,
        object_type: TRY_RUN_AUDIT_OBJECT_TYPE,
        object_id,
        decision,
        trace_id,
        metadata,
    }
}

/// 試運転の許可を監査に残す（承認を通して [`WorkflowRunLauncher::start_version_as_editor`] で起動した呼び出し側が呼ぶ・
/// D-197）。
///
/// 対象は拒否の行と同じ（[`TRY_RUN_AUDIT_OBJECT_TYPE`]・ワークフローの id）。会話の run・スレッド・版・試運転の run・
/// 入力のハッシュなどの相関（設計 §4.2）は、呼び出し側が `metadata` に入れる。
pub async fn record_try_run_allowed(
    audit: &AuditRecorder,
    ctx: &AuthContext,
    workflow_id: Uuid,
    trace_id: Option<&str>,
    metadata: Value,
) -> Result<(), storage::StorageError> {
    let object_id = workflow_id.to_string();
    audit
        .record(
            ctx,
            try_run_audit_entry(&object_id, Decision::Allow, trace_id, metadata),
        )
        .await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// ミニアプリの主体では run を作らない（`principal_kind` の check は user・workflow だけ・BUG-145）。
    #[test]
    fn only_users_and_workflows_run() {
        assert_eq!(
            run_principal_kind(authz::PrincipalKind::User).ok(),
            Some("user")
        );
        assert_eq!(
            run_principal_kind(authz::PrincipalKind::Workflow).ok(),
            Some("workflow")
        );
        assert!(matches!(
            run_principal_kind(authz::PrincipalKind::MiniApp),
            Err(LauncherError::UnsupportedPrincipal)
        ));
        // migration 0022 の check と同じ 2 つ（ワーカーの実行主体の組み方も同じ）。
        let check = include_str!("../../../../migrations/0022_workflow_principal_kind.sql");
        assert!(check.contains("check (principal_kind in ('user', 'workflow'))"));
    }

    /// 試運転の監査の行は、拒否（ArtifactStore の require）と同じ対象・同じ relation の項目で書く（D-197）。
    #[test]
    fn try_run_audit_entry_matches_the_deny_row() {
        let id = Uuid::new_v4().to_string();
        let meta = serde_json::json!({ "chat_run_id": "c", "relation": "viewer" });
        let e = try_run_audit_entry(&id, Decision::Allow, Some("t"), meta);
        assert_eq!(e.action, AuditAction::WorkflowTryRun);
        assert_eq!(e.object_type, "artifact");
        assert_eq!(e.object_id, id);
        assert_eq!(e.decision, Decision::Allow);
        assert_eq!(e.trace_id, Some("t"));
        assert_eq!(e.metadata["relation"], "editor");
        assert_eq!(e.metadata["chat_run_id"], "c");
        let bare = try_run_audit_entry(&id, Decision::Deny, None, Value::Null);
        assert_eq!(bare.metadata, serde_json::json!({ "relation": "editor" }));
        // オブジェクトでない metadata は変えない（呼び出し側の誤りを隠さない）。
        let odd = try_run_audit_entry(&id, Decision::Allow, None, serde_json::json!([1]));
        assert_eq!(odd.metadata, serde_json::json!([1]));
    }

    #[test]
    fn read_only_message_is_the_d197_wording() {
        assert_eq!(
            LauncherError::ReadOnly.to_string(),
            "閲覧のみのワークフローは試運転できません。画面から実行してください"
        );
    }
}

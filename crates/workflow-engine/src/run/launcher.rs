//! 具体 `RunLauncher`（委譲チェック＋IR 取得＋run 作成を束ねる・Task 10.4a/10.3 結線・engine.md §6.2）。
//!
//! schedule/event トリガの発火（[`scheduler`](crate::scheduler)）と interactive 起動 API から呼ばれる。
//! run 開始前に **委譲チェック**（registration enabled・委譲有効・declared ⊆ consented）を通し、
//! 不成立なら run を作らない（fail-closed）。schedule/event は workflow プリンシパルで実行する。

use std::sync::Arc;

use async_trait::async_trait;
use authz::AuthContext;
use serde_json::Value;
use uuid::Uuid;

use super::graph::RunGraph;
use super::store::{RunOrigin, RunStore};
use crate::delegation::{DelegationStore, RunAdmission};
use crate::scheduler::{LaunchOutcome, RunLauncher, SkipReason};
use crate::store::WorkflowStore;
use crate::WorkflowIr;

/// 委譲チェック付き run 起動。DelegationStore + WorkflowStore + RunStore を束ねる。
///
/// schedule/event では run の org は **registration.org** を使う（enable 時に固定した org）。
#[derive(Clone)]
pub struct WorkflowRunLauncher {
    delegation: DelegationStore,
    workflows: WorkflowStore,
    runs: RunStore,
    /// 定義を読めないことを警告した自動実行（テナント・ワークフロー・有効化した版）。発火のたびに警告しない（BUG-146）。
    unreadable_warned: Arc<WarnOnce>,
    /// 実行の前に止める設定があることを警告した自動実行（同じく版ごとに 1 回・BUG-51）。
    not_ready_warned: Arc<WarnOnce>,
}

/// 同じ相手への警告をプロセスで 1 回にする覚え（BUG-146）。覚える数が上限に届いたら忘れて、また 1 回ずつ警告する。
#[derive(Debug, Default)]
struct WarnOnce(std::sync::Mutex<std::collections::HashSet<(String, Uuid, i64)>>);

impl WarnOnce {
    /// 覚える数の上限。
    const MAX: usize = 10_000;

    /// はじめての相手なら覚えて `true`（警告する）。覚えていれば `false`。
    fn first(&self, tenant_id: &str, workflow_id: Uuid, version: i64) -> bool {
        self.0.lock().is_ok_and(|mut seen| {
            if seen.len() >= Self::MAX {
                seen.clear();
            }
            seen.insert((tenant_id.to_string(), workflow_id, version))
        })
    }
}

impl WorkflowRunLauncher {
    pub fn new(delegation: DelegationStore, workflows: WorkflowStore, runs: RunStore) -> Self {
        WorkflowRunLauncher {
            delegation,
            workflows,
            runs,
            unreadable_warned: Arc::default(),
            not_ready_warned: Arc::default(),
        }
    }

    /// 自動実行の版に実行の前に止める設定があることを、(テナント・ワークフロー・有効化した版) ごとにプロセスで 1 回だけ
    /// 警告する（BUG-51）。2 回目からは debug に落とす。
    fn warn_not_ready_once(&self, tenant_id: &str, workflow_id: Uuid, version: i64, reason: &str) {
        if self.not_ready_warned.first(tenant_id, workflow_id, version) {
            tracing::warn!(
                tenant_id,
                %workflow_id,
                version,
                reason,
                "自動実行の版に、実行の前に止める設定があるため run を開始しません（欄を埋めて自動実行をオンにし直すと\
                 直ります。以後この版の発火では警告しません）"
            );
        } else {
            tracing::debug!(tenant_id, %workflow_id, version, "実行の前に止める設定があるため run を開始しません");
        }
    }

    /// 自動実行で定義を読めないことを、(テナント・ワークフロー・有効化した版) ごとにプロセスで 1 回だけ警告する
    /// （BUG-146）。2 回目からは debug に落とす（監査の deny は artifact の側で毎回残る）。
    fn warn_unreadable_once(&self, tenant_id: &str, workflow_id: Uuid, version: i64) {
        if self
            .unreadable_warned
            .first(tenant_id, workflow_id, version)
        {
            tracing::warn!(
                tenant_id,
                %workflow_id,
                version,
                "自動実行の主体がワークフローの定義を読めないため run を開始しません（自動実行をオンにし直すと直ります。\
                 以後この版の発火では警告しません）"
            );
        } else {
            tracing::debug!(tenant_id, %workflow_id, version, "定義を読めないため run を開始しません");
        }
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
        self.start_interactive_trial(ctx, workflow_id, input, None)
            .await
    }

    /// interactive 起動で、きっかけの内容を入力と別に渡す（実行のダイアログの「きっかけの内容」・D-93）。
    ///
    /// スケジュールやファイル・テーブルの変更をきっかけにするワークフローを、手動で試すためのもの。きっかけの
    /// 内容は利用者が書いた値で、入力と同じく本人の権限で使う（本人が読めないものには触れない）。
    /// `trigger` が `None` なら [`Self::start_interactive`] と同じ（きっかけの内容は入力と同じ）。
    pub async fn start_interactive_trial(
        &self,
        ctx: &AuthContext,
        workflow_id: Uuid,
        input: &Value,
        trigger: Option<&Value>,
    ) -> Result<Option<Uuid>, LauncherError> {
        let (version, ir) = self
            .workflows
            .get_latest(ctx, workflow_id, None)
            .await
            .map_err(|e| LauncherError::Ir(format!("{e:?}")))?;
        let origin = RunOrigin {
            trigger_payload: trigger,
            ..RunOrigin::default()
        };
        self.create_interactive_run(ctx, workflow_id, version, &ir, input, origin)
            .await
    }

    /// run の中から子のワークフローを起動する（workflow.start・`Shiki.workflow.*`）。
    ///
    /// 権限・入力の検査は [`Self::start_interactive`] と同じ。子の run は親の run の因果の鎖
    /// （親の鎖 ∪ {親のワークフロー}）を引き継ぐ。子の書込が起こすイベントで親（祖先）のトリガが
    /// 起きる循環を、子が別の run でも見つけられるようにする（D-26）。
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
        self.create_interactive_run(ctx, workflow_id, version, &ir, input, origin)
            .await
    }

    /// interactive 系共通の run 作成（IR 取得＝認可は呼び出し側で済んでいる前提）。
    async fn create_interactive_run(
        &self,
        ctx: &AuthContext,
        workflow_id: Uuid,
        version: i64,
        ir: &WorkflowIr,
        input: &Value,
        origin: RunOrigin<'_>,
    ) -> Result<Option<Uuid>, LauncherError> {
        // 入力の契約（input_schema）は run を作る前に必ず確かめる。対話起動の全経路
        // （API・会話の図のボタン・ミニアプリ）がここを通るので、画面の検証に頼らない。
        // 保存は通しても空のまま動かせない欄（必須の文字の欄が空・D-43）は、入力より先に断る。
        let blockers = crate::run_blockers(ir);
        if !blockers.is_empty() {
            return Err(LauncherError::NotReady(blockers));
        }
        if let Some(schema) = &ir.input_schema {
            crate::ir::input_schema::validate_input_issues(schema, input)
                .map_err(LauncherError::Input)?;
        }
        let ir_json = serde_json::to_value(ir).map_err(|e| LauncherError::Ir(e.to_string()))?;
        let graph = RunGraph::build(ir);
        let principal_kind = run_principal_kind(ctx.principal.kind)?;
        let run_id = self
            .runs
            .create_run_from(
                &ctx.tenant_id,
                &ctx.org,
                workflow_id,
                version,
                "interactive",
                None,
                &ctx.principal.id,
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
        self.start_interactive_version_trial(ctx, workflow_id, version, input, None)
            .await
    }

    /// [`Self::start_interactive_version`] で、きっかけの内容も渡す（実行履歴の「新しく実行し直す」が、元の run で
    /// 試したきっかけの内容を引き継ぐ・D-93）。`trigger` が `None` なら、きっかけの内容は入力と同じ。
    pub async fn start_interactive_version_trial(
        &self,
        ctx: &AuthContext,
        workflow_id: Uuid,
        version: i64,
        input: &Value,
        trigger: Option<&Value>,
    ) -> Result<Option<Uuid>, LauncherError> {
        let (version, ir) = self
            .workflows
            .get_version(ctx, workflow_id, version, None)
            .await
            .map_err(|e| LauncherError::Ir(format!("{e:?}")))?;
        let origin = RunOrigin {
            trigger_payload: trigger,
            ..RunOrigin::default()
        };
        self.create_interactive_run(ctx, workflow_id, version, &ir, input, origin)
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
        self.create_interactive_run(ctx, workflow_id, version, &ir, input, RunOrigin::default())
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
        let wf_ctx =
            AuthContext::for_workflow(tenant_id.to_string(), org.clone(), &workflow_id.to_string());
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
                self.warn_unreadable_once(tenant_id, workflow_id, enabled_version);
                return Ok(LaunchOutcome::Skipped(SkipReason::DelegationInvalid(
                    "自動実行の主体がワークフローの定義を読めません".to_string(),
                )));
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
                // 委譲失効・未同意スコープ・未登録などの切り分けができるよう理由を残す。
                tracing::warn!(
                    tenant_id,
                    %workflow_id,
                    reason,
                    "委譲チェック不成立のため run を開始しません"
                );
                return Ok(LaunchOutcome::Skipped(SkipReason::DelegationInvalid(
                    reason.clone(),
                )));
            }
        }

        // 保存は通しても空のまま動かせない欄（必須の欄が空・中にブロックの無い繰り返し・D-43・D-83・D-85）のある版は、
        // 自動実行でも起動しない。有効化の前の確認より前に有効化した版や、確認の欄が後から増えた版でも、空のまま
        // 動かさない（BUG-51）。何度起こしても同じなので見送り（BUG-146 の定義を読めない見送りと同じ形）、警告は版ごと
        // に 1 回。きっかけは止めない（欄を埋めて自動実行をオンにし直せば直る）。
        let blockers = crate::run_blockers(&ir);
        if let Some(first) = blockers.first() {
            let reason = format!(
                "実行の前に止める設定があります（{} 件）: {}",
                blockers.len(),
                first.message
            );
            self.warn_not_ready_once(tenant_id, workflow_id, enabled_version, &reason);
            return Ok(LaunchOutcome::Skipped(SkipReason::DelegationInvalid(
                reason,
            )));
        }

        let ir_json = serde_json::to_value(&ir).map_err(|e| LauncherError::Ir(e.to_string()))?;
        let graph = RunGraph::build(&ir);
        // event ペイロードを run 入力に載せる（$from trigger/$from input で参照可能・schedule は Null）。
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
                    ..RunOrigin::default()
                },
            )
            .await
            .map_err(|e| LauncherError::Run(e.to_string()))?;
        // None = policies.on_trigger_overflow=skip による多重度あふれ（再試行しても同じ）。
        Ok(run_id.map_or(
            LaunchOutcome::Skipped(SkipReason::Overflow),
            LaunchOutcome::Started,
        ))
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
    /// IR が実行できる状態にない（必須の文字の欄が空・D-43）。理由は [`crate::run_blockers`] の全件
    /// （呼び出し側は 400 に写し、欄に写せるよう node_id・path を保つ）。
    #[error("実行できない設定があります: {}", LauncherError::messages(.0))]
    NotReady(Vec<crate::ValidationError>),
    /// 子のワークフローが無いか、実行主体から見えない（[`WorkflowRunLauncher::start_child`] だけが返す・
    /// 無いと見えないを言い分けない・BUG-107）。何度やっても通らないので、呼び出し側はやり直さない。
    #[error("ワークフローが見つからないか、見る権限がありません")]
    NotVisible,
    /// 実行主体の種類が run を作れないもの（ミニアプリのサービスの主体・[`run_principal_kind`]・BUG-145）。
    #[error("この主体ではワークフローを実行できません")]
    UnsupportedPrincipal,
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

    /// 定義を読めない自動実行の警告は、(テナント・ワークフロー・版) ごとに 1 回（BUG-146）。
    #[test]
    fn unreadable_definitions_are_warned_once_per_version() {
        let seen = WarnOnce::default();
        let wf = Uuid::new_v4();
        assert!(seen.first("t", wf, 1));
        assert!(
            !seen.first("t", wf, 1),
            "同じ版の 2 回目の発火では警告しない"
        );
        assert!(seen.first("t", wf, 2), "オンにし直した別の版は警告する");
        assert!(seen.first("u", wf, 1), "別のテナント");
        // 上限に届いたら忘れる（覚えが際限なく増えない）。
        for i in 0..WarnOnce::MAX {
            seen.first("x", Uuid::new_v4(), i64::try_from(i).unwrap_or(0));
        }
        assert!(seen.0.lock().unwrap().len() <= WarnOnce::MAX);
    }
}

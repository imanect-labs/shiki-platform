//! チャット生成ワーカー（Task 3.11）。jobq を消費し、claim した run を生成して確定する。
//!
//! - **専用レーン**: jobq の `chat_generation` キューのみ消費（ワークフロー/ingestion と同居させない）。
//! - **claim＋リース＋fencing**: [`ChatStore::claim_run`] で running 化し、ハートビートでリース延長。
//!   fencing 不一致の追記は拒否（クラッシュ takeover＋ゾンビ書込拒否）。
//! - **モード分岐**: agent_mode ON=agent-core ループ（doc_search 等）／OFF=古典 RAG 注入＋gateway 直。
//! - **AuthContext 伝播**: run に保存した発話ユーザーで生成し昇格しない（confused-deputy 防御）。
//! - **協調キャンセル**: ユーザー明示停止（cancel_requested）のみ。ページ離脱はキャンセルしない。

mod approval_policy;
/// 古典 RAG 注入経路（generate.rs から分割）。
mod classic;
/// ワーカーの設定と依存束（mod.rs から分割・500 行規約）。
mod config;
mod gate;
mod generate;
mod history;
/// 実行オプション/system プロンプト（generate.rs から分割）。
mod opts;
mod sink;
mod stream_map;
mod toolset;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use authz::{AuthContext, Principal};
use llm_gateway::LlmGateway;
use rag::SearchService;
use sqlx::PgPool;
use uuid::Uuid;

use crate::model::{ContentBlock, RunStatus, StreamEventKind};
use crate::store::{ChatStore, ClaimedRun, CHAT_GENERATION_QUEUE};
use crate::ChatError;
pub use config::{WorkerConfig, WorkerDeps};
use sink::WorkerSink;

/// 順番待ちのジョブを見直す間隔。先行 run の完了はイベントで通知されないため短くポーリングする
/// （`defer` は attempts を消費しないので、何度見直しても DLQ には影響しない）。
const QUEUE_WAIT_POLL: Duration = Duration::from_secs(2);

/// チャット生成ワーカー。複数タスクで並行消費できる（各タスクが claim ループを回す）。
#[derive(Clone)]
pub struct ChatWorker {
    db: PgPool,
    store: ChatStore,
    gateway: LlmGateway,
    search: Option<Arc<SearchService>>,
    /// サンドボックス（code_interpreter 用）。未配線なら code_interpreter ツールを提示しない。
    sandbox: Option<Arc<dyn agent_core::Sandbox>>,
    /// 成果物の保存先（code_interpreter が /workspace のファイルを保存する・Task 4.11）。
    artifacts: Option<Arc<dyn agent_core::ArtifactStore>>,
    /// web 検索プロバイダ。未配線なら web_search / web_fetch ツールを提示しない。
    web_search: Option<Arc<dyn websearch::SearchProvider>>,
    /// StorageService（自律プロファイルのワークスペース）。
    storage: Option<Arc<storage::StorageService>>,
    /// UI スペック検証（emit_ui ツール・Task 6.4）。
    ui_validator: Option<Arc<gui::SpecValidator>>,
    /// skill / ミニアプリのピン解決（Task 6.9）。
    skill_artifacts: Option<Arc<artifact::ArtifactStore>>,
    /// skill カタログ源（skill ツール・#344）。
    skill_catalog: Option<Arc<dyn crate::skill_catalog::SkillCatalogSource>>,
    /// ワークフロー IR ストア（emit_workflow / read_workflow・Task 10.13）。
    workflow_store: Option<Arc<workflow_engine::WorkflowStore>>,
    /// カタログ源（保存 API と同一実装を注入・Task 10.13）。
    workflow_catalog: Option<Arc<dyn crate::workflow_tool::WorkflowCatalogSource>>,
    /// ノート共同編集ハブ（document.edit / document.read・Task 11P.4）。
    collab: Option<Arc<collab::CollabHub>>,
    /// CSV クエリ/パッチサービス（Task 11P.9）。
    tabular: Option<Arc<tabular::TabularService>>,
    /// AI Office 編集（office.edit・Task 11.8）。
    office: Option<Arc<office::OfficeEditor>>,
    /// AI ライブ編集（office.live_edit・CoolWSD headless 参加・issue #352）。
    office_live: Option<Arc<office::live::LiveEditor>>,
    /// Office の新規作成（save_document / save_sheet・#381）。
    office_creator: Option<Arc<office::OfficeCreator>>,
    config: Arc<WorkerConfig>,
}

impl ChatWorker {
    pub fn new(db: PgPool, store: ChatStore, deps: WorkerDeps, config: WorkerConfig) -> Self {
        let WorkerDeps {
            gateway,
            search,
            sandbox,
            artifacts,
            web_search,
            storage,
            ui_validator,
            skill_artifacts,
            skill_catalog,
            workflow_store,
            workflow_catalog,
            collab,
            tabular,
            office,
            office_live,
            office_creator,
        } = deps;
        ChatWorker {
            db,
            store,
            gateway,
            search,
            sandbox,
            artifacts,
            web_search,
            storage,
            ui_validator,
            skill_artifacts,
            skill_catalog,
            workflow_store,
            workflow_catalog,
            collab,
            tabular,
            office,
            office_live,
            office_creator,
            config: Arc::new(config),
        }
    }

    /// `concurrency` 本のワーカータスクと sweeper を起動する。
    pub fn spawn(self, concurrency: usize) -> Vec<tokio::task::JoinHandle<()>> {
        let mut handles = Vec::new();
        for i in 0..concurrency.max(1) {
            let w = self.clone();
            handles.push(tokio::spawn(async move { w.run_loop(i).await }));
        }
        // 孤児回収 sweeper（backstop）。
        let sweeper = self.clone();
        handles.push(tokio::spawn(async move { sweeper.run_sweeper().await }));
        handles
    }

    /// jobq 消費ループ。
    async fn run_loop(self, worker_index: usize) {
        let worker_id = format!("chat-worker-{worker_index}");
        loop {
            match self.claim_and_process(&worker_id).await {
                Ok(true) => {}                                                     // 1 件処理した
                Ok(false) => tokio::time::sleep(Duration::from_millis(300)).await, // 空
                Err(e) => {
                    tracing::error!(error = %e, "chat worker loop error");
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        }
    }

    /// jobq から 1 件 claim して処理する。処理したら true。
    async fn claim_and_process(&self, worker_id: &str) -> Result<bool, ChatError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| ChatError::Internal(format!("acquire: {e}")))?;
        // 可視性タイムアウトは生成時間より長めに（クラッシュ時の再配信 backstop）。
        let jobs = jobq::claim(&mut conn, CHAT_GENERATION_QUEUE, Duration::from_mins(3), 1)
            .await
            .map_err(|e| ChatError::Internal(format!("jobq claim: {e}")))?;
        let Some(job) = jobs.into_iter().next() else {
            return Ok(false);
        };
        let run_id = job
            .payload
            .get("run_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok());
        let Some(run_id) = run_id else {
            // 壊れた payload は恒久エラー→DLQ。
            let _ = jobq::kill(&mut conn, job.id, "invalid payload: missing run_id").await;
            return Ok(true);
        };

        // 1 スレッド 1 本ずつ直列に生成する。先行 run が終わるまでこのジョブは**失敗ではなく
        // 順番待ち**として戻す（`defer` は attempts を消費しないので DLQ へ落ちない）。
        // 生成中でも発話を受け付けられるのはこの直列化が前提（キューはサーバが持つ）。
        if self.store.blocked_by_earlier_run(run_id).await? {
            jobq::defer(&mut conn, job.id, QUEUE_WAIT_POLL)
                .await
                .map_err(|e| ChatError::Internal(format!("jobq defer: {e}")))?;
            return Ok(true);
        }

        let last_attempt = job.attempts >= job.max_attempts;
        match self.process_run(run_id, worker_id).await {
            Ok(()) => {
                jobq::ack(&mut conn, job.id)
                    .await
                    .map_err(|e| ChatError::Internal(format!("jobq ack: {e}")))?;
            }
            Err(e) => {
                tracing::warn!(error = %e, run_id = %run_id, last_attempt, "generation failed");
                // 最終試行なら run を failed 化し UI に反映（DLQ 行き）。
                if last_attempt {
                    let _ = self.store.force_fail_run(run_id, &e.to_string()).await;
                }
                let backoff = jobq::backoff_for(job.attempts);
                let _ = jobq::fail(&mut conn, job.id, &e.to_string(), backoff).await;
            }
        }
        Ok(true)
    }

    /// 1 run を生成する（claim→モード分岐→確定）。
    async fn process_run(&self, run_id: Uuid, worker_id: &str) -> Result<(), ChatError> {
        let Some(run) = self
            .store
            .claim_run(run_id, worker_id, self.config.lease_secs)
            .await?
        else {
            // 既に done/cancelled、または有効リース保持中。ack 相当（何もしない）。
            return Ok(());
        };
        let ctx = build_ctx(&run);
        let fencing = run.fencing_token;

        // 明示停止済みなら即キャンセル確定（終端イベントは finalize と同一 TX）。
        if run.cancel_requested {
            self.store
                .finalize_run(
                    run_id,
                    fencing,
                    RunStatus::Cancelled,
                    &[],
                    None,
                    Some(&StreamEventKind::Status {
                        status: RunStatus::Cancelled,
                    }),
                )
                .await?;
            return Ok(());
        }

        // resume する run は、チェックポイント境界より後に残る**破棄 attempt のイベント**を
        // 先に削除する（SSE replay の二重表示防止・#351）。この後に積む Status(Running) 以降が
        // 新 attempt の行になる。削除に失敗したまま続行すると、以後の checkpoint の event_seq が
        // 破棄行を包含し、次の takeover の seed（projection 再構築）へ部分出力が混入して
        // message.content の真実まで壊れるため、**続行せず Err で job retry へ回す**。
        if let Some(envelope) = approval_policy::restore_checkpoint(&run) {
            if let Err(e) = self
                .store
                .prune_events_after(run_id, fencing, envelope.event_seq)
                .await
            {
                tracing::warn!(run_id = %run_id, error = %e, "破棄イベントの削除に失敗（retry へ）");
                return Err(e);
            }
        }

        // ハートビート（リース延長＋cancel 検知）。共有フラグでキャンセルを伝える。
        let cancel = Arc::new(AtomicBool::new(false));
        let hb = spawn_heartbeat(
            self.store.clone(),
            run_id,
            fencing,
            self.config.lease_secs,
            cancel.clone(),
        );

        // 生成開始（running）。
        let _ = self
            .store
            .append_stream_event(
                run_id,
                fencing,
                &StreamEventKind::Status {
                    status: RunStatus::Running,
                },
            )
            .await;

        let history = self
            .build_history(&ctx, run.thread_id, run.message_id)
            .await?;
        // 自律 run はステップ境界のチェックポイントを durable run 行へ永続化する（resume・#351）。
        let mut worker_sink = WorkerSink::new(self.store.clone(), run_id, fencing, cancel.clone())
            .with_checkpoints(run.autonomous);

        // 既定では非自律チャットも agent-core ループ（Chat プロファイル）を通す＝モデルが
        // ツール発火を裁量する（挨拶等は検索しない・generative UI も通常チャットで出る。issue #102）。
        // `classic_rag=true` の運用でのみ旧・無条件 RAG 注入経路を使う（後方互換フォールバック）。
        // ただし明示的なエージェントモード run（agent_mode）と自律 run はループを維持する
        // （classic_rag はあくまで「未指定の通常チャット」の既定を旧挙動に戻すだけ）。
        let use_classic = self.config.classic_rag && !run.autonomous && !run.agent_mode;
        let gen_result = if use_classic {
            // 古典経路はツールを持たない＝添付 seed の対象外（messages だけ渡す）。
            self.run_classic_mode(&ctx, &run, history.messages, &mut worker_sink)
                .await
        } else {
            self.run_agent_mode(&ctx, &run, history, cancel.clone(), &mut worker_sink)
                .await
        };

        hb.abort();

        // リースを失っていたら（別ワーカー takeover）確定しない。
        if worker_sink.lost_lease() {
            return Ok(());
        }

        let content: Vec<ContentBlock> = worker_sink.content().to_vec();
        let cancelled = cancel.load(Ordering::Relaxed);

        match gen_result {
            Ok(()) if cancelled => {
                self.store
                    .finalize_run(
                        run_id,
                        fencing,
                        RunStatus::Cancelled,
                        &content,
                        None,
                        Some(&StreamEventKind::Status {
                            status: RunStatus::Cancelled,
                        }),
                    )
                    .await?;
            }
            Ok(()) => {
                self.store
                    .finalize_run(
                        run_id,
                        fencing,
                        RunStatus::Done,
                        &content,
                        None,
                        Some(&StreamEventKind::Done {
                            message_id: run.message_id,
                        }),
                    )
                    .await?;
            }
            Err(e) => {
                // 生成失敗→retry のため確定しない（リース失効で takeover・最終試行で force_fail）。
                return Err(e);
            }
        }
        Ok(())
    }

    /// 孤児回収 sweeper（定期）。
    async fn run_sweeper(self) {
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            // リースの数倍を grace に（生きたワーカーを誤って failed 化しない）。
            let grace = self.config.lease_secs * 4;
            if let Err(e) = self.store.reap_orphaned_runs(grace).await {
                tracing::warn!(error = %e, "orphan sweeper error");
            }
        }
    }
}

/// run 行から発話ユーザーの [`AuthContext`] を再構築する（昇格しない）。
fn build_ctx(run: &ClaimedRun) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: run.actor.clone(),
            email: None,
            groups: Vec::new(),
            roles: Vec::new(),
            tenant_id: Some(run.tenant_id.clone()),
        },
        run.org.clone(),
        run.tenant_id.clone(),
    )
}

/// ハートビートタスク（リース延長＋cancel 検知）。リース喪失/キャンセルで cancel フラグを立てる。
fn spawn_heartbeat(
    store: ChatStore,
    run_id: Uuid,
    fencing: i64,
    lease_secs: i64,
    cancel: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // リースの約 1/3 間隔で延長。
        let interval = Duration::from_secs(u64::try_from((lease_secs / 3).max(2)).unwrap_or(10));
        loop {
            tokio::time::sleep(interval).await;
            match store.heartbeat(run_id, fencing, lease_secs).await {
                Ok(Some(cancel_requested)) => {
                    if cancel_requested {
                        cancel.store(true, Ordering::Relaxed);
                    }
                }
                // fencing 不一致 or 非 running → リース喪失。生成を止める。
                Ok(None) => {
                    cancel.store(true, Ordering::Relaxed);
                    return;
                }
                Err(e) => tracing::warn!(error = %e, "heartbeat error"),
            }
        }
    })
}

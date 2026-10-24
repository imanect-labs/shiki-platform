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
/// ワーカーの設定と依存束（mod.rs から分割・行数規約）。
mod config;
mod gate;
mod generate;
mod history;
/// 生成ジョブの可視期限のハートビート（接続を待ちの間に持たない）。
mod job_lease;
/// 実行オプション/system プロンプト（generate.rs から分割）。
mod opts;
mod run_model;
pub use opts::parse_timezone;
mod sink;
/// 生成の枠と承認待ちの保留（検証 C-P1-3）。
pub mod slot;
mod stream_map;
mod toolset;
/// 記録済みスレッド → 剪定再生のステップ列（#532・`context-replay` CLI が使う）。
pub mod trajectory;
/// ```ui ブロック（Yui）の図の書き方（#530）。
mod yui;

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
pub use yui::with_yui_guide;

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
    /// 文書パーサ（web_fetch の PDF/Office 経路・#405）。
    parser: Option<Arc<dyn rag::DocumentParser>>,
    /// StorageService（自律プロファイルのワークスペース）。
    storage: Option<Arc<storage::StorageService>>,
    /// 図のボタンのワークフロー名の解決（#604）。
    workflow_resolver: Option<Arc<gui::WorkflowResolver>>,
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
    /// Excel・Word のレイアウト編集エンジン（office.read / office.excel_edit / office.word_edit・#725）。
    office_engine: Option<Arc<office::engine::OfficeEngine>>,
    /// 構造化データ（data_query / data_schema / data_record_write・Task 15.7）。未配線なら提示しない。
    data: Option<data::DataStore>,
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
            parser,
            storage,
            workflow_resolver,
            skill_artifacts,
            skill_catalog,
            workflow_store,
            workflow_catalog,
            collab,
            tabular,
            office,
            office_live,
            office_creator,
            office_engine,
        } = deps;
        ChatWorker {
            db,
            store,
            gateway,
            search,
            sandbox,
            artifacts,
            web_search,
            parser,
            storage,
            workflow_resolver,
            skill_artifacts,
            skill_catalog,
            workflow_store,
            workflow_catalog,
            collab,
            tabular,
            office,
            office_live,
            office_creator,
            office_engine,
            data: None,
            config: Arc::new(config),
        }
    }

    /// 構造化データのツール（data_query / data_schema / data_record_write）を配線する（Task 15.7）。
    ///
    /// `WorkerDeps` に足すと全呼び出し箇所が壊れるため、追加の依存は builder で受ける。
    #[must_use]
    pub fn with_data(mut self, data: data::DataStore) -> Self {
        self.data = Some(data);
        self
    }

    /// 同時に生成する run が `concurrency` 本のワーカーと sweeper を起動する。
    ///
    /// 枠（`concurrency`）はセマフォで持ち、承認待ちの run は枠を返して保留する（[`slot`]）。タスクは
    /// 枠＋保留の上限ぶん起動し、各タスクは枠を取ってからジョブを取る。
    pub fn spawn(self, concurrency: usize) -> Vec<tokio::task::JoinHandle<()>> {
        let mut handles = Vec::new();
        let slots = Arc::new(tokio::sync::Semaphore::new(concurrency.max(1)));
        for i in 0..concurrency.max(1) + slot::MAX_PARKED_APPROVALS {
            let w = self.clone();
            let slots = Arc::clone(&slots);
            handles.push(tokio::spawn(async move { w.run_loop(i, slots).await }));
        }
        // 孤児回収 sweeper（backstop）。
        let sweeper = self.clone();
        handles.push(tokio::spawn(async move { sweeper.run_sweeper().await }));
        handles
    }

    /// jobq 消費ループ（枠を取ってからジョブを取る）。
    async fn run_loop(self, worker_index: usize, slots: Arc<tokio::sync::Semaphore>) {
        let worker_id = format!("chat-worker-{worker_index}");
        loop {
            let Ok(permit) = Arc::clone(&slots).acquire_owned().await else {
                return; // セマフォが閉じた（終了）
            };
            let slot = slot::RunSlot::new(Arc::clone(&slots), permit);
            let r = self.claim_and_process(&worker_id, &slot).await;
            drop(slot); // 処理を終えたら枠を返す（保留中に返していれば何もしない）
            match r {
                Ok(true) => {}                                                     // 1 件処理した
                Ok(false) => tokio::time::sleep(Duration::from_millis(300)).await, // 空
                Err(e) => {
                    tracing::error!(error = %e, "chat worker loop error");
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        }
    }

    /// jobq の 1 文ぶんだけ接続を借りる。
    ///
    /// **プール接続を長い待ち（承認待ち・LLM のストリーミング・ツールの実行）を跨いで持たない。**
    /// 以前は claim に使った接続を ack まで握っていたため、承認待ちの run が `max_connections` 件
    /// 溜まるとプールが尽き、他のユーザーのチャット・データの配信・承認の送信までが止まった。
    async fn jobq_conn(&self) -> Result<sqlx::pool::PoolConnection<sqlx::Postgres>, ChatError> {
        self.db
            .acquire()
            .await
            .map_err(|e| ChatError::Internal(format!("acquire: {e}")))
    }

    /// jobq から 1 件 claim して処理する。処理したら true。
    async fn claim_and_process(
        &self,
        worker_id: &str,
        slot: &Arc<slot::RunSlot>,
    ) -> Result<bool, ChatError> {
        // 可視期限は短く取り、処理中は [`job_lease::JobLease`] が延ばし続ける（クラッシュ時は
        // 期限のうちに再配信される）。
        let vt = job_lease::job_visibility(self.config.lease_secs);
        let jobs = {
            let mut conn = self.jobq_conn().await?;
            jobq::claim(&mut conn, CHAT_GENERATION_QUEUE, vt, 1)
                .await
                .map_err(|e| ChatError::Internal(format!("jobq claim: {e}")))?
        };
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
            let mut conn = self.jobq_conn().await?;
            let _ = jobq::kill(&mut conn, job.id, "invalid payload: missing run_id").await;
            return Ok(true);
        };

        // 1 スレッド 1 本ずつ直列に生成する。先行 run が終わるまでこのジョブは**失敗ではなく
        // 順番待ち**として戻す（`defer` は attempts を消費しないので DLQ へ落ちない）。
        // 生成中でも発話を受け付けられるのはこの直列化が前提（キューはサーバが持つ）。
        if self.store.blocked_by_earlier_run(run_id).await? {
            let mut conn = self.jobq_conn().await?;
            jobq::defer(&mut conn, job.id, QUEUE_WAIT_POLL)
                .await
                .map_err(|e| ChatError::Internal(format!("jobq defer: {e}")))?;
            return Ok(true);
        }

        let last_attempt = job.attempts >= job.max_attempts;
        // 生成の間（承認待ちを含む）ジョブを再配信させない。接続は拍ごとに借りるだけ。
        let lease = job_lease::JobLease::start(self.db.clone(), job.id, vt);
        let result = self.process_run(run_id, worker_id, slot).await;
        drop(lease);
        match result {
            Ok(()) => {
                let mut conn = self.jobq_conn().await?;
                jobq::ack(&mut conn, job.id)
                    .await
                    .map_err(|e| ChatError::Internal(format!("jobq ack: {e}")))?;
            }
            Err(e) => {
                // 再試行しても同じ結果になる失敗（プロバイダが要求を拒否した等）は、残りの試行を
                // 待たずに確定する。再試行の間は「生成中」に見え続けるため（#877）。
                let give_up = last_attempt || !e.is_retryable();
                tracing::warn!(error = %e, run_id = %run_id, last_attempt, give_up, "generation failed");
                // 確定するなら run を failed 化し UI に反映（DLQ 行き）。会話には定型文だけを
                // 出し、上流の生エラー（プロバイダ JSON・URL 等）は last_error と tracing に留める。
                if give_up {
                    let _ = self
                        .store
                        .force_fail_run(run_id, &e.user_message(), &e.to_string())
                        .await;
                }
                let mut conn = self.jobq_conn().await?;
                if e.is_retryable() {
                    let backoff = jobq::backoff_for(job.attempts);
                    let _ = jobq::fail(&mut conn, job.id, &e.to_string(), backoff).await;
                } else {
                    let _ = jobq::kill(&mut conn, job.id, &e.to_string()).await;
                }
            }
        }
        Ok(true)
    }

    /// 1 run を生成する（claim→モード分岐→確定）。
    async fn process_run(
        &self,
        run_id: Uuid,
        worker_id: &str,
        slot: &Arc<slot::RunSlot>,
    ) -> Result<(), ChatError> {
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
        // 確定（束縛の解決と最終トランザクション）が終わるまでリースを延ばし続ける。止めるのは
        // この関数を抜けるとき（Drop）。束縛の解決で時間がかかってもリースを失わない。
        let _hb = HeartbeatGuard(spawn_heartbeat(
            self.store.clone(),
            run_id,
            fencing,
            self.config.lease_secs,
            cancel.clone(),
        ));

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
        let mut worker_sink = WorkerSink::new(
            self.store.clone(),
            &run.tenant_id,
            run_id,
            fencing,
            cancel.clone(),
        )
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
            self.run_agent_mode(&ctx, &run, history, cancel.clone(), &mut worker_sink, slot)
                .await
        };

        // リースを失っていたら（別ワーカー takeover）確定しない。
        if worker_sink.lost_lease() {
            return Ok(());
        }

        let content: Vec<ContentBlock> = worker_sink.content().to_vec();
        let cancelled = cancel.load(Ordering::Relaxed);
        // 本文の図のワークフローのボタンを、回答した主体の権限で解決して版を固定する（#604）。
        let ui_bindings = if gen_result.is_ok() {
            self.yui_bindings(&ctx, &content).await
        } else {
            Vec::new()
        };

        match gen_result {
            Ok(()) if cancelled => {
                self.store
                    .finalize_run_with_ui(
                        run_id,
                        fencing,
                        RunStatus::Cancelled,
                        &content,
                        None,
                        Some(&StreamEventKind::Status {
                            status: RunStatus::Cancelled,
                        }),
                        &ui_bindings,
                    )
                    .await?;
            }
            Ok(()) => {
                self.store
                    .finalize_run_with_ui(
                        run_id,
                        fencing,
                        RunStatus::Done,
                        &content,
                        None,
                        Some(&StreamEventKind::Done {
                            message_id: run.message_id,
                        }),
                        &ui_bindings,
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

/// ハートビートを、持ち主が抜けるときに止める。
struct HeartbeatGuard(tokio::task::JoinHandle<()>);

impl Drop for HeartbeatGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
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

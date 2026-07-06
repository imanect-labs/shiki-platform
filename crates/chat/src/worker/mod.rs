//! チャット生成ワーカー（Task 3.11）。jobq を消費し、claim した run を生成して確定する。
//!
//! - **専用レーン**: jobq の `chat_generation` キューのみ消費（ワークフロー/ingestion と同居させない）。
//! - **claim＋リース＋fencing**: [`ChatStore::claim_run`] で running 化し、ハートビートでリース延長。
//!   fencing 不一致の追記は拒否（クラッシュ takeover＋ゾンビ書込拒否）。
//! - **モード分岐**: agent_mode ON=agent-core ループ（doc_search 等）／OFF=古典 RAG 注入＋gateway 直。
//! - **AuthContext 伝播**: run に保存した発話ユーザーで生成し昇格しない（confused-deputy 防御）。
//! - **協調キャンセル**: ユーザー明示停止（cancel_requested）のみ。ページ離脱はキャンセルしない。

mod generate;
mod sink;

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
use sink::WorkerSink;

/// ワーカーの設定。
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// システムプロンプト。
    pub system_prompt: String,
    /// 論理モデル名（未指定は gateway 既定）。
    pub model: Option<String>,
    /// リース秒（ハートビート間隔の数倍を推奨）。
    pub lease_secs: i64,
    /// エージェントモードの最大ステップ。
    pub max_steps: usize,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        WorkerConfig {
            system_prompt: "あなたは社内文書に基づいて日本語で丁寧に回答するアシスタントです。\
                根拠がある場合は検索結果を活用し、分からない場合は正直に伝えてください。"
                .to_string(),
            model: None,
            lease_secs: 30,
            max_steps: 6,
        }
    }
}

/// チャット生成ワーカー。複数タスクで並行消費できる（各タスクが claim ループを回す）。
#[derive(Clone)]
pub struct ChatWorker {
    db: PgPool,
    store: ChatStore,
    gateway: LlmGateway,
    search: Option<Arc<SearchService>>,
    /// サンドボックス（code_interpreter 用）。未配線なら code_interpreter ツールを提示しない。
    sandbox: Option<Arc<dyn agent_core::Sandbox>>,
    config: Arc<WorkerConfig>,
}

impl ChatWorker {
    pub fn new(
        db: PgPool,
        store: ChatStore,
        gateway: LlmGateway,
        search: Option<Arc<SearchService>>,
        sandbox: Option<Arc<dyn agent_core::Sandbox>>,
        config: WorkerConfig,
    ) -> Self {
        ChatWorker {
            db,
            store,
            gateway,
            search,
            sandbox,
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

        // 明示停止済みなら即キャンセル確定。
        if run.cancel_requested {
            let _ = self
                .store
                .append_stream_event(
                    run_id,
                    fencing,
                    &StreamEventKind::Status {
                        status: RunStatus::Cancelled,
                    },
                )
                .await;
            self.store
                .finalize_run(run_id, fencing, RunStatus::Cancelled, &[], None)
                .await?;
            return Ok(());
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
        let mut worker_sink = WorkerSink::new(self.store.clone(), run_id, fencing, cancel.clone());

        let gen_result = if run.agent_mode {
            self.run_agent_mode(&ctx, &run, history, &mut worker_sink)
                .await
        } else {
            self.run_classic_mode(&ctx, &run, history, &mut worker_sink)
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
                let _ = self
                    .store
                    .append_stream_event(
                        run_id,
                        fencing,
                        &StreamEventKind::Status {
                            status: RunStatus::Cancelled,
                        },
                    )
                    .await;
                self.store
                    .finalize_run(run_id, fencing, RunStatus::Cancelled, &content, None)
                    .await?;
            }
            Ok(()) => {
                self.store
                    .finalize_run(run_id, fencing, RunStatus::Done, &content, None)
                    .await?;
                let _ = self
                    .store
                    .append_stream_event(
                        run_id,
                        fencing,
                        &StreamEventKind::Done {
                            message_id: run.message_id,
                        },
                    )
                    .await;
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

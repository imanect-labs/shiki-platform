//! jobq consumer（Task 2.8）: claim → 冪等判定 → indexer 実行 → ack / fail / kill。

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use jobq::{ClaimedJob, FailOutcome};

use super::indexer::{self, IndexOutcome};
use super::job_state;
use super::{IngestMessage, PipelineDeps, RAG_INGEST_QUEUE};
use crate::error::RagError;

/// ジョブを 1 バッチ消費する。処理した件数を返す（0 = キューが空）。
pub async fn consume_once(deps: &Arc<PipelineDeps>) -> Result<usize, RagError> {
    let concurrency = deps.config.consumer_concurrency.max(1);
    let vt = Duration::from_secs(deps.config.job_vt_secs);
    let jobs = {
        let mut conn = deps.pool.acquire().await?;
        jobq::claim(&mut conn, RAG_INGEST_QUEUE, vt, concurrency as i64).await?
    };
    let count = jobs.len();
    futures::stream::iter(jobs)
        .for_each_concurrent(concurrency, |job| {
            let deps = Arc::clone(deps);
            async move {
                process_job(&deps, job).await;
            }
        })
        .await;
    Ok(count)
}

/// 1 ジョブの実行と結果整理（ack / バックオフ再配信 / DLQ）。
async fn process_job(deps: &Arc<PipelineDeps>, job: ClaimedJob) {
    let result = run_job(deps, &job).await;
    let Ok(mut conn) = deps.pool.acquire().await else {
        // 接続すら取れない場合は何もしない（vt 経過で自動再配信される）。
        tracing::error!(job_id = job.id, "ジョブ結果の記録用接続が取得できません");
        return;
    };
    match result {
        Ok(outcome) => {
            if let Err(e) = jobq::ack(&mut conn, job.id).await {
                tracing::error!(job_id = job.id, error = %e,
                    "ack に失敗（vt 経過後に再配信され冪等処理される）");
            }
            tracing::info!(job_id = job.id, trace_id = ?job.trace_id, outcome = %outcome,
                "インジェスト完了");
        }
        Err(e) if e.is_transient() => {
            drop(conn);
            retry_or_dead(deps, &job, &e).await;
        }
        Err(e) => {
            drop(conn);
            kill_permanent(deps, &job, &e).await;
        }
    }
}

/// 恒久エラー（パース失敗・版不一致など）: リトライせず即 DLQ。
/// kill と rag_ingest_job の dead 記録は同一 Tx（片方だけ反映される不整合を防ぐ）。
async fn kill_permanent(deps: &Arc<PipelineDeps>, job: &ClaimedJob, error: &RagError) {
    let result: Result<(), RagError> = async {
        let mut tx = deps.pool.begin().await?;
        jobq::kill(&mut tx, job.id, &error.to_string()).await?;
        job_state::mark_job_dead_on(&mut tx, job, &error.to_string()).await?;
        tx.commit().await?;
        Ok(())
    }
    .await;
    if let Err(qe) = result {
        tracing::error!(job_id = job.id, error = %qe, "kill/dead 記録に失敗（vt 経過で再配信）");
        return;
    }
    log_dead_letter(job.id, job.trace_id.as_deref(), error);
}

/// 恒久エラーで DLQ へ送ったことを記録する。
///
/// 利用者の文書が原因（壊れた・空・未対応形式など）の拒否は想定どおりの失敗なので WARN、
/// 設定・版不一致・衝突など調べるべき恒久エラーだけを ERROR で出す。
fn log_dead_letter(job_id: i64, trace_id: Option<&str>, error: &RagError) {
    if let Some(code) = error.document_rejection_code() {
        tracing::warn!(job_id, trace_id = ?trace_id, code, error = %error,
            "文書を処理できないため DLQ へ移送");
    } else {
        tracing::error!(job_id, trace_id = ?trace_id, error = %error,
            "恒久エラー。DLQ へ移送");
    }
}

/// 一時エラーのバックオフ再配信（試行上限で DLQ）。
/// fail と rag_ingest_job の dead 記録は同一 Tx（片方だけ反映される不整合を防ぐ）。
async fn retry_or_dead(deps: &Arc<PipelineDeps>, job: &ClaimedJob, error: &RagError) {
    let backoff = jobq::backoff_for(job.attempts);
    let result: Result<FailOutcome, RagError> = async {
        let mut tx = deps.pool.begin().await?;
        let outcome = jobq::fail(&mut tx, job.id, &error.to_string(), backoff).await?;
        if outcome == FailOutcome::Dead {
            job_state::mark_job_dead_on(&mut tx, job, &error.to_string()).await?;
        }
        tx.commit().await?;
        Ok(outcome)
    }
    .await;
    match result {
        Ok(FailOutcome::Dead) => {
            tracing::error!(job_id = job.id, trace_id = ?job.trace_id, error = %error,
                "試行上限に達し DLQ へ移送");
        }
        Ok(FailOutcome::Retry { .. }) => {
            tracing::warn!(job_id = job.id, trace_id = ?job.trace_id, error = %error,
                backoff_secs = backoff.as_secs(), "一時エラー。バックオフ後に再試行");
        }
        Err(qe) => tracing::error!(job_id = job.id, error = %qe, "fail 記録に失敗"),
    }
}

/// メッセージ検証 → 冪等判定 → 実処理。
async fn run_job(deps: &Arc<PipelineDeps>, job: &ClaimedJob) -> Result<IndexOutcome, RagError> {
    let message: IngestMessage = serde_json::from_value(job.payload.clone())?;
    // tenant はキュー行（第一級カラム）とメッセージの二重持ち。食い違いは越境の兆候
    // なので fail-closed（恒久エラー）で止める。
    if message.tenant_id != job.tenant_id {
        return Err(RagError::Config(format!(
            "tenant 不一致: queue={} message={}",
            job.tenant_id, message.tenant_id
        )));
    }
    indexer::handle(deps, &message, job.trace_id.as_deref()).await
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::fmt::MakeWriter;

    use super::*;

    /// 出力されたログ行を溜める書き込み先。
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Captured {
        type Writer = Captured;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    fn captured_log(error: &RagError) -> String {
        let out = Captured::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(out.clone())
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            log_dead_letter(7, Some("trace-1"), error);
        });
        let bytes = out.0.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }

    fn parse_error(code: &str) -> RagError {
        RagError::Parse {
            code: code.into(),
            detail: "detail".into(),
        }
    }

    #[test]
    fn 文書が原因の拒否は_warn_で出し_恒久エラーの_error_を出さない() {
        for code in [
            "parse_failed",
            "empty_document",
            "unsupported_content_type",
            "source_too_large",
        ] {
            let log = captured_log(&parse_error(code));
            assert!(log.contains(" WARN "), "{code}: {log}");
            assert!(
                log.contains("文書を処理できないため DLQ へ移送"),
                "{code}: {log}"
            );
            assert!(log.contains(&format!("code=\"{code}\"")), "{code}: {log}");
            assert!(!log.contains("ERROR"), "{code}: {log}");
            assert!(!log.contains("恒久エラー。DLQ へ移送"), "{code}: {log}");
        }
    }

    #[test]
    fn 基盤や設定が原因の恒久エラーは_error_のまま() {
        let errors = [
            parse_error("source_fetch_failed"),
            parse_error("source_url_not_allowed"),
            parse_error("invalid_content"),
            RagError::Config("tenant 不一致".into()),
            RagError::EmbeddingVersionMismatch {
                expected: "a".into(),
                actual: "b".into(),
            },
            RagError::ChunkIdConflict {
                node_id: uuid::Uuid::nil(),
                count: 1,
            },
            RagError::InvalidTenant("x".into()),
        ];
        for error in &errors {
            let log = captured_log(error);
            assert!(log.contains("ERROR"), "{error}: {log}");
            assert!(log.contains("恒久エラー。DLQ へ移送"), "{error}: {log}");
            assert!(log.contains("job_id=7"), "{error}: {log}");
        }
    }
}

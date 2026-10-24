//! jobq のジョブのリース（短い可視期限＋ハートビート・検証 D-5/A25）。
//!
//! 長いジョブ（索引のバックフィル・一括取込・統計収集）は可視期限を短く（[`JOB_VT`]）取り、処理中は
//! バックグラウンドのハートビートが期限を延ばし続ける。プロセスが落ちれば延長が止まり、短い期限の
//! うちに再起動後の consumer へ再配信される（以前は 15 分の固定期限で、クラッシュのたびに 15 分止まった）。
//! ハートビートは [`JobLease`] を落とすと止まる（ack / fail の前に落とす）。

use std::time::Duration;

use storage::PoolRouter;

/// 長いジョブの可視期限。ハートビートが [`HEARTBEAT_EVERY`] ごとに延ばす。
pub(crate) const JOB_VT: Duration = Duration::from_mins(1);
/// ハートビートの間隔（期限の 1/3・2 回取りこぼしても期限内）。
const HEARTBEAT_EVERY: Duration = Duration::from_secs(20);

/// 処理中のジョブのリース。生きている間、可視期限を延ばし続ける。
pub(crate) struct JobLease {
    task: tokio::task::JoinHandle<()>,
}

impl JobLease {
    /// ジョブ `id` のハートビートを始める（ジョブはテナントのプールのキューにある）。
    pub(crate) fn start(pools: &PoolRouter, tenant_id: &str, id: i64) -> Self {
        let pools = pools.clone();
        let tenant_id = tenant_id.to_string();
        let task = tokio::spawn(async move {
            let mut tick = tokio::time::interval(HEARTBEAT_EVERY);
            tick.tick().await; // 初回は即時に返るので読み捨てる（確保の直後は期限が新しい）
            loop {
                tick.tick().await;
                let r = async {
                    let mut conn = pools.for_tenant(&tenant_id).acquire().await?;
                    jobq::extend(&mut conn, id, JOB_VT)
                        .await
                        .map_err(|jobq::JobqError::Db(e)| e)
                }
                .await;
                match r {
                    Ok(true) => {}
                    // ack 済み・DLQ へ移送済み（延ばす対象が無い）。
                    Ok(false) => return,
                    // 一時的な DB 障害は次の拍で再試行する（期限は 3 拍分ある）。
                    Err(e) => tracing::warn!(error = %e, job_id = id, "ジョブのハートビートに失敗"),
                }
            }
        });
        JobLease { task }
    }
}

impl Drop for JobLease {
    fn drop(&mut self) {
        self.task.abort();
    }
}

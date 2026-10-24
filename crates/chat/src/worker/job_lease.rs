//! 生成ジョブ（jobq）の可視期限のハートビート。
//!
//! 生成は承認待ちで最大 30 分止まる。以前は claim に使ったプール接続を ack まで握り続け、承認待ちの
//! run が 10 件溜まるとプール（`max_connections`）が尽きてサーバ全体が止まった。いまは接続を文ごとに
//! 借りて返し、その代わりにジョブの可視期限をこのハートビートで延ばし続ける（延ばさないと可視期限が
//! 切れて別のワーカーへ再配信され、そのワーカーは run のリースが有効なので何もせず ack してジョブを
//! 消してしまう）。拍ごとに接続を借りるだけで、待っている間は 1 本も持たない。
//!
//! プロセスが落ちれば延長が止まり、可視期限のうちに再起動後のワーカーへ再配信される。

use std::time::Duration;

use sqlx::PgPool;

/// ジョブの可視期限（`lease_secs` の 2 倍・最低 6 秒）。
///
/// **run のリースより長く取る**: クラッシュ後に再配信されたとき run のリースが切れていないと
/// `claim_run` が取れず、ジョブは ack されて run は誰にも再開されない。最後の延長はクラッシュの
/// 高々 1 拍（期限の 1/3）前なので、ジョブが見えるのはクラッシュから期限の 2/3（= リースの 4/3 倍）
/// 以上後になり、run のリース（クラッシュから高々 `lease_secs`）は必ず先に切れる。
pub(super) fn job_visibility(lease_secs: i64) -> Duration {
    Duration::from_secs(
        u64::try_from(lease_secs.saturating_mul(2))
            .unwrap_or(0)
            .max(6),
    )
}

/// 処理中のジョブの可視期限を延ばし続ける。落とすと止まる。
pub(super) struct JobLease(tokio::task::JoinHandle<()>);

impl JobLease {
    /// ジョブ `id` の延長を始める（`vt` ごとに `vt` の 1/3 間隔で延ばす）。
    pub(super) fn start(db: PgPool, id: i64, vt: Duration) -> Self {
        let every = (vt / 3).max(Duration::from_secs(1));
        JobLease(tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            tick.tick().await; // 初回は即時に返る（claim の直後は期限が新しい）
            loop {
                tick.tick().await;
                // 接続は 1 文だけ借りる（待っている間は持たない）。
                let r = async {
                    let mut conn = db.acquire().await?;
                    jobq::extend(&mut conn, id, vt)
                        .await
                        .map_err(|jobq::JobqError::Db(e)| e)
                }
                .await;
                match r {
                    Ok(true) => {}
                    // ack 済み・DLQ へ移送済み（延ばす対象が無い）。
                    Ok(false) => return,
                    // 一時的な DB 障害は次の拍で再試行する（期限は 3 拍分ある）。
                    Err(e) => tracing::warn!(error = %e, job_id = id, "生成ジョブの延長に失敗"),
                }
            }
        }))
    }
}

impl Drop for JobLease {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_visibility_outlives_the_run_lease() {
        assert_eq!(job_visibility(30), Duration::from_secs(60));
        assert_eq!(job_visibility(1), Duration::from_secs(6));
        assert_eq!(job_visibility(-5), Duration::from_secs(6));
        for lease in [3_i64, 30, 120] {
            let vt = job_visibility(lease);
            // 最後の延長から見えるまで最短で期限の 2/3。これが run のリースより長いこと。
            assert!(vt * 2 / 3 > Duration::from_secs(u64::try_from(lease).unwrap_or(0)));
        }
    }
}

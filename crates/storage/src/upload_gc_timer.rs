//! 中断アップロードの回収タイマ（#468）。
//!
//! 共有リンク失効（`expiry_timer`）は「次に失効する時刻」が DB から分かるためイベント駆動に
//! できるが、こちらは TTL 経過を待つだけなので固定間隔で十分。間隔は既定 1 時間で、
//! アイドル時のクエリ負荷（#450）から見て無視できる頻度に留める。
//!
//! 1 周で TTL sweep → 孤児 sweep の順に回す。TTL sweep が行を消してから孤児 sweep が走ると、
//! 同じ周でオブジェクトの取り残しまで回収できる（TTL sweep の delete_batch が失敗した場合）。

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;

use crate::service::StorageService;

/// 中断アップロード回収の設定。
#[derive(Debug, Clone, Copy)]
pub struct UploadGcOptions {
    /// finalize されないまま経過したら回収する時間。
    pub ttl: Duration,
    /// sweep の実行間隔。
    pub interval: Duration,
}

/// 連続失敗時の最小待機（0 で回して DB/オブジェクトストアを叩き続けないための下限）。
const MIN_BACKOFF: Duration = Duration::from_secs(30);

/// 中断アップロードの回収タイマをバックグラウンド起動する。
///
/// 返した `JoinHandle` は保持不要（プロセス生存中は動き続ける）。
pub fn spawn_upload_gc_timer(
    service: Arc<StorageService>,
    opts: UploadGcOptions,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // 起動直後に 1 回走らせる（前回プロセスが落ちた時の取り残しを引き継いで回収する）。
        let mut backoff = MIN_BACKOFF;
        loop {
            let mut failed = false;

            match service
                .sweep_expired_pending_uploads(Utc::now(), opts.ttl)
                .await
            {
                Ok(n) if n > 0 => {
                    tracing::info!(count = n, "中断アップロードを回収しました（#468）");
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "中断アップロードの回収に失敗しました");
                    failed = true;
                }
            }

            match service.sweep_orphan_upload_objects().await {
                Ok(n) if n > 0 => {
                    tracing::info!(
                        count = n,
                        "対応行の無い staging/incoming オブジェクトを削除しました（#468）"
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "孤児オブジェクトの削除に失敗しました");
                    failed = true;
                }
            }

            let wait = if failed {
                let w = backoff;
                backoff = (backoff * 2).min(opts.interval);
                w
            } else {
                backoff = MIN_BACKOFF;
                opts.interval
            };
            tokio::time::sleep(wait).await;
        }
    })
}

//! 中断アップロードの回収タイマ（#468）。
//!
//! 共有リンク失効（`expiry_timer`）は「次に失効する時刻」が DB から分かるためイベント駆動に
//! できるが、こちらは TTL 経過を待つだけなので固定間隔で十分。間隔は既定 1 時間で、
//! アイドル時のクエリ負荷（#450）から見て無視できる頻度に留める。
//!
//! 1 周で TTL sweep → 孤児 sweep の順に回す。TTL sweep が行を消してから孤児 sweep が走ると、
//! 同じ周でオブジェクトの取り残しまで回収できる（TTL sweep の delete_batch が失敗した場合）。
//!
//! 同じ周で、保持期間を過ぎたゴミ箱の中身の完全削除（GAP-108c・D-202・[`StorageService::sweep_trash_retention`]）
//! を回し、最後に完全削除の片付け待ち（`storage_purge_pending`・GAP-108）を片付ける
//! （[`StorageService::sweep_purge_pending`]）。保持期間の完全削除が積んだ片付け待ちも同じ周で片付く。api の配線と
//! 設定は増やさず、このタイマに相乗りする。`upload_gc.enabled=false` の環境では、保持期間の完全削除も片付けも回らない
//! （保持期間を入れたテナントのゴミ箱が消えない。片付けが残っても DB の行はもう無いので利用者に見えるものは増えない）。

use std::sync::Arc;
use std::time::Duration;

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

/// sweep 間隔の下限。
///
/// 設定は起動時に検証しているが（`AppConfig::check_upload_gc_bounds`）、タイマ側でも
/// 下限を効かせる。ここが 0 になると、成功時の待機も失敗時 backoff の上限
/// （`min(interval)`）も 0 に潰れ、全テナントのプレフィックス LIST と DB 走査を
/// 休みなく連続実行して自分でストアを飽和させる。
const MIN_INTERVAL: Duration = Duration::from_mins(1);

/// 中断アップロードの回収タイマをバックグラウンド起動する。
///
/// 返した `JoinHandle` は保持不要（プロセス生存中は動き続ける）。1 周は [`run_upload_gc_cycle`]。
pub fn spawn_upload_gc_timer(
    service: Arc<StorageService>,
    opts: UploadGcOptions,
) -> tokio::task::JoinHandle<()> {
    let interval = opts.interval.max(MIN_INTERVAL);
    tokio::spawn(async move {
        // 起動直後に 1 回走らせる（前回プロセスが落ちた時の取り残しを引き継いで回収する）。
        let mut backoff = MIN_BACKOFF;
        loop {
            let failed = run_upload_gc_cycle(&service, opts.ttl).await;
            let wait = if failed {
                let w = backoff;
                backoff = (backoff * 2).min(interval).max(MIN_BACKOFF);
                w
            } else {
                backoff = MIN_BACKOFF;
                interval
            };
            tokio::time::sleep(wait).await;
        }
    })
}

/// 回収タイマの 1 周（TTL sweep → 孤児 sweep → ゴミ箱の保持期間 → 完全削除の片付け待ち）。
///
/// どれかが DB・オブジェクトストアに届かずに失敗したら `true`（タイマは待ちを延ばす）。根ごと・行ごとの失敗は
/// それぞれの掃除が数えて次の周に回すので、ここでは `true` にしない。
pub async fn run_upload_gc_cycle(service: &StorageService, ttl: Duration) -> bool {
    // 失敗しても続ける（1 つの掃除の失敗で、後の掃除を止めない）。
    let expired = sweep_expired(service, ttl).await;
    let orphans = sweep_orphans(service).await;
    let retention = sweep_retention(service).await;
    let pending = sweep_pending(service).await;
    expired || orphans || retention || pending
}

/// TTL を過ぎた中断アップロードを回収する。失敗したら `true`。
async fn sweep_expired(service: &StorageService, ttl: Duration) -> bool {
    match service.sweep_expired_pending_uploads(ttl).await {
        Ok(n) => {
            if n > 0 {
                tracing::info!(count = n, "中断アップロードを回収しました（#468）");
            }
            false
        }
        Err(e) => {
            tracing::warn!(error = %e, "中断アップロードの回収に失敗しました");
            true
        }
    }
}

/// 対応行の無い staging/incoming オブジェクトを消す。失敗したら `true`。
async fn sweep_orphans(service: &StorageService) -> bool {
    match service.sweep_orphan_upload_objects().await {
        Ok(n) => {
            if n > 0 {
                tracing::info!(
                    count = n,
                    "対応行の無い staging/incoming オブジェクトを削除しました（#468）"
                );
            }
            false
        }
        Err(e) => {
            tracing::warn!(error = %e, "孤児オブジェクトの削除に失敗しました");
            true
        }
    }
}

/// 保持期間を過ぎたゴミ箱の中身を完全に削除する。根ごとの失敗は数えて次の周に回し、候補を引けない（DB に
/// 届かない）ときだけ `true`。
async fn sweep_retention(service: &StorageService) -> bool {
    match service.sweep_trash_retention(chrono::Utc::now()).await {
        Ok(r) => {
            if r.purged > 0 || r.live_descendants > 0 || r.failed > 0 {
                tracing::info!(
                    purged = r.purged,
                    nodes = r.nodes,
                    live_descendants = r.live_descendants,
                    failed = r.failed,
                    more = r.more,
                    "保持期間を過ぎたゴミ箱の中身を完全に削除しました（GAP-108c）"
                );
            }
            false
        }
        Err(e) => {
            tracing::warn!(error = %e, "ゴミ箱の保持期間を適用できませんでした");
            true
        }
    }
}

/// 完全削除の片付け待ちを片付ける。行ごとの失敗は行に残して次の周に回し、DB に届かないときだけ `true`。
async fn sweep_pending(service: &StorageService) -> bool {
    match service.sweep_purge_pending().await {
        Ok(r) => {
            if r.done > 0 || r.failed > 0 {
                tracing::info!(
                    done = r.done,
                    failed = r.failed,
                    "完全削除の片付け待ちを片付けました（GAP-108）"
                );
            }
            false
        }
        Err(e) => {
            tracing::warn!(error = %e, "完全削除の片付け待ちを片付けられませんでした");
            true
        }
    }
}

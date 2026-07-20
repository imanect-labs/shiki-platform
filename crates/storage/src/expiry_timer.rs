//! 一般アクセス有効期限の失効タイマ（#338・イベント駆動・定期ポーリング無し）。
//!
//! OpenFGA タプルに TTL が無いため、期限切れの一般アクセス（broad / redeem 済み per-user）を
//! バックグラウンドで剥奪する。**固定 interval ポーリングはしない**: 次に失効する時刻まで
//! sleep し、その瞬間だけ起きて処理する。新しい（今より早い）期限が設定されたら
//! [`StorageService::expiry_notify`] で起こされて次回起床時刻を再計算する。失効対象が無ければ
//! backstop 時間だけ idle する（安全網の再チェック）。

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;

use crate::service::StorageService;

/// 失効対象が無いときの最大 sleep（安全網。タイマ取りこぼしがあってもこの間隔で再チェックする）。
const BACKSTOP: Duration = Duration::from_hours(1);

/// 一般アクセス失効タイマをバックグラウンド起動する（shiki-server の起動フローから呼ぶ）。
///
/// 返した `JoinHandle` は保持不要（プロセス生存中は動き続ける）。
pub fn spawn_general_access_expiry_timer(
    service: Arc<StorageService>,
) -> tokio::task::JoinHandle<()> {
    let notify = service.expiry_notify();
    tokio::spawn(async move {
        loop {
            // 1. 既に期限切れのものを剥奪する。
            match service.revoke_expired_general_access(Utc::now()).await {
                Ok(n) if n > 0 => {
                    tracing::info!(count = n, "一般アクセスの期限切れを剥奪しました（#338）");
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "一般アクセスの失効処理に失敗しました"),
            }
            // 2. 次に失効する時刻まで待つ（無ければ backstop）。
            let wait = match service.next_general_access_expiry().await {
                Ok(Some(next)) => {
                    let ms = (next - Utc::now()).num_milliseconds();
                    if ms <= 0 {
                        Duration::from_millis(0)
                    } else {
                        Duration::from_millis(u64::try_from(ms).unwrap_or(u64::MAX))
                    }
                }
                Ok(None) => BACKSTOP,
                Err(e) => {
                    tracing::warn!(error = %e, "次回失効時刻の取得に失敗しました");
                    BACKSTOP
                }
            }
            // 遠い将来の期限でも backstop 間隔で再チェックする（安全網）。
            .min(BACKSTOP);
            // 3. 起床時刻 or 新期限設定の通知のどちらか早い方まで待つ。
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                () = notify.notified() => {}
            }
        }
    })
}

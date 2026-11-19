//! プロバイダの稼働状況の読み取り（管理コンソール・利用者のモデル選択）。
use std::collections::HashMap;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

use super::{LlmGateway, ProviderInfo};
use crate::config::ProviderKind;
use crate::health::{status, HealthState, HealthThresholds, ProviderHealth};

/// 利用者向けの状態を使い回す時間。書き出し（15 秒ごと）と同じ程度にする。
const STATE_TTL: Duration = Duration::from_secs(15);

/// プロバイダ ID → 今の状態。
type States = HashMap<String, HealthState>;

/// 読み取りの上限。過ぎたら注意を省いてモデルの一覧を先に返す。
const STATE_READ_TIMEOUT: Duration = Duration::from_secs(1);

/// 利用者向けの状態の短い間のキャッシュ（読んだ時刻と状態）。
///
/// 期限切れの読み直しを 1 本にまとめるため、await をまたいでロックを持つ（tokio の Mutex）。
#[derive(Default)]
pub(super) struct StateCache(tokio::sync::Mutex<Option<(Instant, States)>>);

/// 稼働状況を数えるプロバイダか（ChatGPT プランは呼び出し主体ごとの枠なので数えない）。
fn monitored(p: &ProviderInfo) -> bool {
    p.kind != ProviderKind::ChatgptPlan
}

impl LlmGateway {
    /// プロバイダの稼働状況（[`providers`](Self::providers) と同じ順）。数えていない
    /// プロバイダ（ChatGPT プラン）は `None`。
    ///
    /// # Errors
    /// 集計の読み取りに失敗したとき。
    pub async fn provider_health(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<(ProviderInfo, Option<ProviderHealth>)>, sqlx::Error> {
        let infos = self.providers();
        let ids: Vec<String> = infos
            .iter()
            .filter(|p| monitored(p))
            .map(|p| p.id.clone())
            .collect();
        let mut health = status::read(
            self.inner.usage.db(),
            &ids,
            now,
            &self.inner.config.health_thresholds,
        )
        .await?
        .into_iter();
        Ok(infos
            .into_iter()
            .map(|p| {
                let h = if monitored(&p) { health.next() } else { None };
                (p, h)
            })
            .collect())
    }

    /// 稼働状況の判定の閾値（設定値）。
    #[must_use]
    pub fn health_thresholds(&self) -> &HealthThresholds {
        &self.inner.config.health_thresholds
    }

    /// 今の状態（プロバイダ ID → 状態）。利用者のモデル選択の注意表示用で、読めなければ空
    /// （注意を出さないだけで、モデルの一覧そのものは止めない）。
    pub async fn provider_states(&self) -> States {
        let mut cache = self.inner.states.0.lock().await;
        if let Some((at, states)) = cache.as_ref() {
            if at.elapsed() < STATE_TTL {
                return states.clone();
            }
        }
        let ids: Vec<String> = self
            .providers()
            .iter()
            .filter(|p| monitored(p))
            .map(|p| p.id.clone())
            .collect();
        let read = tokio::time::timeout(
            STATE_READ_TIMEOUT,
            status::read_states(
                self.inner.usage.db(),
                &ids,
                Utc::now(),
                &self.inner.config.health_thresholds,
            ),
        )
        .await;
        // 読めなかったときも空を覚えておく（DB が詰まっている間、要求ごとに待ち直さない）。
        let states: States = match read {
            Ok(Ok(s)) => ids.into_iter().zip(s).collect(),
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "LLM プロバイダの状態を読めなかった（注意表示を省く）");
                HashMap::new()
            }
            Err(_) => {
                tracing::warn!(
                    "LLM プロバイダの状態の読み取りが時間内に終わらなかった（注意表示を省く）"
                );
                HashMap::new()
            }
        };
        *cache = Some((Instant::now(), states.clone()));
        states
    }
}

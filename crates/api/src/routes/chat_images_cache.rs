//! 回答の画像の代理取得の、キャッシュと同時実行の上限（#1044）。
//!
//! - **キャッシュ**: 取得した画像を `(テナント, 出典の URL)` ごとに [`TTL`] だけ持つ。会話を開くたびに配信元へ
//!   取りに行くと、配信元は「その会話がいつ・何回開かれたか」を観測できる（ページごとに固有の画像 URL を置けば
//!   会話を特定できる）。キャッシュの間は配信元へ通信しないので、伝わるのは初回の取得の時刻だけになる。
//!   テナントをまたいで共有しない（他テナントがその画像を見たかを、応答の速さで推測させない）。
//!   認可（スレッドの viewer）と出所の判定はキャッシュの前に毎回通る。キャッシュは取得の結果だけを持つ。
//! - **同時実行の上限**: 外部への取得を [`MAX_CONCURRENT_FETCHES`] 件までにする。大きな画像を並べたページを
//!   読ませて連打し、shiki-server を増幅中継にしたり、メモリを使い切らせたりさせない。空きを
//!   [`ACQUIRE_TIMEOUT`] 待っても得られなければ取得しない（フロントはリンクに落とす）。

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use tokio::sync::{Semaphore, SemaphorePermit};

/// キャッシュに持つ時間（ブラウザ側の `max-age` と揃える）。
pub(crate) const TTL: Duration = Duration::from_hours(1);
/// キャッシュ全体の上限（バイト）。超えたら古いものから捨てる。
pub(crate) const MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;
/// 外部への取得の同時実行数。
pub(crate) const MAX_CONCURRENT_FETCHES: usize = 16;
/// 空きを待つ時間。
pub(crate) const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(2);

/// キャッシュの鍵（テナント, 出典の URL）。
type Key = (String, String);

/// キャッシュした画像（判定した型と中身）。
#[derive(Clone)]
pub(crate) struct CachedImage {
    pub mime: &'static str,
    pub bytes: Bytes,
}

struct Entry {
    image: CachedImage,
    at: Instant,
}

/// プロセス内の画像キャッシュ（1 プロセスで完結。複数台構成でも各台が独立に持つだけで、正しさには影響しない）。
pub(crate) struct ImageCache {
    inner: Mutex<(HashMap<Key, Entry>, usize)>,
    max_bytes: usize,
    ttl: Duration,
}

impl ImageCache {
    pub(crate) fn new(max_bytes: usize, ttl: Duration) -> Self {
        Self {
            inner: Mutex::new((HashMap::new(), 0)),
            max_bytes,
            ttl,
        }
    }

    pub(crate) fn get(&self, tenant: &str, url: &str) -> Option<CachedImage> {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (map, total) = &mut *guard;
        let key = (tenant.to_owned(), url.to_owned());
        let fresh = map.get(&key).is_some_and(|e| e.at.elapsed() < self.ttl);
        if fresh {
            return map.get(&key).map(|e| e.image.clone());
        }
        if let Some(stale) = map.remove(&key) {
            *total -= stale.image.bytes.len();
        }
        None
    }

    pub(crate) fn put(&self, tenant: &str, url: &str, image: CachedImage) {
        let size = image.bytes.len();
        if size > self.max_bytes {
            return;
        }
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (map, total) = &mut *guard;
        if let Some(old) = map.remove(&(tenant.to_owned(), url.to_owned())) {
            *total -= old.image.bytes.len();
        }
        // 期限切れを先に捨て、それでも入らなければ古い順に捨てる（件数は多くても数千で、走査で足りる）。
        let ttl = self.ttl;
        map.retain(|_, e| {
            let keep = e.at.elapsed() < ttl;
            if !keep {
                *total -= e.image.bytes.len();
            }
            keep
        });
        while *total + size > self.max_bytes {
            let Some(oldest) = map.iter().min_by_key(|(_, e)| e.at).map(|(k, _)| k.clone()) else {
                break;
            };
            if let Some(e) = map.remove(&oldest) {
                *total -= e.image.bytes.len();
            }
        }
        map.insert(
            (tenant.to_owned(), url.to_owned()),
            Entry {
                image,
                at: Instant::now(),
            },
        );
        *total += size;
    }
}

/// プロセス全体で共有するキャッシュ。
pub(crate) static CACHE: LazyLock<ImageCache> =
    LazyLock::new(|| ImageCache::new(MAX_CACHE_BYTES, TTL));

static FETCH_SLOTS: Semaphore = Semaphore::const_new(MAX_CONCURRENT_FETCHES);

/// 外部への取得の枠を取る。[`ACQUIRE_TIMEOUT`] 待っても空かなければ None。
pub(crate) async fn acquire_fetch_slot() -> Option<SemaphorePermit<'static>> {
    tokio::time::timeout(ACQUIRE_TIMEOUT, FETCH_SLOTS.acquire())
        .await
        .ok()?
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(n: usize) -> CachedImage {
        CachedImage {
            mime: "image/png",
            bytes: Bytes::from(vec![0u8; n]),
        }
    }

    #[test]
    fn hit_is_per_tenant() {
        let c = ImageCache::new(1024, TTL);
        c.put("t1", "https://img.example.com/a.png", img(10));
        assert!(c.get("t1", "https://img.example.com/a.png").is_some());
        assert!(
            c.get("t2", "https://img.example.com/a.png").is_none(),
            "テナントをまたいで共有しない"
        );
    }

    #[test]
    fn expired_entries_are_dropped() {
        let c = ImageCache::new(1024, Duration::from_millis(0));
        c.put("t", "u", img(10));
        assert!(c.get("t", "u").is_none());
        assert_eq!(c.inner.lock().unwrap().1, 0, "期限切れの分は合計から引く");
    }

    #[test]
    fn evicts_oldest_to_stay_within_the_byte_limit() {
        let c = ImageCache::new(100, TTL);
        c.put("t", "a", img(60));
        std::thread::sleep(Duration::from_millis(2));
        c.put("t", "b", img(30));
        std::thread::sleep(Duration::from_millis(2));
        c.put("t", "c", img(30));
        assert!(c.get("t", "a").is_none(), "古いものから捨てる");
        assert!(c.get("t", "b").is_some());
        assert!(c.get("t", "c").is_some());
        assert!(c.inner.lock().unwrap().1 <= 100);
        // 上限より大きいものは持たない。
        c.put("t", "huge", img(101));
        assert!(c.get("t", "huge").is_none());
    }

    #[tokio::test]
    async fn fetch_slots_are_bounded() {
        let mut held = Vec::new();
        for _ in 0..MAX_CONCURRENT_FETCHES {
            held.push(acquire_fetch_slot().await.expect("枠が空いている"));
        }
        assert!(
            acquire_fetch_slot().await.is_none(),
            "上限を超えたら待ってから諦める"
        );
        drop(held);
        assert!(acquire_fetch_slot().await.is_some());
    }
}

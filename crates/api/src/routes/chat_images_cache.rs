//! 回答の画像の代理取得の、キャッシュと同時実行の上限（#1044）。
//!
//! - **キャッシュ**: 取得した画像を `(テナント, 出典の URL)` ごとに [`TTL`] だけ持つ。会話を開くたびに配信元へ
//!   取りに行くと、配信元は「その会話がいつ・何回開かれたか」を観測できる（ページごとに固有の画像 URL を置けば
//!   会話を特定できる）。キャッシュの間は配信元へ通信しないので、伝わるのは初回の取得の時刻だけになる。
//!   テナントをまたいで共有しない（他テナントがその画像を見たかを、応答の速さで推測させない）。
//!   認可（スレッドの viewer）と出所の判定はキャッシュの前に毎回通る。キャッシュは取得の結果だけを持つ。
//!   **取得の失敗も** [`FAILURE_TTL`] だけ持つ（502 や画像でない応答を返す URL を置かれても、会話を開くたびに
//!   配信元へ通信させない）。宛先の検証で止まった失敗も持つ（名前解決の問い合わせは配信元の権威 DNS に届き、
//!   リダイレクトの先で止まった場合は最初の配信元へ要求済みなので、どちらも観測されうる）。
//!   同時実行数の上限で断った分（取得していない）は持たない。
//! - **同時実行の上限**: 外部への取得を [`MAX_CONCURRENT_FETCHES`] 件までにする。大きな画像を並べたページを
//!   読ませて連打し、shiki-server を増幅中継にしたり、メモリを使い切らせたりさせない。空きを
//!   [`ACQUIRE_TIMEOUT`] 待っても得られなければ取得しない（フロントはリンクに落とす）。

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::http::StatusCode;
use tokio::sync::{Semaphore, SemaphorePermit};

/// 画像をキャッシュに持つ時間（ブラウザ側の `max-age` と揃える）。
pub(crate) const TTL: Duration = Duration::from_hours(1);
/// 取得の失敗をキャッシュに持つ時間（配信元の一時的な不調から回復できるよう短くする）。
pub(crate) const FAILURE_TTL: Duration = Duration::from_mins(5);
/// キャッシュ全体の上限（バイト）。画像本体に加え、鍵（テナント・URL）と 1 件あたりの管理領域も数える
/// （小さな画像を長い URL で大量に入れられても、上限がメモリの上限として効くように）。超えたら古いものから捨てる。
pub(crate) const MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;
/// 持つ件数の上限。
pub(crate) const MAX_CACHE_ENTRIES: usize = 4096;
/// 1 件あたりの管理領域の見積もり（HashMap の枠・Entry・Bytes の参照など）。
const ENTRY_OVERHEAD: usize = 128;
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

/// キャッシュに持つ取得の結果。
#[derive(Clone)]
pub(crate) enum Cached {
    /// 取得できた画像。
    Image(CachedImage),
    /// 取得の失敗（クライアントへ返す状態）。
    Failed(StatusCode),
}

struct Entry {
    value: Cached,
    at: Instant,
    ttl: Duration,
    /// この 1 件が使うメモリの見積もり（画像本体＋鍵＋管理領域）。
    cost: usize,
}

impl Entry {
    fn fresh(&self) -> bool {
        self.at.elapsed() < self.ttl
    }
}

/// 1 件が使うメモリの見積もり。
fn cost_of(tenant: &str, url: &str, value: &Cached) -> usize {
    let body = match value {
        Cached::Image(image) => image.bytes.len(),
        Cached::Failed(_) => 0,
    };
    body + tenant.len() + url.len() + ENTRY_OVERHEAD
}

/// プロセス内の画像キャッシュ（1 プロセスで完結。複数台構成でも各台が独立に持つだけで、正しさには影響しない）。
pub(crate) struct ImageCache {
    inner: Mutex<(HashMap<Key, Entry>, usize)>,
    max_bytes: usize,
    max_entries: usize,
}

impl ImageCache {
    pub(crate) fn new(max_bytes: usize, max_entries: usize) -> Self {
        Self {
            inner: Mutex::new((HashMap::new(), 0)),
            max_bytes,
            max_entries,
        }
    }

    pub(crate) fn get(&self, tenant: &str, url: &str) -> Option<Cached> {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (map, total) = &mut *guard;
        let key = (tenant.to_owned(), url.to_owned());
        let fresh = map.get(&key).is_some_and(Entry::fresh);
        if fresh {
            return map.get(&key).map(|e| e.value.clone());
        }
        if let Some(stale) = map.remove(&key) {
            *total -= stale.cost;
        }
        None
    }

    pub(crate) fn put(&self, tenant: &str, url: &str, value: Cached, ttl: Duration) {
        let size = cost_of(tenant, url, &value);
        if size > self.max_bytes || self.max_entries == 0 {
            return;
        }
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (map, total) = &mut *guard;
        if let Some(old) = map.remove(&(tenant.to_owned(), url.to_owned())) {
            *total -= old.cost;
        }
        // 期限切れを先に捨て、それでも入らなければ古い順に捨てる（件数は多くても数千で、走査で足りる）。
        map.retain(|_, e| {
            let keep = e.fresh();
            if !keep {
                *total -= e.cost;
            }
            keep
        });
        while *total + size > self.max_bytes || map.len() >= self.max_entries {
            let Some(oldest) = map.iter().min_by_key(|(_, e)| e.at).map(|(k, _)| k.clone()) else {
                break;
            };
            if let Some(e) = map.remove(&oldest) {
                *total -= e.cost;
            }
        }
        map.insert(
            (tenant.to_owned(), url.to_owned()),
            Entry {
                value,
                at: Instant::now(),
                ttl,
                cost: size,
            },
        );
        *total += size;
    }
}

/// プロセス全体で共有するキャッシュ。
pub(crate) static CACHE: LazyLock<ImageCache> =
    LazyLock::new(|| ImageCache::new(MAX_CACHE_BYTES, MAX_CACHE_ENTRIES));

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

    fn img(n: usize) -> Cached {
        Cached::Image(CachedImage {
            mime: "image/png",
            bytes: Bytes::from(vec![0u8; n]),
        })
    }

    #[test]
    fn hit_is_per_tenant() {
        let c = ImageCache::new(4096, 100);
        c.put("t1", "https://img.example.com/a.png", img(10), TTL);
        assert!(c.get("t1", "https://img.example.com/a.png").is_some());
        assert!(
            c.get("t2", "https://img.example.com/a.png").is_none(),
            "テナントをまたいで共有しない"
        );
    }

    #[test]
    fn expired_entries_are_dropped() {
        let c = ImageCache::new(4096, 100);
        c.put("t", "u", img(10), Duration::ZERO);
        assert!(c.get("t", "u").is_none());
        assert_eq!(c.inner.lock().unwrap().1, 0, "期限切れの分は合計から引く");
    }

    #[test]
    fn evicts_oldest_to_stay_within_the_byte_limit() {
        // 1 件の見積もり = 本体 + "t" + 鍵 1 文字 + 管理領域。
        let per = |n: usize| n + 2 + ENTRY_OVERHEAD;
        let c = ImageCache::new(per(60) + per(30) + 10, 100);
        c.put("t", "a", img(60), TTL);
        std::thread::sleep(Duration::from_millis(2));
        c.put("t", "b", img(30), TTL);
        std::thread::sleep(Duration::from_millis(2));
        c.put("t", "c", img(30), TTL);
        assert!(c.get("t", "a").is_none(), "古いものから捨てる");
        assert!(c.get("t", "b").is_some());
        assert!(c.get("t", "c").is_some());
        assert!(c.inner.lock().unwrap().1 <= per(60) + per(30) + 10);
        // 上限より大きいものは持たない。
        c.put("t", "huge", img(per(60) + per(30)), TTL);
        assert!(c.get("t", "huge").is_none());
    }

    #[test]
    fn long_keys_with_tiny_images_are_bounded() {
        // 8 バイトの画像を 2 KiB の URL で大量に入れても、鍵の分まで含めて上限に収まる。
        let c = ImageCache::new(64 * 1024, 10_000);
        for i in 0..1_000 {
            let url = format!("https://img.example.com/{i}/{}", "x".repeat(2048));
            c.put("t", &url, img(8), TTL);
        }
        let (map, total) = &*c.inner.lock().unwrap();
        assert!(*total <= 64 * 1024, "{total}");
        assert!(map.len() < 1_000, "古いものから捨てている");
    }

    #[test]
    fn entry_count_is_bounded() {
        let c = ImageCache::new(usize::MAX / 2, 3);
        for i in 0..10 {
            c.put("t", &i.to_string(), img(1), TTL);
        }
        assert_eq!(c.inner.lock().unwrap().0.len(), 3);
    }

    #[test]
    fn failures_are_cached_with_their_own_ttl() {
        let c = ImageCache::new(4096, 100);
        c.put(
            "t",
            "bad",
            Cached::Failed(StatusCode::BAD_GATEWAY),
            FAILURE_TTL,
        );
        assert!(matches!(
            c.get("t", "bad"),
            Some(Cached::Failed(StatusCode::BAD_GATEWAY))
        ));
        // 期限は 1 件ごと。失敗の期限が切れれば取り直せる。
        c.put(
            "t",
            "gone",
            Cached::Failed(StatusCode::NOT_FOUND),
            Duration::ZERO,
        );
        assert!(c.get("t", "gone").is_none());
        assert!(c.get("t", "bad").is_some(), "他の件の期限には影響しない");
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

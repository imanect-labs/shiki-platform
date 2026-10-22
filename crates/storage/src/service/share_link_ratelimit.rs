//! 共有リンク redeem の簡易レート制限（#342 レビュー B-3）。
//!
//! redeem は**認証済みなら誰でも任意 token を叩ける唯一のエンドポイント**で、パスワード総当たりと
//! （Argon2 検証を走らせる）CPU DoS の経路になり得る。principal 単位・token 単位の固定窓カウンタで
//! 1 レプリカあたりの試行速度を抑える。**プロセス内**の best-effort（各レプリカ独立・厳密な分散
//! 制限ではない）。基準時刻 `now` は呼び出し側が渡す（テスト決定性・SQL now との齟齬回避）。

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};

/// 固定窓カウンタ 1 個の状態（窓開始時刻・その窓での試行回数）。
struct Window {
    start: DateTime<Utc>,
    count: u32,
}

/// redeem のレート制限器。`key`（principal / token）ごとに固定窓で試行回数を数える。
pub(super) struct RedeemRateLimiter {
    window: Duration,
    max: u32,
    buckets: Mutex<HashMap<String, Window>>,
}

impl RedeemRateLimiter {
    /// `window` 秒あたり `max` 回まで許可する制限器。
    pub(super) fn new(window: Duration, max: u32) -> Self {
        RedeemRateLimiter {
            window,
            max,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// `key` の試行を 1 つ計上し、窓内上限を超えていなければ `true`（許可）を返す。
    ///
    /// 窓が経過していれば窓をリセットする。ロック毒化時は安全側（許可）に倒す（redeem 自体は
    /// パスワード検証で守られており、レート制限の一時的な失効で認可は破れない）。
    pub(super) fn check(&self, key: &str, now: DateTime<Utc>) -> bool {
        let Ok(mut buckets) = self.buckets.lock() else {
            return true;
        };
        // ときどき古い窓を掃除して無制限成長を防ぐ（同時に触れた鍵のみで十分・軽量）。
        buckets.retain(|_, w| now.signed_duration_since(w.start) < self.window);
        let w = buckets.entry(key.to_owned()).or_insert(Window {
            start: now,
            count: 0,
        });
        if now.signed_duration_since(w.start) >= self.window {
            w.start = now;
            w.count = 0;
        }
        w.count += 1;
        w.count <= self.max
    }
}

impl Default for RedeemRateLimiter {
    /// 既定: 60 秒あたり最大 20 回。principal / token いずれの鍵にも同じ器を使う（別インスタンス）。
    fn default() -> Self {
        RedeemRateLimiter::new(Duration::seconds(60), 20)
    }
}

/// principal 単位の鍵（テナント内で一意な principal.id をテナントで名前空間化）。
pub(super) fn principal_key(tenant_id: &str, principal_id: &str) -> String {
    format!("p:{tenant_id}|{principal_id}")
}

/// token 単位の鍵。token はテナント**かつ** org スコープでしか一致しない（redeem の lookup が
/// `tenant_id = $2 AND org = $3` で絞る）ので、鍵も同じ単位で名前空間化する（#817）。
/// 素の token だけだと、他テナント／他 org からの空打ちで自テナントのリンクの窓を使い切らせる
/// （redeem を DoS する）余地が残る。`tenant_id`・`org` は `|` を含まない（識別子ポリシー）。
pub(super) fn token_key(tenant_id: &str, org: &str, token: &str) -> String {
    format!("t:{tenant_id}|{org}|{token}")
}

/// 現在時刻での既定ヘルパ（本番経路）。
#[allow(dead_code)]
pub(super) fn now() -> DateTime<Utc> {
    Utc::now()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(secs: i64) -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(1_700_000_000 + secs, 0).unwrap()
    }

    #[test]
    fn allows_up_to_max_then_blocks_within_window() {
        let rl = RedeemRateLimiter::new(Duration::seconds(60), 3);
        assert!(rl.check("k", t(0)));
        assert!(rl.check("k", t(1)));
        assert!(rl.check("k", t(2)));
        assert!(!rl.check("k", t(3))); // 4 回目は上限超過。
    }

    #[test]
    fn resets_after_window() {
        let rl = RedeemRateLimiter::new(Duration::seconds(60), 2);
        assert!(rl.check("k", t(0)));
        assert!(rl.check("k", t(1)));
        assert!(!rl.check("k", t(2)));
        // 窓越え後はリセットされる。
        assert!(rl.check("k", t(61)));
    }

    #[test]
    fn token_key_is_scoped_by_tenant_and_org() {
        // 同じ token 文字列でも、テナント／org が違えば別の窓で数える。
        let rl = RedeemRateLimiter::new(Duration::seconds(60), 1);
        assert!(rl.check(&token_key("a-corp", "a-corp", "tok"), t(0)));
        assert!(!rl.check(&token_key("a-corp", "a-corp", "tok"), t(0)));
        // 他テナント・他 org の試行は a-corp の窓を消費しない（逆も同様）。
        assert!(rl.check(&token_key("b-corp", "a-corp", "tok"), t(0)));
        assert!(rl.check(&token_key("a-corp", "sales", "tok"), t(0)));
        assert_ne!(
            token_key("a-corp", "a-corp", "tok"),
            token_key("b-corp", "a-corp", "tok")
        );
        assert_ne!(
            principal_key("a-corp", "alice"),
            principal_key("b-corp", "alice")
        );
        // principal 鍵と token 鍵は接頭辞で衝突しない。
        assert_ne!(principal_key("a-corp", "x"), token_key("a-corp", "x", ""));
    }

    #[test]
    fn keys_are_independent() {
        let rl = RedeemRateLimiter::new(Duration::seconds(60), 1);
        assert!(rl.check("a", t(0)));
        assert!(!rl.check("a", t(0)));
        assert!(rl.check("b", t(0))); // 別鍵は独立。
    }
}

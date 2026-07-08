//! 失敗ループ検出（Task 5.5）。
//!
//! 盲目的リトライではなく、**同一の失敗の反復**を検出して安全停止/エスカレーションへ倒す。
//! ツール呼び出しの署名（名前＋入力ダイジェスト＋エラー有無）の直近履歴を見て、
//! 「同一署名のエラーが連続で閾値回」または「同一署名が窓内で過多」を検出する。
//!
//! 判定は**純粋**（内部状態＝直近署名リングのみ・時刻/乱数に依存しない）。入力ダイジェストは
//! `DefaultHasher`（固定鍵 SipHash・run 内決定的）で取り、生の引数を保持しない（監査/メモリ節約）。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// ツール呼び出しの署名（名前＋入力ダイジェスト＋エラー有無）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Signature {
    name_hash: u64,
    input_hash: u64,
    error: bool,
}

/// 失敗ループ検出器。直近 `window` 件の署名を保持する。
#[derive(Debug, Clone)]
pub struct LoopDetector {
    window: usize,
    /// 同一署名エラーの連続回数の閾値（これに達したらループ）。
    consecutive_threshold: usize,
    recent: Vec<Signature>,
}

impl Default for LoopDetector {
    fn default() -> Self {
        // 既定: 直近 12 件を見て、同一失敗が 3 連続でループ判定。
        LoopDetector::new(12, 3)
    }
}

impl LoopDetector {
    #[must_use]
    pub fn new(window: usize, consecutive_threshold: usize) -> Self {
        LoopDetector {
            window: window.max(1),
            consecutive_threshold: consecutive_threshold.max(2),
            recent: Vec::new(),
        }
    }

    /// 1 件のツール結果を観測し、ループに陥っているかを返す。
    ///
    /// `input` は入力 JSON、`error` はツールがエラーだったか。**エラーでない呼び出しは
    /// 連続失敗を断ち切る**（成功が挟まればループではない）。
    pub fn observe(&mut self, name: &str, input: &serde_json::Value, error: bool) -> bool {
        let sig = Signature {
            name_hash: hash_str(name),
            input_hash: hash_json(input),
            error,
        };
        self.recent.push(sig);
        if self.recent.len() > self.window {
            let overflow = self.recent.len() - self.window;
            self.recent.drain(0..overflow);
        }
        self.is_looping(sig)
    }

    /// 窓内で同一の失敗が閾値回**以上出現**していればループとみなす。
    ///
    /// 連続だけでなく「失敗 A・別の失敗 B・失敗 A …」のように**別の呼び出しを挟む同一失敗の反復**も
    /// 検出する（CodeRabbit 指摘）。ただし**同一 (ツール,入力) が一度でも成功したら、その成功以降の
    /// 失敗のみ数える**（成功＝自己修正が効いた合図なので過去の失敗をリセットし、誤検出しない）。
    fn is_looping(&self, last: Signature) -> bool {
        if !last.error {
            return false;
        }
        let same_call =
            |s: &Signature| s.name_hash == last.name_hash && s.input_hash == last.input_hash;
        // 同一 (ツール,入力) の直近成功位置。それ以降の失敗だけを数える。
        let start = self
            .recent
            .iter()
            .rposition(|s| same_call(s) && !s.error)
            .map_or(0, |i| i + 1);
        let count = self.recent[start..].iter().filter(|s| **s == last).count();
        count >= self.consecutive_threshold
    }
}

fn hash_str(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// JSON を正規化（キーソート）せず、`to_string` の安定表現でハッシュする。
/// serde_json の `Map` は `preserve_order` 無効なら BTreeMap 相当でキー順が安定するため決定的。
fn hash_json(v: &serde_json::Value) -> u64 {
    let mut h = DefaultHasher::new();
    v.to_string().hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::json;

    #[test]
    fn detects_three_consecutive_identical_errors() {
        let mut d = LoopDetector::new(12, 3);
        let inp = json!({"cmd": "boom"});
        assert!(!d.observe("shell", &inp, true));
        assert!(!d.observe("shell", &inp, true));
        assert!(d.observe("shell", &inp, true)); // 3 連続でループ
    }

    #[test]
    fn detects_interleaved_same_failure() {
        // 失敗 A・失敗 B・失敗 A・失敗 B・失敗 A → A が窓内で 3 回失敗（連続でなくても）ループ。
        let mut d = LoopDetector::new(12, 3);
        let a = json!({"cmd": "a"});
        let b = json!({"cmd": "b"});
        assert!(!d.observe("shell", &a, true));
        assert!(!d.observe("shell", &b, true));
        assert!(!d.observe("shell", &a, true));
        assert!(!d.observe("shell", &b, true));
        assert!(d.observe("shell", &a, true)); // A の 3 回目
    }

    #[test]
    fn success_resets_interleaved_count() {
        // 途中で A が成功したら、それ以前の A の失敗はリセット（自己修正が効いた）。
        let mut d = LoopDetector::new(12, 3);
        let a = json!({"cmd": "a"});
        d.observe("shell", &a, true);
        d.observe("shell", &a, true);
        d.observe("shell", &a, false); // 成功でリセット
        d.observe("shell", &a, true);
        assert!(!d.observe("shell", &a, true)); // 成功以降は 2 回のみ
    }

    #[test]
    fn success_breaks_the_streak() {
        let mut d = LoopDetector::new(12, 3);
        let inp = json!({"cmd": "boom"});
        d.observe("shell", &inp, true);
        d.observe("shell", &inp, false); // 成功が挟まる
        d.observe("shell", &inp, true);
        assert!(!d.observe("shell", &inp, true)); // 連続は 2 まで
    }

    #[test]
    fn different_input_is_not_a_loop() {
        let mut d = LoopDetector::new(12, 3);
        assert!(!d.observe("shell", &json!({"cmd": "a"}), true));
        assert!(!d.observe("shell", &json!({"cmd": "b"}), true));
        assert!(!d.observe("shell", &json!({"cmd": "c"}), true));
    }

    #[test]
    fn different_tool_same_input_not_a_loop() {
        let mut d = LoopDetector::new(12, 3);
        let inp = json!({"x": 1});
        d.observe("t1", &inp, true);
        d.observe("t2", &inp, true);
        assert!(!d.observe("t3", &inp, true));
    }

    #[test]
    fn successful_calls_never_loop() {
        let mut d = LoopDetector::new(4, 2);
        let inp = json!({"q": "same"});
        for _ in 0..20 {
            assert!(!d.observe("doc_search", &inp, false));
        }
    }

    proptest! {
        // 成功呼び出しは（何回続いても）決してループ判定にならない。
        #[test]
        fn ok_calls_never_trip(n in 1usize..30) {
            let mut d = LoopDetector::new(12, 3);
            let inp = json!({"k": "v"});
            let mut tripped = false;
            for _ in 0..n {
                tripped |= d.observe("tool", &inp, false);
            }
            prop_assert!(!tripped);
        }

        // 閾値未満の連続エラーではループにならない。
        #[test]
        fn below_threshold_never_trips(k in 1usize..3) {
            let mut d = LoopDetector::new(12, 3);
            let inp = json!({"cmd": "x"});
            let mut tripped = false;
            for _ in 0..k {
                tripped |= d.observe("shell", &inp, true);
            }
            prop_assert!(!tripped);
        }
    }
}

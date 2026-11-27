//! ブロックの実行中の panic（BUG-215・engine.md §9.5）。
//!
//! ブロックの実装が panic したら、ワーカーはそれを捕まえて step を失敗（[`NODE_PANIC_CODE`]・やり直さない）にし、次の
//! step を拾い続ける。捕まえないと、ワーカーのタスクが終わって起動し直されず、heartbeat がリースを延ばし続けて step と
//! run が実行中のまま残る（リースが切れないので取り消しも回収も効かない）。panic の文は内部の値やソースの位置を含みうる
//! ので、利用者には決まった文（[`NODE_PANIC_MESSAGE`]）だけを見せ、文はログにだけ残す（付録 C の §3.4.5）。

use std::any::Any;

use super::NodeResult;

/// panic した step の `error.code`（失敗の分類は「分類できない失敗」・[`super::failure::KNOWN_CODES`]）。
pub const NODE_PANIC_CODE: &str = "internal";

/// panic した step の誤りの文（実行履歴に出る・作成者には直せないので管理者に連絡するよう言う）。
pub const NODE_PANIC_MESSAGE: &str =
    "サーバーで問題が起きたため、このブロックを実行できませんでした。続くときは管理者に連絡してください。";

/// panic した step の結果。同じ入力ではまた panic するのがふつうなので、やり直さない（試す回数を使い切らせない）。
#[must_use]
pub fn node_panic_result() -> NodeResult {
    NodeResult::fail("internal", NODE_PANIC_MESSAGE, false)
}

/// panic の中身の文（ログに残す）。`panic!("…")` の `&str` と書式つきの `String` だけを読み、ほかは型が分からない。
#[must_use]
pub fn panic_text(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&'static str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("（文字でない panic）")
}

/// 落とすと止まる tokio のタスク。`JoinHandle` は落としてもタスクを止めないので、早く戻る道と panic の巻き戻しでも
/// 止まるよう、持ち主の値が落ちたときに `abort` する（heartbeat に使う）。
pub(crate) struct AbortOnDrop(pub(crate) tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_text_reads_str_and_string_payloads() {
        let s: Box<dyn Any + Send> = Box::new("boom");
        assert_eq!(panic_text(s.as_ref()), "boom");
        let owned: Box<dyn Any + Send> = Box::new(format!("index {} out of range", 3));
        assert_eq!(panic_text(owned.as_ref()), "index 3 out of range");
        let other: Box<dyn Any + Send> = Box::new(7_u8);
        assert_eq!(panic_text(other.as_ref()), "（文字でない panic）");
    }

    #[test]
    fn the_result_is_a_permanent_internal_failure_with_a_fixed_message() {
        let r = node_panic_result();
        let err = r.error.expect("失敗");
        assert_eq!(err.code, NODE_PANIC_CODE);
        assert_eq!(err.message, NODE_PANIC_MESSAGE);
        assert!(!err.retryable);
        assert_eq!(
            super::super::failure::category_of(NODE_PANIC_CODE),
            super::super::FailureCategory::Unknown
        );
    }

    /// 落とすとタスクが止まる（`JoinHandle` を落としただけでは止まらない）。
    #[tokio::test]
    async fn dropping_the_guard_aborts_the_task() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let guard = AbortOnDrop(tokio::spawn(async move {
            loop {
                let _ = tx.send(());
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }));
        assert!(rx.recv().await.is_some());
        drop(guard);
        // 止まったタスクは送り手を落とすので、溜まった分を読み切ると閉じる。
        let closed = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while rx.recv().await.is_some() {}
        })
        .await;
        assert!(closed.is_ok(), "タスクが止まらない");
    }

    /// 持ったまま panic で巻き戻っても、タスクが止まる（ワーカーが heartbeat を持ったまま panic したとき）。
    #[tokio::test]
    #[allow(clippy::panic)]
    async fn a_panic_while_holding_the_guard_aborts_the_task() {
        use futures::FutureExt;

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let work = async move {
            let _guard = AbortOnDrop(tokio::spawn(async move {
                loop {
                    let _ = tx.send(());
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            }));
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            panic!("boom");
        };
        let caught = std::panic::AssertUnwindSafe(work).catch_unwind().await;
        assert!(caught.is_err());
        let closed = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while rx.recv().await.is_some() {}
        })
        .await;
        assert!(closed.is_ok(), "タスクが止まらない");
    }
}

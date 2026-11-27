//! cron 評価（5 フィールド＋IANA tz・engine.md §5.2）。
//!
//! 読み方（曜日は 0 と 7 が日曜・BUG-95）は、ミニアプリの関数のスケジュールと共有する `shiki-cron5` が持つ
//! （写像を 2 つ持たない・BUG-129）。ここはその公開し直しで、`workflow_engine::scheduler::cron::*` の呼び出しは
//! そのまま使える。

pub use cron5::{
    latest_occurrence_between, next_after, next_occurrences, occurrences_between, validate,
    CronError,
};

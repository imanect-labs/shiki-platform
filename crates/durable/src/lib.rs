//! 耐久実行（durable execution）の下部構造プリミティブ（Task 10.0・engine.md §1.2）。
//!
//! chat の `generation_run` / `generation_event`（Task 3.11・#82）で確立した
//! claim・リース＋heartbeat・fencing token・`(id, seq)` 追記 exactly-once・
//! Redis pub/sub best-effort 配信を、テーブル非依存のプリミティブとして提供する。
//! chat run と workflow run は同じ run 抽象に乗るが、**キュー・レーン・優先度・状態機械は
//! 各ドメインが所有**する（本クレートは共有しない・engine.md §1.2 の分担表が正）。
//!
//! 提供する不変条件:
//! - **Idempotent Consumer ＋ Lease/Fencing**: [`claim`] は queued かリース失効 running を
//!   claim し fencing token を +1。以降の追記/確定（[`append_event`] / [`fenced_finalize`]）は
//!   fencing 一致時のみ通す（クラッシュ takeover ＋ゾンビ書込拒否）。
//! - **Append-only Event Log**: [`append_event`] は `(キー, seq)` 単調 seq を真実のソースへ
//!   追記する（exactly-once・重複 seq は主キーで拒否）。
//! - **DB=真実のソース／Redis=best-effort**: [`RedisPubSub`] は起床通知のみ。正しさは常に
//!   DB replay（[`replay_events`]）が担保する。
//!
//! テーブル・カラム名は [`RunTableSpec`] / [`EventTableSpec`] の `'static` 識別子でのみ
//! 与えられ、実行時入力が SQL 組み立てに混ざる経路はない。

mod claim;
mod events;
mod pubsub;
mod spec;

pub use claim::{claim, fenced_finalize, heartbeat};
pub use events::{append_event, append_event_unfenced, replay_events};
pub use pubsub::RedisPubSub;
pub use spec::{EventTableSpec, Key, KeyValue, RunTableSpec};

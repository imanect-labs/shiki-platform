//! CSV の集計を動かす隔離ランナーと、api 側の tabular が共有するもの（GAP-95）。
//!
//! 隔離ランナー（`shiki-tabular-runner`・PIT-39）はこのクレートだけに依存する。api 側の `shiki-tabular` は、ここを
//! 再エクスポートして同じ型・同じ上限・同じ検証を使う（`tabular::protocol`・`tabular::input`・`tabular::sql_guard`）。
//!
//! - [`protocol`]: ランナーとの JSON プロトコルの型（[`RunnerRequest`]・[`RunnerResponse`] ほか）と、結果の大きさの
//!   数え方・プロセスのアドレス空間の上限。
//! - [`input`]: アップロードされた Excel の上限（ランナーが展開と読み取りで確かめ、api はファイルの大きさを渡す前に
//!   確かめる）。
//! - [`sql_guard`]: SQL の読み取り専用の検証と、SQL が表として読む名前。
//!
//! ストレージ・認可・HTTP・DB のクレートには依存しない（ランナーの依存に入れないため）。

pub mod input;
pub mod protocol;
pub mod sql_guard;

pub use protocol::{CsvDialect, InputFormat, RunnerOp, RunnerRequest, RunnerResponse, SheetInfo};

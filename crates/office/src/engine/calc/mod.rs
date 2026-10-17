//! Excel（Calc）の操作（#725）。
//!
//! - `ops`: LLM が書く操作の語彙（単一定義）
//! - `normalize`: 検証とエンジン向けの形への正規化
//! - `view`: 読み取り結果を LLM 向けのテキストへ整形
//! - `run`: 接続済みセッションへの読み書き（読み戻しと検査つき）

pub mod describe;
pub mod normalize;
pub mod ops;
pub mod run;
pub mod view;

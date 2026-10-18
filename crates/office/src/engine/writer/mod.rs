//! Word（Writer）の操作（#725）。
//!
//! - `ops`: LLM が書く操作の語彙（単一定義）
//! - `html`: LLM が書いた HTML＋CSS を LibreOffice の取り込みが読める形へ整える
//! - `normalize`: 検証とエンジン向けの形への正規化
//! - `view`: 読み取り結果を LLM 向けに整形
//! - `run`: 接続済みセッションへの読み書き
//! - `describe`: ツールの説明文と入力スキーマ

pub mod describe;
pub mod html;
pub mod normalize;
pub mod ops;
pub mod run;
pub mod view;

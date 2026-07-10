//! クエリ実行チョークポイント（Task 9.3・PIT-21）。
//!
//! `data_record` への**全読取 SQL はこのモジュールだけが組み立てる**。行ポリシー述語
//! （[`crate::policy`]）は [`executor`] の各関数が無条件に AND 合成し、合成しない
//! SELECT を書ける場所を構造的に無くす（モジュール可視性 pub(crate)・crate 外非公開）。
//!
//! 書込（update/delete/遷移）の対象行ロックも読取述語つきの [`executor`] を通り、
//! 不可視行は存在しない扱い（404・rev オラクルなし）になる。

pub(crate) mod aggregate;
pub(crate) mod declarative;
pub(crate) mod executor;

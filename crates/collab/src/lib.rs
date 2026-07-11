//! ノート（md）共同編集の同期サブシステム（Phase 11-pre Task 11P.1・design §4.8.1）。
//!
//! # 設計（PIT-37 への回答）
//!
//! - **真実は Yjs ドキュメント**（update log ＋ snapshot・[`store`]）。md はシリアライズ
//!   形式であり、保存時に生成する（Task 11P.2）。
//! - **肥大化**: update が [`store::COMPACT_EVERY`] 件たまるごと＋最終切断時に snapshot へ
//!   圧縮し、取り込み済み update 行を削除する（PIT-37①）。
//! - **権限**: 対応する node の ReBAC（`file:<id>` の editor/viewer）を接続時＋
//!   30 秒ごとに `HigherConsistency` で再チェックし、剥奪で切断する（PIT-37②）。
//!   viewer は読めるが update を受理しない（fail-closed）。
//! - **ワイヤ**: y-websocket 互換（sync step1/2・update・awareness）。クライアントは
//!   標準の y-protocols 実装をそのまま使える。
//!
//! # md に落ちない情報（PIT-37③・往復対象外の正本一覧）
//!
//! 以下は md シリアライズ（Task 11P.2）の対象外であり、**Yjs snapshot（collab_doc.snapshot）
//! を正本**としてストレージに併置・保全する。md 保存で消失しない:
//!
//! - サジェスト提案マーク（AI 編集のサジェストモード・Task 11P.4）
//! - コメント・スレッドアンカー（将来）
//! - awareness（カーソル・プレゼンス）— 揮発情報のため永続化もしない
//! - 相対位置（RelativePosition）等の CRDT 内部参照

pub mod doc;
pub mod error;
pub mod hub;
pub mod session;
pub mod store;

pub use doc::LiveDoc;
pub use error::CollabError;
pub use hub::{AccessMode, CollabHub};
pub use session::run_session;
pub use store::{DocStore, PersistedDoc};

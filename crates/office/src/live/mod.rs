//! Collabora（CoolWSD）セッションへの AI headless 参加（issue #352・design §4.8）。
//!
//! ノート/スライドの `CollabHub::apply_ai_edit`（AI が共同編集エンジンに載る）と
//! 同じ思想を Collabora に揃える。AI は CoolWSD の**独立 view** として WS 接続し、
//! 自分のカーソル・選択でアンカー指定編集を行う:
//!
//! - ユーザーの選択に依存しない（承認〜適用間の選択ずれ＝TOCTOU が構造的に無い）
//! - 編集は CoolWSD の協調プロトコルで全 view へ即時反映される
//! - 参加者リストには WOPI CheckFileInfo 由来の「Shiki AI」が表示される
//! - 保存は CoolWSD 自身の WOPI PutFile → StorageService の既存チョークポイント
//!   （版・監査・outbox→RAG 再索引）を通る
//!
//! モジュール構成: `protocol`（ワイヤ形式の純関数）→ `client`（WS セッション）
//! → `session`（authz・直列化・ops 適用の [`LiveEditor`]）。

mod client;
mod error;
mod protocol;
mod session;

pub use client::{CoolWsClient, CoolWsConfig, SaveAck, SearchOutcome};
pub use error::LiveError;
pub use protocol::{is_cell_ref, Loaded};
pub use session::{
    CellValue, LiveEditError, LiveEditReport, LiveEditor, LiveOp, LiveOpResult, LiveSaveResult,
};

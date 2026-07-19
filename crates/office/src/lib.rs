//! Office 統合（docs/design.md §4.8・Task 11.5/11.6）。
//!
//! - [`OfficeSuite`] トレイト: Collabora Online のラップ（OnlyOffice への差し替え退路）。
//!   discovery（編集アクション URL）をキャッシュ付きで解決する。
//! - WOPI ホスト: CheckFileInfo/GetFile/PutFile/Lock 系を **StorageService の
//!   一クライアント**として実装する（チョークポイント維持・直バケット禁止）。
//!
//! セキュリティ設計（PIT-11 / PIT-44）:
//! - WOPI access_token は（実行主体×ファイル×短寿命・HMAC-SHA256）の自己完結トークン。
//!   クレームに tenant_id/org を焼き込み、他テナントのファイルに流用できない。
//! - **トークンは UX 用であり権限の根拠ではない**。毎 WOPI 呼び出しで OpenFGA check
//!   （`HigherConsistency`）を行い、共有解除を次の呼び出しで即時反映する（fail-closed）。
//! - WOPI ロックは 30 分 TTL の助言的ロック（lazy 解放）。編集排他ではなく
//!   「AI を提案保存へ迂回させるシグナル」（PIT-44・Task 11.8 が `current_lock` で判定）。

mod compose;
mod edit;
mod error;
mod suite;
pub mod wopi;

pub use compose::{DocxComposer, DOCX_CONTENT_TYPE};
pub use edit::{
    EditOpResult, EditOutcome, EditReport, OfficeEditor, SavedEdit, EDITABLE_CONTENT_TYPES,
};
pub use error::OfficeError;
pub use suite::{CollaboraSuite, OfficeSuite, SUPPORTED_EXTENSIONS};
pub use wopi::lock::{current_lock, LockInfo};
pub use wopi::routes::{build_wopi_router, WopiState};
pub use wopi::token::{OfficeTokenKey, WopiClaims, TOKEN_TTL};

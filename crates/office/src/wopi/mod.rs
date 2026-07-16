//! WOPI ホスト（Task 11.6・design §4.8）。
//!
//! Collabora（WOPI クライアント）から呼ばれる CheckFileInfo/GetFile/PutFile/Lock 系。
//! **StorageService の一クライアント**であり、実体（オブジェクトストア）へ直接
//! 触れない（チョークポイント維持）。認可は access_token の検証**後**に
//! 毎呼び出しの OpenFGA check（`HigherConsistency`）で行う（PIT-11・fail-closed）。

pub mod lock;
pub mod routes;
pub mod token;

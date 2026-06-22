//! shiki-storage — StorageService・ObjectStore（Phase 1 ストレージ基盤）。
//!
//! 設計上の不変条件（docs/design.md §4.2, architecture-invariants）:
//! - **単一チョークポイント**: ファイル/フォルダの全 read/write は [`StorageService`] 経由。
//!   権限（OpenFGA）・監査（[`audit`]）・content-addressing をここで必ず担保する。
//! - **アンビエント権限の禁止**: 全公開メソッドは第 1 引数に `&AuthContext` を取る。
//! - **差し替えはトレイト裏で**: バイトの実体は [`ObjectStore`](object_store::ObjectStore)
//!   トレイト裏（MinIO 実装。GCS は Phase 8）。
//! - **presigned URL 方式**: バイトはクライアント↔オブジェクトストア直転送し、アプリは
//!   presigned URL の発行（認可・監査つき）と server-side メタ操作のみ（PIT-6）。

pub mod audit;
pub mod content_address;
pub mod error;
pub mod model;
pub mod object_store;
pub mod service;

pub use error::StorageError;
pub use model::{DownloadTicket, Node, NodeKind, UploadOutcome};
pub use object_store::{ObjectStore, ObjectStoreError, S3Config, S3ObjectStore};
pub use service::StorageService;

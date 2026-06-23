//! `ObjectStore` トレイト（差し替え点）と S3/MinIO 実装。
//!
//! バイトの実体はこのトレイト裏に閉じ込め、cloud/onprem の差（MinIO↔GCS）を
//! 実装差し替えで吸収する（docs/design.md §3.1）。Phase 1 は **presigned URL 方式**:
//! バイトはクライアント↔オブジェクトストア直で転送し、StorageService は presigned URL の
//! 発行（認可・監査つき）と server-side のメタ操作（copy/head/delete）だけを担う（PIT-6）。

use std::time::Duration;

pub mod s3;

pub use s3::{S3Config, S3ObjectStore};

#[derive(Debug, thiserror::Error)]
pub enum ObjectStoreError {
    #[error("オブジェクトが存在しません: {0}")]
    NotFound(String),
    #[error("オブジェクトストアのエラー: {0}")]
    Backend(String),
    #[error("presigned URL 発行に失敗: {0}")]
    Presign(String),
}

/// オブジェクトストア（content-addressed blob の置き場）の抽象。
///
/// 直アクセス禁止の不変条件を守るため、これを公開するのは StorageService のみ。
#[async_trait::async_trait]
pub trait ObjectStore: Send + Sync {
    /// バケットの存在（と CORS）を保証する。起動時に一度呼ぶ。
    async fn ensure_bucket(&self) -> Result<(), ObjectStoreError>;

    /// アップロード用 presigned PUT URL を発行する（staging キー宛て）。
    /// `content_length` を署名に含め、クライアントが宣言サイズと異なるバイト数を
    /// アップロードできないように束縛する（巨大オブジェクトの押し込みを防ぐ）。
    async fn presign_put(
        &self,
        key: &str,
        ttl: Duration,
        content_length: i64,
    ) -> Result<String, ObjectStoreError>;

    /// ダウンロード用 presigned GET URL を発行する。
    /// `filename`/`content_type` は response ヘッダ上書き（ブラウザ DL の挙動制御）。
    async fn presign_get(
        &self,
        key: &str,
        ttl: Duration,
        filename: Option<&str>,
        content_type: Option<&str>,
    ) -> Result<String, ObjectStoreError>;

    /// オブジェクトを server-side で読み、`(sha256 hex, バイト数)` を返す（finalize の再ハッシュ用）。
    /// 内容を逐次読みしながらハッシュするため、巨大ファイルでもメモリに載せきらない。
    /// 対象が存在しない場合は [`ObjectStoreError::NotFound`]。
    async fn read_and_hash(&self, key: &str) -> Result<(String, u64), ObjectStoreError>;

    /// オブジェクトの存在確認。
    async fn exists(&self, key: &str) -> Result<bool, ObjectStoreError>;

    /// server-side copy（staging → content-addressed への昇格。バイトはアプリを通らない）。
    async fn copy(&self, src_key: &str, dst_key: &str) -> Result<(), ObjectStoreError>;

    /// オブジェクトを削除する（staging の後始末・blob GC）。
    async fn delete(&self, key: &str) -> Result<(), ObjectStoreError>;
}

//! MinIO(S3 互換) 実装。GCS 実装は Phase 8 で同 trait 裏に追加する。
//!
//! presigned URL は **公開エンドポイント**で署名する必要がある（ブラウザのアクセス先と
//! 署名ホストを一致させる）。一方 head/copy/delete 等の server-side 操作は**内部
//! エンドポイント**で叩く。よって 2 つのクライアントを保持する。

use std::time::Duration;

use aws_sdk_s3::{
    config::{BehaviorVersion, Credentials, Region},
    presigning::PresigningConfig,
    types::{CorsConfiguration, CorsRule},
    Client,
};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;

use super::{ObjectStore, ObjectStoreError};
use crate::content_address::Sha256Hasher;

fn default_region() -> String {
    "us-east-1".to_string()
}

fn default_get_ttl() -> u64 {
    120
}

fn default_put_ttl() -> u64 {
    900
}

/// MinIO/S3 接続設定。`StorageConfig.s3` として API の設定から渡る。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct S3Config {
    /// server→MinIO の内部エンドポイント（例 `http://minio:9000`）。head/copy/delete に使う。
    pub internal_endpoint: String,
    /// presigned URL の署名に使う公開エンドポイント（ブラウザ到達可能・例 `http://localhost:9000`）。
    pub public_endpoint: String,
    pub bucket: String,
    pub access_key: String,
    pub secret_key: String,
    #[serde(default = "default_region")]
    pub region: String,
    /// ダウンロード presigned URL の TTL（秒）。短く保ち失効ウィンドウを絞る（PIT-6/PIT-11）。
    #[serde(default = "default_get_ttl")]
    pub presign_get_ttl_secs: u64,
    /// アップロード presigned URL の TTL（秒）。
    #[serde(default = "default_put_ttl")]
    pub presign_put_ttl_secs: u64,
    /// ブラウザ直 PUT/GET を許可する CORS オリジン（空なら CORS 設定をスキップ）。
    #[serde(default)]
    pub cors_allowed_origins: Vec<String>,
}

impl S3Config {
    pub fn presign_get_ttl(&self) -> Duration {
        Duration::from_secs(self.presign_get_ttl_secs)
    }

    pub fn presign_put_ttl(&self) -> Duration {
        Duration::from_secs(self.presign_put_ttl_secs)
    }
}

/// S3 互換オブジェクトストア（MinIO）。
pub struct S3ObjectStore {
    /// server-side 操作（head/copy/delete/get）用。内部エンドポイント。
    internal: Client,
    /// presigned URL 署名用。公開エンドポイント。
    presign: Client,
    bucket: String,
    cors_allowed_origins: Vec<String>,
}

impl S3ObjectStore {
    pub fn new(cfg: &S3Config) -> Self {
        S3ObjectStore {
            internal: build_client(&cfg.internal_endpoint, cfg),
            presign: build_client(&cfg.public_endpoint, cfg),
            bucket: cfg.bucket.clone(),
            cors_allowed_origins: cfg.cors_allowed_origins.clone(),
        }
    }

    async fn put_bucket_cors(&self) -> Result<(), ObjectStoreError> {
        let rule = CorsRule::builder()
            .set_allowed_origins(Some(self.cors_allowed_origins.clone()))
            .allowed_methods("GET")
            .allowed_methods("PUT")
            .allowed_methods("HEAD")
            .allowed_headers("*")
            .expose_headers("ETag")
            .expose_headers("x-amz-checksum-sha256")
            .max_age_seconds(3600)
            .build()
            .map_err(|e| ObjectStoreError::Backend(format!("CORS ルール構築: {e}")))?;
        let cors = CorsConfiguration::builder()
            .cors_rules(rule)
            .build()
            .map_err(|e| ObjectStoreError::Backend(format!("CORS 構成構築: {e}")))?;
        self.internal
            .put_bucket_cors()
            .bucket(&self.bucket)
            .cors_configuration(cors)
            .send()
            .await
            .map_err(|e| ObjectStoreError::Backend(format!("put_bucket_cors: {e}")))?;
        Ok(())
    }
}

fn build_client(endpoint: &str, cfg: &S3Config) -> Client {
    let creds = Credentials::new(
        cfg.access_key.clone(),
        cfg.secret_key.clone(),
        None,
        None,
        "shiki-static",
    );
    let conf = aws_sdk_s3::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new(cfg.region.clone()))
        .endpoint_url(endpoint)
        .credentials_provider(creds)
        // MinIO は path-style（`http://host/bucket/key`）が必要。
        .force_path_style(true)
        .build();
    Client::from_conf(conf)
}

#[async_trait::async_trait]
impl ObjectStore for S3ObjectStore {
    async fn ensure_bucket(&self) -> Result<(), ObjectStoreError> {
        let exists = self
            .internal
            .head_bucket()
            .bucket(&self.bucket)
            .send()
            .await
            .is_ok();
        if !exists {
            // 既存（BucketAlreadyOwnedByYou 等）は冪等に握りつぶす。
            if let Err(e) = self
                .internal
                .create_bucket()
                .bucket(&self.bucket)
                .send()
                .await
            {
                let msg = format!("{e:?}");
                if !(msg.contains("BucketAlreadyOwnedByYou") || msg.contains("BucketAlreadyExists"))
                {
                    return Err(ObjectStoreError::Backend(format!("create_bucket: {e}")));
                }
            }
        }
        if !self.cors_allowed_origins.is_empty() {
            self.put_bucket_cors().await?;
        }
        Ok(())
    }

    async fn presign_put(&self, key: &str, ttl: Duration) -> Result<String, ObjectStoreError> {
        let pc = PresigningConfig::expires_in(ttl)
            .map_err(|e| ObjectStoreError::Presign(e.to_string()))?;
        let req = self
            .presign
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .presigned(pc)
            .await
            .map_err(|e| ObjectStoreError::Presign(format!("put_object presign: {e}")))?;
        Ok(req.uri().to_string())
    }

    async fn presign_get(
        &self,
        key: &str,
        ttl: Duration,
        filename: Option<&str>,
        content_type: Option<&str>,
    ) -> Result<String, ObjectStoreError> {
        let pc = PresigningConfig::expires_in(ttl)
            .map_err(|e| ObjectStoreError::Presign(e.to_string()))?;
        let mut builder = self.presign.get_object().bucket(&self.bucket).key(key);
        if let Some(ct) = content_type {
            builder = builder.response_content_type(ct);
        }
        if let Some(name) = filename {
            // " を含むファイル名は壊れた header を避けるため除去する。
            let safe = name.replace('"', "");
            builder =
                builder.response_content_disposition(format!("attachment; filename=\"{safe}\""));
        }
        let req = builder
            .presigned(pc)
            .await
            .map_err(|e| ObjectStoreError::Presign(format!("get_object presign: {e}")))?;
        Ok(req.uri().to_string())
    }

    async fn read_and_hash(&self, key: &str) -> Result<(String, u64), ObjectStoreError> {
        let resp = self
            .internal
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| {
                let se = e.into_service_error();
                if se.is_no_such_key() {
                    ObjectStoreError::NotFound(key.to_string())
                } else {
                    ObjectStoreError::Backend(format!("get_object: {se}"))
                }
            })?;
        // 逐次読みしながら sha256 を計算する（メモリにバッファし切らない）。
        let mut reader = resp.body.into_async_read();
        let mut hasher = Sha256Hasher::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = reader
                .read(&mut buf)
                .await
                .map_err(|e| ObjectStoreError::Backend(format!("read body: {e}")))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(hasher.finalize())
    }

    async fn exists(&self, key: &str) -> Result<bool, ObjectStoreError> {
        match self
            .internal
            .head_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(_) => Ok(true),
            Err(e) => {
                let se = e.into_service_error();
                if se.is_not_found() {
                    Ok(false)
                } else {
                    Err(ObjectStoreError::Backend(format!("head_object: {se}")))
                }
            }
        }
    }

    async fn copy(&self, src_key: &str, dst_key: &str) -> Result<(), ObjectStoreError> {
        // copy_source は `bucket/key`。org/sha256/uuid 由来でパス安全な文字のみ。
        let copy_source = format!("{}/{}", self.bucket, src_key);
        self.internal
            .copy_object()
            .bucket(&self.bucket)
            .key(dst_key)
            .copy_source(copy_source)
            .send()
            .await
            .map_err(|e| ObjectStoreError::Backend(format!("copy_object: {e}")))?;
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), ObjectStoreError> {
        self.internal
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| ObjectStoreError::Backend(format!("delete_object: {e}")))?;
        Ok(())
    }
}

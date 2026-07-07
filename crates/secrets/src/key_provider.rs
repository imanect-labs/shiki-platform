//! `KeyProvider` トレイト（マスターキーで DEK を包む/解く）。
//!
//! cloud=Cloud KMS / onprem=ローカルキーファイル（将来 HSM）をトレイト裏に隠す
//! （差し替えはトレイト裏で・design §1）。Stage A は [`LocalKeyFileProvider`] のみ実装する。

use std::path::Path;

use async_trait::async_trait;

use crate::crypto::{self, CryptoError, KeyGuard, KEY_LEN};

/// マスターキーで包んだ DEK（暗号文＋nonce）。
#[derive(Debug, Clone)]
pub struct WrappedKey {
    pub encrypted_dek: Vec<u8>,
    pub nonce: Vec<u8>,
    /// どの provider が包んだか（ローテーション/移行の判別・DB の key_provider 列）。
    pub provider_id: String,
}

/// DEK を包む/解くマスターキープロバイダ。
///
/// 実装はマスターキーを外部（KMS / キーファイル）に持ち、DEK 平文をプロセス外へ出さない。
#[async_trait]
pub trait KeyProvider: Send + Sync {
    /// provider の識別子（DB へ記録し、解く際に一致を確認する）。
    fn id(&self) -> &str;

    /// DEK 平文をマスターキーで包む。
    async fn wrap(&self, dek: &[u8]) -> Result<WrappedKey, CryptoError>;

    /// 包まれた DEK を解いて 32 バイトの DEK を返す（[`KeyGuard`] でゼロ化管理）。
    async fn unwrap(&self, wrapped: &WrappedKey) -> Result<KeyGuard, CryptoError>;
}

/// ローカルキーファイル実装（オンプレ既定）。
///
/// マスターキー（32 バイト・base64 or 生バイト）をファイルから読み、AES-256-GCM で DEK を包む。
/// マスターキーはメモリ上に保持し、`KeyGuard` の Drop でゼロ化する運用を推奨する
/// （本実装はプロセス生存中保持・鍵ローテーションはファイル差し替え＋再ラップで対応）。
pub struct LocalKeyFileProvider {
    master_key: [u8; KEY_LEN],
    id: String,
}

impl LocalKeyFileProvider {
    /// マスターキーのバイト列から作る（32 バイト必須）。
    pub fn from_bytes(master_key: [u8; KEY_LEN]) -> Self {
        LocalKeyFileProvider {
            master_key,
            id: "local-key-file".to_string(),
        }
    }

    /// ファイルからマスターキーを読み込む（base64 デコードを試み、失敗時は生バイト）。
    pub fn from_file(path: &Path) -> Result<Self, CryptoError> {
        let raw = std::fs::read(path).map_err(|_| CryptoError::BadKeyLength)?;
        let key_bytes = decode_master_key(&raw)?;
        Ok(Self::from_bytes(key_bytes))
    }
}

/// マスターキーのバイト列を 32 バイトへ正規化する（base64 or 生バイト）。
fn decode_master_key(raw: &[u8]) -> Result<[u8; KEY_LEN], CryptoError> {
    // 生バイトが 32 バイトならそのまま。
    if raw.len() == KEY_LEN {
        let mut key = [0u8; KEY_LEN];
        key.copy_from_slice(raw);
        return Ok(key);
    }
    // base64（改行を許容）としてデコードを試みる。
    use base64::Engine;
    let trimmed: Vec<u8> = raw
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&trimmed)
        .map_err(|_| CryptoError::BadKeyLength)?;
    if decoded.len() != KEY_LEN {
        return Err(CryptoError::BadKeyLength);
    }
    let mut key = [0u8; KEY_LEN];
    key.copy_from_slice(&decoded);
    Ok(key)
}

#[async_trait]
impl KeyProvider for LocalKeyFileProvider {
    fn id(&self) -> &str {
        &self.id
    }

    async fn wrap(&self, dek: &[u8]) -> Result<WrappedKey, CryptoError> {
        let sealed = crypto::seal(&self.master_key, dek)?;
        Ok(WrappedKey {
            encrypted_dek: sealed.ciphertext,
            nonce: sealed.nonce,
            provider_id: self.id.clone(),
        })
    }

    async fn unwrap(&self, wrapped: &WrappedKey) -> Result<KeyGuard, CryptoError> {
        if wrapped.provider_id != self.id {
            return Err(CryptoError::Decrypt);
        }
        let dek = crypto::open(&self.master_key, &wrapped.nonce, &wrapped.encrypted_dek)?;
        if dek.len() != KEY_LEN {
            return Err(CryptoError::BadKeyLength);
        }
        let mut key = [0u8; KEY_LEN];
        key.copy_from_slice(&dek);
        Ok(KeyGuard(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::generate_key;

    #[tokio::test]
    async fn wrap_unwrap_roundtrip() {
        let provider = LocalKeyFileProvider::from_bytes(generate_key());
        let dek = generate_key();
        let wrapped = provider.wrap(&dek).await.expect("wrap");
        assert_eq!(wrapped.provider_id, "local-key-file");
        let unwrapped = provider.unwrap(&wrapped).await.expect("unwrap");
        assert_eq!(unwrapped.0, dek);
    }

    #[tokio::test]
    async fn provider_mismatch_fails() {
        let provider = LocalKeyFileProvider::from_bytes(generate_key());
        let dek = generate_key();
        let mut wrapped = provider.wrap(&dek).await.expect("wrap");
        wrapped.provider_id = "other".into();
        assert!(provider.unwrap(&wrapped).await.is_err());
    }

    #[test]
    fn decode_master_key_variants() {
        // 生 32 バイト。
        let raw = [7u8; KEY_LEN];
        assert_eq!(decode_master_key(&raw).unwrap(), raw);
        // base64。
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode([9u8; KEY_LEN]);
        assert_eq!(decode_master_key(b64.as_bytes()).unwrap(), [9u8; KEY_LEN]);
        // 不正長。
        assert!(decode_master_key(b"short").is_err());
    }
}

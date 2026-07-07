//! envelope encryption の暗号プリミティブ（AES-256-GCM）。
//!
//! - 平文は毎回新しい DEK（32 バイト）で暗号化する。
//! - DEK は [`KeyProvider`](crate::KeyProvider) がマスターキーで包む（本モジュールは対称暗号のみ担う）。
//! - nonce は 96bit をランダム生成し暗号文と共に保存する（GCM の要件）。

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use rand::RngCore;
use zeroize::Zeroize;

/// AES-256-GCM の nonce 長（96bit）。
pub(crate) const NONCE_LEN: usize = 12;
/// DEK / マスターキー長（256bit）。
pub(crate) const KEY_LEN: usize = 32;

/// 暗号処理のエラー（詳細はログのみ・値は漏らさない）。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CryptoError {
    #[error("暗号化に失敗しました")]
    Encrypt,
    #[error("復号に失敗しました（改竄または鍵不一致）")]
    Decrypt,
    #[error("鍵長が不正です")]
    BadKeyLength,
}

/// 暗号文と nonce の対。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Sealed {
    pub ciphertext: Vec<u8>,
    pub nonce: Vec<u8>,
}

/// 32 バイトの鍵をランダム生成する（DEK 用）。使用後は [`zeroize`] すること。
pub(crate) fn generate_key() -> [u8; KEY_LEN] {
    let mut key = [0u8; KEY_LEN];
    rand::thread_rng().fill_bytes(&mut key);
    key
}

/// `key`（32 バイト）で `plaintext` を AES-256-GCM 暗号化する。
pub(crate) fn seal(key: &[u8], plaintext: &[u8]) -> Result<Sealed, CryptoError> {
    if key.len() != KEY_LEN {
        return Err(CryptoError::BadKeyLength);
    }
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|_| CryptoError::Encrypt)?;
    Ok(Sealed {
        ciphertext,
        nonce: nonce_bytes.to_vec(),
    })
}

/// `key`（32 バイト）と `nonce` で `ciphertext` を復号する。
pub(crate) fn open(key: &[u8], nonce: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if key.len() != KEY_LEN {
        return Err(CryptoError::BadKeyLength);
    }
    if nonce.len() != NONCE_LEN {
        return Err(CryptoError::Decrypt);
    }
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|_| CryptoError::Decrypt)
}

/// 使用後に鍵バッファをゼロ化するガード（Drop で消す）。
pub struct KeyGuard(pub [u8; KEY_LEN]);

impl Drop for KeyGuard {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_roundtrip() {
        let key = generate_key();
        let sealed = seal(&key, b"super secret token").expect("seal");
        let opened = open(&key, &sealed.nonce, &sealed.ciphertext).expect("open");
        assert_eq!(opened, b"super secret token");
    }

    #[test]
    fn wrong_key_fails_decrypt() {
        let key = generate_key();
        let other = generate_key();
        let sealed = seal(&key, b"x").expect("seal");
        assert_eq!(
            open(&other, &sealed.nonce, &sealed.ciphertext),
            Err(CryptoError::Decrypt)
        );
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let key = generate_key();
        let mut sealed = seal(&key, b"tamper me").expect("seal");
        sealed.ciphertext[0] ^= 0xff;
        assert_eq!(
            open(&key, &sealed.nonce, &sealed.ciphertext),
            Err(CryptoError::Decrypt)
        );
    }

    #[test]
    fn bad_key_length() {
        assert_eq!(seal(b"short", b"x"), Err(CryptoError::BadKeyLength));
        assert_eq!(
            open(b"short", &[0; 12], b"x"),
            Err(CryptoError::BadKeyLength)
        );
    }

    #[test]
    fn distinct_nonces_per_seal() {
        let key = generate_key();
        let a = seal(&key, b"same").expect("a");
        let b = seal(&key, b"same").expect("b");
        // nonce がランダムなので同一平文でも暗号文が変わる。
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(a.ciphertext, b.ciphertext);
    }
}

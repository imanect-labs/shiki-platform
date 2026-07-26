//! マニフェスト署名（Task 9.13b・ed25519）。
//!
//! 署名対象は **canonical manifest digest**（[`crate::manifest_digest`]・sha256 hex）の
//! UTF-8 バイト列。バンドル改竄はマニフェスト内の `frontend.sha256` / `server` 参照が
//! digest に含まれることで検知される（バンドル差し替え → digest 不一致 → 署名不一致）。
//! 秘密鍵はサーバに置かない（署名は CLI/CI 側・Task 9.14）。検証のみをここで行う。

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

use crate::{manifest_digest, AppPlatformError, MiniAppManifest};

/// マニフェストの署名を検証する（公開鍵は 32 バイト raw ed25519）。
///
/// **fail-closed**: 鍵/署名の形式不正・不一致はすべて `Forbidden`（詳細を呼び出し側で
/// 出し分けさせない＝オラクルを作らない）。
pub fn verify_manifest_signature(
    manifest: &MiniAppManifest,
    signature: &[u8],
    public_key: &[u8],
) -> Result<(), AppPlatformError> {
    let digest = manifest_digest(manifest)?;
    verify_digest_signature(&digest, signature, public_key)
}

/// digest 文字列（sha256 hex）への署名を検証する（マニフェスト非依存の一般形・#344）。
///
/// skill レジストリ（署名対象 = skill body の正規化 JSON digest）等、マニフェスト以外の
/// 成果物の first-party 署名検証で共用する。fail-closed 方針は
/// [`verify_manifest_signature`] と同一（形式不正・不一致はすべて `Forbidden`）。
pub fn verify_digest_signature(
    digest: &str,
    signature: &[u8],
    public_key: &[u8],
) -> Result<(), AppPlatformError> {
    let key_bytes: [u8; 32] = public_key
        .try_into()
        .map_err(|_| AppPlatformError::Forbidden)?;
    let key = VerifyingKey::from_bytes(&key_bytes).map_err(|_| AppPlatformError::Forbidden)?;
    let sig_bytes: [u8; 64] = signature
        .try_into()
        .map_err(|_| AppPlatformError::Forbidden)?;
    let sig = Signature::from_bytes(&sig_bytes);
    key.verify(digest.as_bytes(), &sig)
        .map_err(|_| AppPlatformError::Forbidden)
}

/// digest 文字列へ署名する（CLI/テスト用・秘密鍵 32 バイト raw・#344）。
pub fn sign_digest(digest: &str, secret_key: &[u8]) -> Result<Vec<u8>, AppPlatformError> {
    let key_bytes: [u8; 32] = secret_key
        .try_into()
        .map_err(|_| AppPlatformError::Invalid("秘密鍵は 32 バイトです".into()))?;
    let key = SigningKey::from_bytes(&key_bytes);
    Ok(key.sign(digest.as_bytes()).to_bytes().to_vec())
}

/// マニフェストへ署名する（CLI/テスト用・秘密鍵 32 バイト raw）。
pub fn sign_manifest(
    manifest: &MiniAppManifest,
    secret_key: &[u8],
) -> Result<Vec<u8>, AppPlatformError> {
    let digest = manifest_digest(manifest)?;
    sign_digest(&digest, secret_key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::TrustTier;

    fn manifest(name: &str) -> MiniAppManifest {
        MiniAppManifest {
            name: name.into(),
            version: "1.0.0".into(),
            description: String::new(),
            requested_scopes: vec!["data.read".into()],
            tools: vec![],
            tables: vec![],
            workflows: vec![],
            budget: crate::manifest::Budget::default(),
            frontend: None,
            server: None,
            trust_tier: TrustTier::FirstParty,
        }
    }

    #[test]
    fn sign_verify_roundtrip_and_tamper_detection() {
        let secret = [7u8; 32];
        let public = SigningKey::from_bytes(&secret).verifying_key().to_bytes();
        let m = manifest("expense");
        let sig = sign_manifest(&m, &secret).unwrap();
        assert!(verify_manifest_signature(&m, &sig, &public).is_ok());

        // マニフェスト改竄 → 検証失敗。
        let tampered = manifest("expense-evil");
        assert!(verify_manifest_signature(&tampered, &sig, &public).is_err());
        // 署名改竄 → 検証失敗。
        let mut bad = sig.clone();
        bad[0] ^= 0xff;
        assert!(verify_manifest_signature(&m, &bad, &public).is_err());
        // 別鍵 → 検証失敗。
        let other = SigningKey::from_bytes(&[9u8; 32])
            .verifying_key()
            .to_bytes();
        assert!(verify_manifest_signature(&m, &sig, &other).is_err());
        // 形式不正（長さ）→ fail-closed。
        assert!(verify_manifest_signature(&m, &sig[..10], &public).is_err());
        assert!(verify_manifest_signature(&m, &sig, &public[..8]).is_err());
    }
}

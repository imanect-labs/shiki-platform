//! StorageService: 共有リンク（#342）— 純粋ヘルパ（broad subject 解決・token 生成・パスワード）。
//!
//! reconcile/redeem/失効の各モジュールが共有する小さな関数群（`pub(super)`）。副作用は無い。

#[allow(clippy::wildcard_imports)]
use super::*;

use crate::model::GeneralAccessLevel;

/// audience に対応する broad な共有先 subject（restricted は `None`＝付与ゼロの純ポインタ）。
/// `organization` → `organization:<tenant>|<org>#member`、`anyone` → `user:*`。
pub(super) fn broad_subject(
    ns: &Namespace<'_>,
    level: GeneralAccessLevel,
    org: &str,
) -> Option<Subject> {
    match level {
        GeneralAccessLevel::Organization => Some(ns.organization_member(org)),
        GeneralAccessLevel::Anyone => Some(Subject::public()),
        GeneralAccessLevel::Restricted => None,
    }
}

/// FGA object の型プレフィクスからノード種別を判定する（`folder:` 以外は file 扱い）。
pub(super) fn kind_of(obj: &FgaObject) -> NodeKind {
    if obj.as_str().starts_with("folder:") {
        NodeKind::Folder
    } else {
        NodeKind::File
    }
}

/// URL/redeem 起点の不透明トークンを生成する（CSPRNG 32byte を hex 化・64 文字）。
/// 一意性は `node_share_link.token` の unique 制約＋衝突時リトライで担保する。
pub(super) fn new_share_token() -> String {
    use argon2::password_hash::rand_core::{OsRng, RngCore};
    let mut buf = [0u8; 32];
    OsRng.fill_bytes(&mut buf);
    let mut s = String::with_capacity(64);
    for b in buf {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// パスワードを Argon2id で PHC 文字列にハッシュ化する（ソルトは CSPRNG）。
pub(super) fn hash_password(password: &str) -> Result<String, StorageError> {
    use argon2::password_hash::rand_core::OsRng;
    use argon2::password_hash::{PasswordHasher, SaltString};
    use argon2::Argon2;

    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|_| StorageError::Integrity("パスワードハッシュ生成に失敗しました".into()))
}

/// パスワードを PHC 文字列に対して検証する（定数時間・失敗はすべて false へ潰す＝オラクル防止）。
pub(super) fn verify_password(password: &str, phc: &str) -> bool {
    use argon2::password_hash::{PasswordHash, PasswordVerifier};
    use argon2::Argon2;

    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{hash_password, new_share_token, verify_password};

    #[test]
    fn password_hash_roundtrips_and_rejects_wrong() {
        let phc = hash_password("s3cret-passphrase").unwrap();
        assert!(phc.starts_with("$argon2"));
        assert!(verify_password("s3cret-passphrase", &phc));
        assert!(!verify_password("wrong", &phc));
        assert!(!verify_password("x", "not-a-phc-string"));
    }

    #[test]
    fn password_hashes_are_salted_unique() {
        let a = hash_password("same").unwrap();
        let b = hash_password("same").unwrap();
        assert_ne!(a, b);
        assert!(verify_password("same", &a));
        assert!(verify_password("same", &b));
    }

    #[test]
    fn share_tokens_are_unique_hex() {
        let a = new_share_token();
        let b = new_share_token();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}

//! コンテンツアドレッシングのハッシュ計算とオブジェクトキー組み立て。
//!
//! blob は内容の sha256（hex）をキーに保存し、自動重複排除する（Task 1.2）。
//! PIT-14 に従い、キーは **`{tenant_id}/{org}/{sha256}`** の名前空間に閉じる。
//! SaaS では同一 org slug を複数テナントが共有し得るため、tenant_id を最上位に織り込んで
//! 越境のハッシュ存在オラクル・dedup 共有・refcount 破壊を防ぐ（SAAS.1・#84）。

use sha2::{Digest, Sha256};

/// ストリーミング sha256 ハッシャ。チャンクごとに [`update`](Self::update) し、
/// 最後に [`finalize`](Self::finalize) で `(hex, byte長)` を得る。
///
/// presigned アップロードでは内容をサーバが直接観測できないため、finalize 時に
/// staging オブジェクトを読み戻して本ハッシャで再計算し、宣言値と照合する。
#[derive(Default)]
pub struct Sha256Hasher {
    inner: Sha256,
    len: u64,
}

impl Sha256Hasher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, chunk: &[u8]) {
        self.inner.update(chunk);
        self.len += chunk.len() as u64;
    }

    /// `(sha256 hex 小文字, 総バイト数)` を返す。
    pub fn finalize(self) -> (String, u64) {
        (hex::encode(self.inner.finalize()), self.len)
    }
}

/// 一括 sha256（小データ・テスト用）。
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// content-addressed blob の ObjectStore キー（PIT-14: `{tenant_id}/{org}` 名前空間）。
/// ミニアプリ・フロントバンドルのオブジェクトキー（Task 9.11・content-addressed）。
///
/// ユーザーファイル（node/blob）とは別枠のアプリ資産。キー体系をここ（storage）に置くのは
/// 書込側（app-platform BundleStore）と配信側（app-gateway 第3リスナ）のキー drift を防ぐため。
pub fn miniapp_bundle_key(tenant_id: &str, sha256: &str) -> String {
    format!("miniapp-bundle/{tenant_id}/{sha256}")
}

pub fn blob_object_key(tenant_id: &str, org: &str, sha256: &str) -> String {
    format!("{tenant_id}/{org}/{sha256}")
}

/// 昇格前 staging オブジェクトのキー（`{tenant_id}/{org}/staging/{upload_id}`）。
/// クライアントが presigned PUT で書き込む唯一のキー（可変）。
pub fn staging_object_key(tenant_id: &str, org: &str, upload_id: &str) -> String {
    format!("{tenant_id}/{org}/staging/{upload_id}")
}

/// finalize 時の不変スナップショットキー（`{tenant_id}/{org}/incoming/{upload_id}`）。
/// staging を server-side copy した直後はクライアントが触れないため、
/// ハッシュ検証と content-addressed への昇格をこのキー基準で race-free に行う（TOCTOU 回避）。
pub fn incoming_object_key(tenant_id: &str, org: &str, upload_id: &str) -> String {
    format!("{tenant_id}/{org}/incoming/{upload_id}")
}

/// sha256 hex として妥当か（64 桁の小文字 16 進）。
pub fn is_valid_sha256_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vectors() {
        // 空入力の sha256 は既知値。
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn streaming_equals_oneshot() {
        // チャンク分割で update しても一括ハッシュと一致すること。
        let mut hasher = Sha256Hasher::new();
        hasher.update(b"hello ");
        hasher.update(b"world");
        let (hex, len) = hasher.finalize();
        assert_eq!(hex, sha256_hex(b"hello world"));
        assert_eq!(len, 11);
    }

    #[test]
    fn object_keys_are_tenant_and_org_scoped() {
        assert_eq!(
            blob_object_key("acme", "sales", "deadbeef"),
            "acme/sales/deadbeef"
        );
        assert_eq!(
            staging_object_key("acme", "sales", "abc-123"),
            "acme/sales/staging/abc-123"
        );
        assert_eq!(
            incoming_object_key("acme", "sales", "abc-123"),
            "acme/sales/incoming/abc-123"
        );
        // 同一 org slug でも tenant が違えば別名前空間（越境 dedup を防ぐ）。
        assert_ne!(
            blob_object_key("t1", "sales", "h"),
            blob_object_key("t2", "sales", "h")
        );
    }

    #[test]
    fn sha256_hex_validation() {
        assert!(is_valid_sha256_hex(&sha256_hex(b"x")));
        assert!(!is_valid_sha256_hex("short"));
        assert!(!is_valid_sha256_hex(&"A".repeat(64))); // 大文字は不可
        assert!(!is_valid_sha256_hex(&"g".repeat(64))); // 16 進外は不可
    }
}

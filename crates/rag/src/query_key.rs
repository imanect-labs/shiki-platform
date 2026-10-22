//! 監査に残す検索語ハッシュの鍵（#833）。
//!
//! `rag.search` の監査行は検索語そのものを残さず、`HMAC-SHA256(鍵, tenant_id \0 検索語)` を残す。
//! 鍵はサーバだけが持つので、監査閲覧者（管理者でなくてもなれる）が「この語で検索した人がいるか」を
//! 自分で計算して確かめることはできない。同じ鍵のあいだは同じ語が同じ値になるので、
//! 同じ検索語の記録をまとめて見ることはできる。
//!
//! `SHIKI__RAG__QUERY_HASH_KEY` 未設定ならプロセス起動時に乱数生成する（再起動や別レプリカとは
//! 同じ語でも値が揃わない＝突き合わせられないだけで、逆算はできない）。

use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;

/// 設定注入する鍵の最小長（バイト）。弱すぎる鍵を弾く。
const MIN_KEY_LEN: usize = 32;

/// 検索語ハッシュの鍵。`Debug` に値を出さない（設定のログ出力から漏らさない）。
#[derive(Clone, Deserialize)]
#[serde(try_from = "String")]
pub struct QueryHashKey(Vec<u8>);

impl QueryHashKey {
    /// プロセス起動時の乱数鍵（32 バイト・CSPRNG）。
    #[must_use]
    pub fn random() -> Self {
        use rand::RngCore;
        let mut key = vec![0u8; MIN_KEY_LEN];
        rand::rngs::OsRng.fill_bytes(&mut key);
        QueryHashKey(key)
    }

    /// 検索語のハッシュ（16 進 64 桁）。tenant を混ぜ、テナントをまたいで値が揃わないようにする。
    #[must_use]
    pub fn digest(&self, tenant_id: &str, query: &str) -> String {
        use std::fmt::Write;
        // HMAC は任意長の鍵を受けるので失敗しない。
        let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(&self.0) else {
            unreachable!("HMAC は任意長の鍵を受ける")
        };
        mac.update(tenant_id.as_bytes());
        mac.update(b"\x00");
        mac.update(query.as_bytes());
        mac.finalize()
            .into_bytes()
            .iter()
            .fold(String::new(), |mut s, b| {
                let _ = write!(s, "{b:02x}");
                s
            })
    }
}

impl TryFrom<String> for QueryHashKey {
    type Error = String;

    fn try_from(secret: String) -> Result<Self, Self::Error> {
        if secret.len() < MIN_KEY_LEN {
            return Err(format!(
                "rag.query_hash_key は {MIN_KEY_LEN} バイト以上が必要です"
            ));
        }
        Ok(QueryHashKey(secret.into_bytes()))
    }
}

impl std::fmt::Debug for QueryHashKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("QueryHashKey(..)")
    }
}

/// 空文字は未設定として扱う（compose は未設定の env を空文字で渡す）。
pub(crate) fn empty_key_as_none<'de, D>(d: D) -> Result<Option<QueryHashKey>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<String>::deserialize(d)? {
        Some(s) if !s.is_empty() => QueryHashKey::try_from(s)
            .map(Some)
            .map_err(serde::de::Error::custom),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;

    fn key(s: &str) -> QueryHashKey {
        QueryHashKey::try_from(s.to_string()).expect("32 バイト以上")
    }

    #[test]
    fn same_key_same_value_and_tenant_separates() {
        let k = key("0123456789abcdef0123456789abcdef");
        assert_eq!(k.digest("a", "X社 買収"), k.digest("a", "X社 買収"));
        assert_ne!(k.digest("a", "X社 買収"), k.digest("b", "X社 買収"));
        assert_eq!(k.digest("a", "q").len(), 64);
    }

    #[test]
    fn cannot_be_recomputed_without_the_key() {
        // 鍵が違えば値が違う。鍵なしの sha256(tenant \0 語)（旧方式）とも一致しない。
        let a = key("0123456789abcdef0123456789abcdef");
        let b = key("fedcba9876543210fedcba9876543210");
        assert_ne!(a.digest("t", "q"), b.digest("t", "q"));
        let plain = Sha256::digest(b"t\x00q")
            .iter()
            .fold(String::new(), |mut s, b| {
                use std::fmt::Write;
                let _ = write!(s, "{b:02x}");
                s
            });
        assert_ne!(a.digest("t", "q"), plain);
        assert_ne!(
            QueryHashKey::random().digest("t", "q"),
            QueryHashKey::random().digest("t", "q")
        );
    }

    #[test]
    fn rejects_short_key_and_hides_value_in_debug() {
        assert!(QueryHashKey::try_from("short".to_string()).is_err());
        let k = key("0123456789abcdef0123456789abcdef");
        assert!(!format!("{k:?}").contains("0123"));
    }

    #[test]
    fn empty_env_is_unset() {
        #[derive(Deserialize)]
        struct C {
            #[serde(default, deserialize_with = "empty_key_as_none")]
            k: Option<QueryHashKey>,
        }
        let c: C = serde_json::from_str(r#"{"k":""}"#).expect("空文字");
        assert!(c.k.is_none());
        let c: C = serde_json::from_str("{}").expect("未設定");
        assert!(c.k.is_none());
        assert!(serde_json::from_str::<C>(r#"{"k":"short"}"#).is_err());
    }
}

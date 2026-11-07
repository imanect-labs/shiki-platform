//! 検証済みのテナント識別子 [`TenantId`]。
//!
//! `tenant_id` は SaaS の隔離境界で、FGA 識別子 `<type>:<tenant_id>|<local_id>` とオブジェクトキー
//! `{tenant_id}/{org}/...` の両方の名前空間になる。以前は `String` のまま [`AuthContext`](crate::AuthContext)
//! に入り、文字ポリシー（[`validate_tenant_id`]）を通るのは api 層の入口だけだった。背景ジョブや
//! サービス identity が DB の値などから文脈を組む経路では検証が素通りし、空文字の tenant も作れた。
//!
//! [`TenantId`] は [`TenantId::parse`]（＝[`validate_tenant_id`]）を通さないと作れない。
//! `AuthContext` はこの型しか受け取らないので、「未検証の tenant_id を持つ認可文脈」は型の上で作れない。

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::ident::{validate_tenant_id, IdentViolation};

/// 文字ポリシー（[`validate_tenant_id`]）を通ったテナント識別子。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TenantId(String);

impl TenantId {
    /// 文字ポリシーを検証して `TenantId` を作る（fail-closed）。
    ///
    /// claim・CLI 引数・API 入力・DB の値のいずれ由来でも、この関数を通す。
    ///
    /// # Errors
    ///
    /// 空・予約名・禁止文字（`|` `:` `#` `@` `/`・空白・制御文字）を含むとき。
    pub fn parse(value: impl Into<String>) -> Result<Self, IdentViolation> {
        let value = value.into();
        validate_tenant_id(&value)?;
        Ok(TenantId(value))
    }

    /// 文字列として参照する。
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 所有する文字列を取り出す。
    pub fn into_string(self) -> String {
        self.0
    }
}

impl AsRef<str> for TenantId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::ops::Deref for TenantId {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TenantId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<TenantId> for String {
    fn from(value: TenantId) -> Self {
        value.0
    }
}

impl TryFrom<String> for TenantId {
    type Error = IdentViolation;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        TenantId::parse(value)
    }
}

impl TryFrom<&str> for TenantId {
    type Error = IdentViolation;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        TenantId::parse(value)
    }
}

impl PartialEq<str> for TenantId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for TenantId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<String> for TenantId {
    fn eq(&self, other: &String) -> bool {
        &self.0 == other
    }
}

impl Serialize for TenantId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

/// 逆直列化でも文字ポリシーを通す（セッションストア等から読み戻した値も信頼しない）。
impl<'de> Deserialize<'de> for TenantId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        TenantId::parse(value).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_valid_and_rejects_invalid() {
        assert_eq!(TenantId::parse("acme-saas").unwrap().as_str(), "acme-saas");
        assert_eq!(TenantId::parse(""), Err(IdentViolation::Empty));
        assert_eq!(
            TenantId::parse("a|b"),
            Err(IdentViolation::ForbiddenChar('|'))
        );
        assert_eq!(TenantId::parse("a b"), Err(IdentViolation::Whitespace));
        assert_eq!(TenantId::parse(".."), Err(IdentViolation::Reserved));
    }

    #[test]
    fn behaves_like_a_string() {
        let t = TenantId::parse("t1").unwrap();
        assert_eq!(t, "t1");
        assert_eq!(t, *"t1");
        assert_eq!(t, "t1".to_string());
        assert_eq!(format!("{t}/org"), "t1/org");
        assert_eq!(t.len(), 2);
        assert_eq!(String::from(t), "t1");
    }

    #[test]
    fn serde_round_trips_and_validates() {
        let t = TenantId::parse("t1").unwrap();
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(json, "\"t1\"");
        assert_eq!(serde_json::from_str::<TenantId>(&json).unwrap(), t);
        assert!(serde_json::from_str::<TenantId>("\"a|b\"").is_err());
        assert!(serde_json::from_str::<TenantId>("\"\"").is_err());
    }
}

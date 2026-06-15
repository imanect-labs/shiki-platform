//! OpenFGA のオブジェクト識別子・サブジェクトの型安全ラッパ。
//!
//! `type:id` / `user:id` の文字列組み立てを一箇所に閉じ込め、
//! 呼び出し側が生文字列を組まないようにする（[`vocab`](crate::vocab) と対）。

use crate::vocab::ObjectType;

/// OpenFGA のオブジェクト識別子 `type:id`。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FgaObject(String);

impl FgaObject {
    /// 任意の object type と id から構築する。
    pub fn new(object_type: ObjectType, id: &str) -> Self {
        FgaObject(format!("{}:{}", object_type.as_str(), id))
    }

    pub fn organization(id: &str) -> Self {
        Self::new(ObjectType::Organization, id)
    }

    pub fn department(id: &str) -> Self {
        Self::new(ObjectType::Department, id)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for FgaObject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// OpenFGA のサブジェクト（ユーザー）`user:id`。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Subject(String);

impl Subject {
    pub fn user(id: &str) -> Self {
        Subject(format!("{}:{}", ObjectType::User.as_str(), id))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Subject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_format() {
        assert_eq!(
            FgaObject::organization("acme").as_str(),
            "organization:acme"
        );
        assert_eq!(Subject::user("alice").as_str(), "user:alice");
    }
}

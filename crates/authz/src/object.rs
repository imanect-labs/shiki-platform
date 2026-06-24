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

    /// ロールオブジェクト `role:<id>`（テナント内メンバーシップ集合・階層対応）。
    pub fn role(id: &str) -> Self {
        Self::new(ObjectType::Role, id)
    }

    /// ストレージのフォルダオブジェクト `folder:<id>`。
    pub fn folder(id: &str) -> Self {
        Self::new(ObjectType::Folder, id)
    }

    /// ストレージのファイルオブジェクト `file:<id>`（認可の最小オブジェクト）。
    pub fn file(id: &str) -> Self {
        Self::new(ObjectType::File, id)
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

    /// オブジェクトを subject として参照する（userset 親子の結線に使う）。
    ///
    /// 例: `file:<id>#parent@folder:<parent>` の右辺 `folder:<parent>`。
    /// ReBAC では subject が `user:` 以外（オブジェクト参照）になり得るため、
    /// [`FgaObject`] からそのまま subject 文字列を作る経路を用意する。
    pub fn object(object: &FgaObject) -> Self {
        Subject(object.as_str().to_string())
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

    // --- FgaObject ---

    #[test]
    fn fga_object_new_uses_object_type_prefix() {
        // new は object type の文字列表現を prefix として `type:id` を組むこと。
        assert_eq!(FgaObject::new(ObjectType::User, "u1").as_str(), "user:u1");
        assert_eq!(FgaObject::new(ObjectType::Role, "r1").as_str(), "role:r1");
        assert_eq!(
            FgaObject::new(ObjectType::Organization, "o1").as_str(),
            "organization:o1"
        );
    }

    #[test]
    fn fga_object_role_constructor() {
        // role ショートカットコンストラクタ。
        assert_eq!(FgaObject::role("sales").as_str(), "role:sales");
    }

    #[test]
    fn fga_object_storage_constructors() {
        // folder/file ショートカットコンストラクタ（Phase 1 ストレージ）。
        assert_eq!(FgaObject::folder("f1").as_str(), "folder:f1");
        assert_eq!(FgaObject::file("doc1").as_str(), "file:doc1");
    }

    #[test]
    fn fga_object_display_matches_as_str() {
        // Display 実装は as_str と一致すること。
        let obj = FgaObject::organization("acme");
        assert_eq!(obj.to_string(), "organization:acme");
        assert_eq!(obj.to_string(), obj.as_str());
    }

    #[test]
    fn fga_object_empty_id() {
        // id が空でも `type:` 形式になること（境界）。
        assert_eq!(FgaObject::organization("").as_str(), "organization:");
    }

    #[test]
    fn fga_object_id_with_colon() {
        // id に colon を含んでいてもそのまま連結されること（境界）。
        assert_eq!(
            FgaObject::new(ObjectType::User, "ns:alice").as_str(),
            "user:ns:alice"
        );
    }

    #[test]
    fn fga_object_eq_and_hash() {
        // 同じ type/id は等価、異なれば非等価。Hash でも区別されること。
        use std::collections::HashSet;
        let a = FgaObject::organization("acme");
        let b = FgaObject::organization("acme");
        let c = FgaObject::organization("other");
        assert_eq!(a, b);
        assert_ne!(a, c);
        let mut set = HashSet::new();
        set.insert(a.clone());
        set.insert(b);
        set.insert(c);
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn fga_object_clone_is_equal() {
        // Clone は等価なオブジェクトを生むこと。
        let a = FgaObject::role("r1");
        assert_eq!(a.clone(), a);
    }

    // --- Subject ---

    #[test]
    fn subject_user_prefix() {
        // Subject::user は常に `user:` prefix を付けること。
        assert_eq!(Subject::user("bob").as_str(), "user:bob");
    }

    #[test]
    fn subject_from_object_keeps_type_prefix() {
        // Subject::object はオブジェクトの `type:id` をそのまま subject にすること。
        assert_eq!(
            Subject::object(&FgaObject::folder("f1")).as_str(),
            "folder:f1"
        );
    }

    #[test]
    fn subject_display_matches_as_str() {
        // Display 実装は as_str と一致すること。
        let s = Subject::user("alice");
        assert_eq!(s.to_string(), "user:alice");
        assert_eq!(s.to_string(), s.as_str());
    }

    #[test]
    fn subject_empty_id() {
        // 空 id でも `user:` 形式（境界）。
        assert_eq!(Subject::user("").as_str(), "user:");
    }

    #[test]
    fn subject_eq_and_hash() {
        // 等価性と Hash の区別を確認する。
        use std::collections::HashSet;
        let a = Subject::user("alice");
        let b = Subject::user("alice");
        let c = Subject::user("bob");
        assert_eq!(a, b);
        assert_ne!(a, c);
        let mut set = HashSet::new();
        set.insert(a.clone());
        set.insert(b);
        set.insert(c);
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn subject_clone_is_equal() {
        // Clone は等価。
        let s = Subject::user("x");
        assert_eq!(s.clone(), s);
    }
}

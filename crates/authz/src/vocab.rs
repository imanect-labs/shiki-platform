//! 認可語彙の単一ソース（Single Source of Truth）。
//!
//! OpenFGA の relation 名・object type 名をここの enum でのみ定義する。
//! Rust enum が正本であり、`#[derive(TS)]` で TypeScript 型を生成して
//! フロント/ミニアプリ側も同じ閉じた集合を共有する（docs/design.md §4.1）。
//!
//! Phase 0 は骨格のみ（organization/department/user, member/parent）。
//! 後続フェーズで folder/file/thread/doc_chunk と viewer/editor/owner 等を追加する。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// ReBAC の relation 名。OpenFGA タプル `object#relation@subject` の `relation`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    /// あるオブジェクトのメンバー（例: 部署/組織のメンバー）。
    Member,
    /// 親オブジェクトへの継承関係（例: department → organization）。
    Parent,
}

impl Relation {
    /// OpenFGA に送出する文字列表現。
    pub const fn as_str(self) -> &'static str {
        match self {
            Relation::Member => "member",
            Relation::Parent => "parent",
        }
    }
}

impl std::fmt::Display for Relation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// ReBAC の object type 名。OpenFGA オブジェクト識別子 `type:id` の `type`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ObjectType {
    Organization,
    Department,
    User,
}

impl ObjectType {
    /// OpenFGA に送出する文字列表現。
    pub const fn as_str(self) -> &'static str {
        match self {
            ObjectType::Organization => "organization",
            ObjectType::Department => "department",
            ObjectType::User => "user",
        }
    }
}

impl std::fmt::Display for ObjectType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relation_str_roundtrip() {
        assert_eq!(Relation::Member.as_str(), "member");
        assert_eq!(Relation::Parent.as_str(), "parent");
    }

    #[test]
    fn object_type_str() {
        assert_eq!(ObjectType::Organization.as_str(), "organization");
        assert_eq!(ObjectType::User.as_str(), "user");
    }

    // --- Relation ---

    #[test]
    fn relation_display_matches_as_str() {
        // Display 実装は as_str と一致すること。
        assert_eq!(Relation::Member.to_string(), "member");
        assert_eq!(Relation::Parent.to_string(), "parent");
        assert_eq!(Relation::Member.to_string(), Relation::Member.as_str());
        assert_eq!(Relation::Parent.to_string(), Relation::Parent.as_str());
    }

    #[test]
    fn relation_serialize_snake_case() {
        // serde は snake_case で（OpenFGA 送出語彙と一致）シリアライズすること。
        assert_eq!(
            serde_json::to_string(&Relation::Member).unwrap(),
            "\"member\""
        );
        assert_eq!(
            serde_json::to_string(&Relation::Parent).unwrap(),
            "\"parent\""
        );
    }

    #[test]
    fn relation_deserialize_snake_case() {
        // snake_case 文字列から正しくデシリアライズできること。
        let member: Relation = serde_json::from_str("\"member\"").unwrap();
        let parent: Relation = serde_json::from_str("\"parent\"").unwrap();
        assert_eq!(member, Relation::Member);
        assert_eq!(parent, Relation::Parent);
    }

    #[test]
    fn relation_roundtrip_via_serde() {
        // serialize → deserialize のラウンドトリップで同値に戻ること。
        for r in [Relation::Member, Relation::Parent] {
            let json = serde_json::to_string(&r).unwrap();
            let back: Relation = serde_json::from_str(&json).unwrap();
            assert_eq!(r, back);
        }
    }

    #[test]
    fn relation_deserialize_unknown_fails() {
        // 閉じた集合外の relation はデシリアライズに失敗すること（負例）。
        let result: Result<Relation, _> = serde_json::from_str("\"owner\"");
        assert!(result.is_err());
    }

    #[test]
    fn relation_derives_eq_hash_clone() {
        // Copy / PartialEq / Hash 由来の挙動を確認する。
        use std::collections::HashSet;
        let a = Relation::Member;
        let b = a; // Copy
        assert_eq!(a, b);
        assert_ne!(Relation::Member, Relation::Parent);
        let mut set = HashSet::new();
        set.insert(Relation::Member);
        set.insert(Relation::Member);
        set.insert(Relation::Parent);
        assert_eq!(set.len(), 2);
    }

    // --- ObjectType ---

    #[test]
    fn object_type_all_variants_as_str() {
        // 全 variant の文字列表現を確認する。
        assert_eq!(ObjectType::Organization.as_str(), "organization");
        assert_eq!(ObjectType::Department.as_str(), "department");
        assert_eq!(ObjectType::User.as_str(), "user");
    }

    #[test]
    fn object_type_display_matches_as_str() {
        // Display 実装は as_str と一致すること。
        for ot in [
            ObjectType::Organization,
            ObjectType::Department,
            ObjectType::User,
        ] {
            assert_eq!(ot.to_string(), ot.as_str());
        }
    }

    #[test]
    fn object_type_serialize_snake_case() {
        // serde は snake_case でシリアライズすること。
        assert_eq!(
            serde_json::to_string(&ObjectType::Organization).unwrap(),
            "\"organization\""
        );
        assert_eq!(
            serde_json::to_string(&ObjectType::Department).unwrap(),
            "\"department\""
        );
        assert_eq!(
            serde_json::to_string(&ObjectType::User).unwrap(),
            "\"user\""
        );
    }

    #[test]
    fn object_type_roundtrip_via_serde() {
        // serialize → deserialize のラウンドトリップで同値に戻ること。
        for ot in [
            ObjectType::Organization,
            ObjectType::Department,
            ObjectType::User,
        ] {
            let json = serde_json::to_string(&ot).unwrap();
            let back: ObjectType = serde_json::from_str(&json).unwrap();
            assert_eq!(ot, back);
        }
    }

    #[test]
    fn object_type_deserialize_unknown_fails() {
        // 閉じた集合外の object type はデシリアライズに失敗すること（負例）。
        let result: Result<ObjectType, _> = serde_json::from_str("\"folder\"");
        assert!(result.is_err());
    }

    #[test]
    fn object_type_derives_eq_hash_copy() {
        // Copy / PartialEq / Hash 由来の挙動を確認する。
        use std::collections::HashSet;
        let a = ObjectType::User;
        let b = a; // Copy
        assert_eq!(a, b);
        assert_ne!(ObjectType::User, ObjectType::Organization);
        let mut set = HashSet::new();
        set.insert(ObjectType::User);
        set.insert(ObjectType::User);
        set.insert(ObjectType::Organization);
        set.insert(ObjectType::Department);
        assert_eq!(set.len(), 3);
    }
}

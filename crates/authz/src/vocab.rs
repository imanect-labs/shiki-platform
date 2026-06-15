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
}

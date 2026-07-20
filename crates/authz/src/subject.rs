//! OpenFGA のサブジェクト（タプル右辺）の型安全ラッパ。
//!
//! `user:<id>` / オブジェクト参照 / userset（`object#relation`）の組み立てを
//! [`crate::object`]（識別子側）と対で一箇所に閉じ込める。生 id からの構築は
//! [`crate::Namespace`] 経由のみ（テナント名前空間化・SAAS.1）。

use crate::object::FgaObject;
use crate::vocab::ObjectType;

/// OpenFGA のサブジェクト（ユーザー）`user:id`。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Subject(String);

impl Subject {
    /// ユーザー subject `user:<id>`。tenant 名前空間化は [`Namespace::user`] が担うため
    /// アプリ側からの生 id 構築を防ぐ `pub(crate)`。
    pub(crate) fn user(id: &str) -> Self {
        Subject(format!("{}:{}", ObjectType::User.as_str(), id))
    }

    /// 型束縛パブリックワイルドカード subject `user:*`（type-bound public access）。
    ///
    /// 一般アクセス「すべての認証済みユーザー」を表す。**意図的に tenant 名前空間化しない**
    /// （OpenFGA のワイルドカードは subject 側でグローバル）。テナント隔離は付与先の
    /// **オブジェクトが名前空間化されている**こと（`file:<tenant>|<id>`）で担保する:
    /// アクセス側は自テナントの識別子でしかオブジェクトを組めないため、`user:*` タプルは
    /// 実質「同一テナント内の任意ユーザー」にしか効かない（越境は構造的に不能）。
    pub fn public() -> Self {
        Subject(format!("{}:*", ObjectType::User.as_str()))
    }

    /// オブジェクトを subject として参照する（userset 親子の結線に使う）。
    ///
    /// 例: `file:<id>#parent@folder:<parent>` の右辺 `folder:<parent>`。
    /// ReBAC では subject が `user:` 以外（オブジェクト参照）になり得るため、
    /// [`FgaObject`] からそのまま subject 文字列を作る経路を用意する。
    pub fn object(object: &FgaObject) -> Self {
        Subject(object.as_str().to_string())
    }

    /// userset（`object#relation`）を subject として参照する。
    ///
    /// 例: ロール階層の結線 `role:営業部#member@role:営業1課#member` の右辺
    /// `role:営業1課#member`（配下ロールのメンバー集合を親ロールに含める）。
    /// `role` 型の `member: [user, role#member]` のように直接型へ userset を許す
    /// relation のタプルを、チョークポイント（[`AuthzClient`](crate::AuthzClient)）
    /// 経由で構築するための経路。
    pub fn userset(object: &FgaObject, relation: crate::vocab::Relation) -> Self {
        Subject(format!("{}#{}", object.as_str(), relation.as_str()))
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
    fn subject_public_is_user_wildcard() {
        // Subject::public は type-bound public `user:*`（テナント名前空間化しない）。
        assert_eq!(Subject::public().as_str(), "user:*");
    }

    #[test]
    fn subject_userset_appends_relation() {
        // Subject::userset は `object#relation` を生成すること（ロール階層・共有の結線に使う）。
        use crate::vocab::Relation;
        assert_eq!(
            Subject::userset(&FgaObject::role("sales-sec1"), Relation::Member).as_str(),
            "role:sales-sec1#member"
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

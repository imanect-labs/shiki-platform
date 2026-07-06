//! OpenFGA のオブジェクト識別子・サブジェクトの型安全ラッパ。
//!
//! `type:id` / `user:id` の文字列組み立てを一箇所に閉じ込め、
//! 呼び出し側が生文字列を組まないようにする（[`vocab`](crate::vocab) と対）。
//!
//! # テナント名前空間化（SAAS.1）
//!
//! SaaS は全テナント共有の単一 OpenFGA ストアを使うため、識別子へ tenant を織り込んで
//! **越境タプルを構造的に不能化**する。識別子は `<type>:<tenant_id>|<local_id>` の形を取り、
//! 生の local id から識別子を組む経路は [`Namespace`] に一本化する。`FgaObject` /
//! [`Subject::user`] の生コンストラクタは `pub(crate)` に閉じ、アプリ側（storage / api）は
//! [`AuthContext::ns`](crate::AuthContext::ns) から得た [`Namespace`] 経由でしか識別子を
//! 構築できない（tenant を渡さずに構築できない ＝ 型レベルで境界を強制する）。

use crate::vocab::{ObjectType, Relation};

/// tenant と local id を FGA 識別子へ織り込む区切り文字。
///
/// AD group パス（`/` を含む）と衝突しないよう `|` を使う。tenant_id 側はこの文字を
/// 含まないことを解決時（`resolve_tenant_id`）に検証するため、parse-back は最初の
/// `<tenant>|` 一致で安全に local を切り出せる。
pub const TENANT_SEP: char = '|';

/// OpenFGA のオブジェクト識別子 `type:id`。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FgaObject(String);

impl FgaObject {
    /// 任意の object type と id から構築する。
    ///
    /// tenant 名前空間化は [`Namespace`] が担う。生の local id からの直接構築を防ぐため
    /// `pub(crate)`（アプリ側は [`Namespace`] 経由で構築する）。
    pub(crate) fn new(object_type: ObjectType, id: &str) -> Self {
        FgaObject(format!("{}:{}", object_type.as_str(), id))
    }

    pub(crate) fn organization(id: &str) -> Self {
        Self::new(ObjectType::Organization, id)
    }

    /// ロールオブジェクト `role:<id>`（テナント内メンバーシップ集合・階層対応）。
    pub(crate) fn role(id: &str) -> Self {
        Self::new(ObjectType::Role, id)
    }

    /// ストレージのフォルダオブジェクト `folder:<id>`。
    pub(crate) fn folder(id: &str) -> Self {
        Self::new(ObjectType::Folder, id)
    }

    /// ストレージのファイルオブジェクト `file:<id>`（認可の最小オブジェクト）。
    pub(crate) fn file(id: &str) -> Self {
        Self::new(ObjectType::File, id)
    }

    /// チャットスレッドオブジェクト `thread:<id>`（会話・ReBAC 共有の単位・#37）。
    pub(crate) fn thread(id: &str) -> Self {
        Self::new(ObjectType::Thread, id)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// tenant に束縛した識別子ビルダ（SAAS.1 のチョークポイント）。
///
/// [`AuthContext::ns`](crate::AuthContext::ns) から得る。全メソッドが local id を
/// `<tenant_id>|<local_id>` へ織り込んでから [`FgaObject`] / [`Subject`] を組むため、
/// このビルダを通す限りテナント境界を越えるタプル/問い合わせは構築できない。
#[derive(Debug, Clone, Copy)]
pub struct Namespace<'a> {
    tenant_id: &'a str,
}

impl<'a> Namespace<'a> {
    pub(crate) fn new(tenant_id: &'a str) -> Self {
        Namespace { tenant_id }
    }

    /// local id を `<tenant_id>|<local_id>` へ修飾する。
    fn qualify(&self, local_id: &str) -> String {
        format!("{}{}{}", self.tenant_id, TENANT_SEP, local_id)
    }

    /// parse-back 用の tenant プレフィクス `<tenant_id>|`。
    fn prefix(&self) -> String {
        format!("{}{}", self.tenant_id, TENANT_SEP)
    }

    /// 組織オブジェクト `organization:<tenant>|<org>`。
    pub fn organization(&self, org: &str) -> FgaObject {
        FgaObject::organization(&self.qualify(org))
    }

    /// ロールオブジェクト `role:<tenant>|<id>`。
    pub fn role(&self, id: &str) -> FgaObject {
        FgaObject::role(&self.qualify(id))
    }

    /// フォルダオブジェクト `folder:<tenant>|<id>`。
    pub fn folder(&self, id: &str) -> FgaObject {
        FgaObject::folder(&self.qualify(id))
    }

    /// ファイルオブジェクト `file:<tenant>|<id>`。
    pub fn file(&self, id: &str) -> FgaObject {
        FgaObject::file(&self.qualify(id))
    }

    /// スレッドオブジェクト `thread:<tenant>|<id>`（会話・ReBAC 共有・#37）。
    pub fn thread(&self, id: &str) -> FgaObject {
        FgaObject::thread(&self.qualify(id))
    }

    /// ユーザー subject `user:<tenant>|<id>`。
    pub fn user(&self, id: &str) -> Subject {
        Subject::user(&self.qualify(id))
    }

    /// ロールメンバー userset `role:<tenant>|<id>#member`（#76 共有先・ロール階層の結線）。
    pub fn role_member(&self, id: &str) -> Subject {
        Subject::userset(&self.role(id), Relation::Member)
    }

    /// FGA が返す object id 部（`<tenant>|<local>`）から local id を取り出す。
    ///
    /// tenant が一致しなければ `None`（他テナントのオブジェクトを防御的に除外する）。
    /// `list_objects` 等で得た `type:<tenant>|<local>` を `split_once(':')` した後段で使う。
    pub fn strip_object_id<'s>(&self, id_part: &'s str) -> Option<&'s str> {
        id_part.strip_prefix(self.prefix().as_str())
    }

    /// user subject 文字列 `user:<tenant>|<id>` から local user id を取り出す。
    /// 型/tenant が一致しなければ `None`。
    pub fn parse_user_subject<'s>(&self, raw: &'s str) -> Option<&'s str> {
        raw.strip_prefix("user:")
            .and_then(|rest| self.strip_object_id(rest))
    }

    /// ロールメンバー subject `role:<tenant>|<id>#member` から local role id を取り出す。
    /// 型/tenant/relation が一致しなければ `None`。
    pub fn parse_role_member_subject<'s>(&self, raw: &'s str) -> Option<&'s str> {
        raw.strip_prefix("role:")
            .and_then(|rest| rest.strip_suffix("#member"))
            .and_then(|body| self.strip_object_id(body))
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
    /// ユーザー subject `user:<id>`。tenant 名前空間化は [`Namespace::user`] が担うため
    /// アプリ側からの生 id 構築を防ぐ `pub(crate)`。
    pub(crate) fn user(id: &str) -> Self {
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

    // --- Namespace（SAAS.1 テナント名前空間化） ---

    #[test]
    fn namespace_qualifies_objects_with_tenant() {
        // 全 object 識別子が `<type>:<tenant>|<local>` へ名前空間化されること。
        let ns = Namespace::new("acme");
        assert_eq!(ns.organization("sales").as_str(), "organization:acme|sales");
        assert_eq!(ns.role("dept-1").as_str(), "role:acme|dept-1");
        assert_eq!(ns.folder("f1").as_str(), "folder:acme|f1");
        assert_eq!(ns.file("doc1").as_str(), "file:acme|doc1");
    }

    #[test]
    fn namespace_qualifies_subjects_with_tenant() {
        // user / role_member subject も tenant 名前空間化されること。
        let ns = Namespace::new("acme");
        assert_eq!(ns.user("alice").as_str(), "user:acme|alice");
        assert_eq!(ns.role_member("dept-1").as_str(), "role:acme|dept-1#member");
    }

    #[test]
    fn namespace_role_member_local_id_can_contain_slash() {
        // AD group パス由来の role local id（`/` 含む）でも区切り `|` と衝突しないこと。
        let ns = Namespace::new("acme");
        assert_eq!(
            ns.role_member("sales/team-1").as_str(),
            "role:acme|sales/team-1#member"
        );
    }

    #[test]
    fn namespace_strip_object_id_roundtrips_and_isolates() {
        // strip_object_id は自 tenant の local を返し、他 tenant は None（越境防御）。
        let ns = Namespace::new("acme");
        assert_eq!(ns.strip_object_id("acme|f1"), Some("f1"));
        // local が `|` を含んでも最初の `<tenant>|` だけ剥がすこと。
        assert_eq!(ns.strip_object_id("acme|a|b"), Some("a|b"));
        // 他テナントのオブジェクトは None。
        assert_eq!(ns.strip_object_id("other|f1"), None);
        // tenant 名の前方一致だけでは通さない（区切り必須）。
        assert_eq!(ns.strip_object_id("acme2|f1"), None);
    }

    #[test]
    fn namespace_parse_user_subject() {
        // `user:<tenant>|<id>` から local user id を取り出す。型/tenant 不一致は None。
        let ns = Namespace::new("acme");
        assert_eq!(ns.parse_user_subject("user:acme|alice"), Some("alice"));
        assert_eq!(ns.parse_user_subject("user:other|alice"), None);
        assert_eq!(ns.parse_user_subject("role:acme|alice#member"), None);
    }

    #[test]
    fn namespace_parse_role_member_subject() {
        // `role:<tenant>|<id>#member` から local role id を取り出す。
        let ns = Namespace::new("acme");
        assert_eq!(
            ns.parse_role_member_subject("role:acme|dept-1#member"),
            Some("dept-1")
        );
        // `/` を含む role id も復元できること。
        assert_eq!(
            ns.parse_role_member_subject("role:acme|sales/team-1#member"),
            Some("sales/team-1")
        );
        // 他 tenant / user 型 / member 以外は None。
        assert_eq!(
            ns.parse_role_member_subject("role:other|dept-1#member"),
            None
        );
        assert_eq!(ns.parse_role_member_subject("user:acme|alice"), None);
        assert_eq!(ns.parse_role_member_subject("role:acme|dept-1#owner"), None);
    }

    #[test]
    fn namespace_object_and_userset_preserve_tenant() {
        // Namespace 由来の FgaObject を Subject::object/userset に渡しても tenant が保たれること。
        let ns = Namespace::new("acme");
        assert_eq!(Subject::object(&ns.folder("f1")).as_str(), "folder:acme|f1");
    }
}

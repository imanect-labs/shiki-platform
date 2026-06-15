//! 認可コンテキスト（docs/design.md §4.1）。
//!
//! 全データアクセスは [`AuthContext`] を受け取る規約とする。後続フェーズの
//! storage / rag / data 等の公開 API は第一引数に `&AuthContext` を要求し、
//! アンビエント権限（暗黙のグローバル権限）を排除する。`tenant_id` を将来
//! 追加する場合もここに足す（全クエリへ撒く継ぎ目を一点に集約）。

use serde::{Deserialize, Serialize};

/// 検証済み JWT から抽出した認証主体。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Principal {
    /// ユーザー ID（OIDC の `sub`）。
    pub id: String,
    pub email: Option<String>,
    /// Keycloak group マッパー由来の所属グループ。
    #[serde(default)]
    pub groups: Vec<String>,
    /// 所属部署（claim `department`）。
    pub dept: Option<String>,
}

/// データアクセスの認可コンテキスト。`principal` + `org`（将来 `tenant_id`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthContext {
    pub principal: Principal,
    /// 所属組織（シングルテナント内の最上位スコープ）。
    pub org: String,
}

impl AuthContext {
    pub fn new(principal: Principal, org: String) -> Self {
        AuthContext { principal, org }
    }

    /// 認可問い合わせの主体（`user:<id>`）。
    pub fn subject(&self) -> crate::object::Subject {
        crate::object::Subject::user(&self.principal.id)
    }
}

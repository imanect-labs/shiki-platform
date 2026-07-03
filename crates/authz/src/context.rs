//! 認可コンテキスト（docs/design.md §4.1）。
//!
//! 全データアクセスは [`AuthContext`] を受け取る規約とする。後続フェーズの
//! storage / rag / data 等の公開 API は第一引数に `&AuthContext` を要求し、
//! アンビエント権限（暗黙のグローバル権限）を排除する。SaaS マルチテナント前提で
//! `tenant_id` を day-1 から保持し、全クエリ・全セッションへ撒く継ぎ目をここ一点に
//! 集約する（docs/design.md §4.1 / AGENTS.md 不変条件）。

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
    /// IdP が宣言した所属ロール（claim `roles`・多値）。`groups` と同列のフラットな
    /// 識別メタデータ。authz 判定で使う階層込みの実効メンバーシップは OpenFGA の
    /// `role` タプルが正本（docs/design.md §4.1）。
    #[serde(default)]
    pub roles: Vec<String>,
    /// テナント識別子の素（claim `tenant` 由来・SaaS）。取得元はここではなく
    /// `crates/api` 側の継ぎ目（`resolve_tenant_id`）で解決する。オンプレ/cell の
    /// シングルテナントでは claim が無く設定の固定値にフォールバックする。
    #[serde(default)]
    pub tenant_id: Option<String>,
}

/// データアクセスの認可コンテキスト。`principal` + `org` + `tenant_id`。
///
/// `tenant_id` は SaaS マルチテナントの隔離境界であり day-1 から必須。`new()` の
/// 必須引数とすることで「`tenant_id` 無しの `AuthContext`」を型レベルで構築不能にする。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthContext {
    pub principal: Principal,
    /// 所属組織（テナント内の最上位スコープ）。
    pub org: String,
    /// テナント識別子（SaaS の隔離境界。オンプレは単一固定）。
    pub tenant_id: String,
}

impl AuthContext {
    pub fn new(principal: Principal, org: String, tenant_id: String) -> Self {
        AuthContext {
            principal,
            org,
            tenant_id,
        }
    }

    /// 認可問い合わせの主体（`user:<id>`）。
    ///
    /// 注意: 現状の subject/object 識別子（`user:<id>` / `organization:<org>`）は
    /// `tenant_id` を含まない。共用 OpenFGA ストアでの**実分離の強制**（object/store
    /// namespace の `tenant_id` スコープ化）は roadmap トラック SAAS.1 の責務であり、
    /// 本フェーズ（#57）は `AuthContext` に `tenant_id` を保持する継ぎ目の用意までを範囲とする。
    /// SaaS で共用ストアを使う前に SAAS.1 でテナント境界を識別子へ織り込むこと。
    pub fn subject(&self) -> crate::object::Subject {
        crate::object::Subject::user(&self.principal.id)
    }
}

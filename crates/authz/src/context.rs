//! 認可コンテキスト（docs/design.md §4.1）。
//!
//! 全データアクセスは [`AuthContext`] を受け取る規約とする。後続フェーズの
//! storage / rag / data 等の公開 API は第一引数に `&AuthContext` を要求し、
//! アンビエント権限（暗黙のグローバル権限）を排除する。SaaS マルチテナント前提で
//! `tenant_id` を day-1 から保持し、全クエリ・全セッションへ撒く継ぎ目をここ一点に
//! 集約する（docs/design.md §4.1 / AGENTS.md 不変条件）。

use serde::{Deserialize, Serialize};

/// プリンシパルの種別（Task 10.4a）。
///
/// `user` = 対話トリガの本人（OIDC subject）。`workflow` = schedule/event run の専用サービス
/// プリンシパル（`workflow:<tenant>|<id>`）。委譲タプルはこの subject へ書かれる（engine.md §6.1）。
/// `#[serde(default)]` で既存 Redis セッション（kind 無し）は User にフォールバックする（後方互換）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalKind {
    #[default]
    User,
    Workflow,
}

/// 検証済み JWT から抽出した認証主体。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Principal {
    /// プリンシパル種別（既定 user・schedule/event run は workflow）。
    #[serde(default)]
    pub kind: PrincipalKind,
    /// ユーザー ID（OIDC の `sub`）、または workflow プリンシパルのローカル id。
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

    /// この認可コンテキストの tenant に束縛した識別子ビルダ（SAAS.1 のチョークポイント）。
    ///
    /// FGA 識別子は `<type>:<tenant_id>|<local_id>` で名前空間化され、共用 OpenFGA ストア
    /// 上でも越境タプル/問い合わせを構造的に不能化する。アプリ側（storage / api）は
    /// 生の [`FgaObject`](crate::FgaObject) / [`Subject`](crate::Subject) を組めず、必ず
    /// この [`Namespace`](crate::Namespace) 経由で識別子を構築する。
    pub fn ns(&self) -> crate::object::Namespace<'_> {
        crate::object::Namespace::new(&self.tenant_id)
    }

    /// 認可問い合わせの主体。tenant 名前空間化済み（SAAS.1）。
    ///
    /// principal.kind により `user:<tenant>|<id>`（対話）または `workflow:<tenant>|<id>`
    /// （schedule/event run）を返す。schedule/event run はこの workflow subject で全 check/
    /// ListObjects を評価し、委譲タプルに照合する（engine.md §6.1・confused-deputy 防御）。
    pub fn subject(&self) -> crate::object::Subject {
        match self.principal.kind {
            PrincipalKind::User => self.ns().user(&self.principal.id),
            PrincipalKind::Workflow => self.ns().workflow_principal(&self.principal.id),
        }
    }

    /// workflow プリンシパルの AuthContext を組む（schedule/event run の実行主体・Task 10.4a）。
    pub fn for_workflow(tenant_id: String, org: String, workflow_local_id: &str) -> Self {
        AuthContext {
            principal: Principal {
                kind: PrincipalKind::Workflow,
                id: workflow_local_id.to_string(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some(tenant_id.clone()),
            },
            org,
            tenant_id,
        }
    }
}

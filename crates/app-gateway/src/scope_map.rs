//! ルート → 必要スコープの宣言的マップ（Task 9.6・design §4.6 集中 PEP）。
//!
//! ゲートウェイの各ルートが要求する [`authz::CapabilityScope`] を**単一の const 表**で定義し、
//! 二重ゲート middleware が一律強制する（個別ハンドラでスコープチェックを書かせない＝抜け漏れを
//! 構造的に不可能化）。表に無いルートは fail-closed で拒否する（未宣言 = 到達不能）。
//!
//! 能力アダプタ（storage/data/rag/identity/events/notify・Task 9.8/PR7）はこの表へ行を足す。
//! 本 PR（9.6）はゲート機構の証明用に whoami（スコープ不要）と data.read プローブのみを持つ。

use authz::CapabilityScope;

/// ルートのスコープ要件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteScope {
    /// 認証は要るが能力スコープは不要（自己情報など）。
    Public,
    /// 指定の能力スコープを要求する（granted_scopes に含まれ、かつトークン scope 内であること）。
    Scoped(CapabilityScope),
}

/// ルート表の 1 行（HTTP メソッド × パステンプレート → スコープ要件）。
#[derive(Debug, Clone, Copy)]
pub struct GatewayRoute {
    pub method: &'static str,
    /// axum の [`MatchedPath`](axum::extract::MatchedPath) と一致するテンプレート（`/gw/...`）。
    pub path: &'static str,
    pub scope: RouteScope,
}

/// ゲートウェイの全ルートとスコープ要件（宣言的マップ・単一定義）。
///
/// 新しい能力ルートは必ずここへ追加する（追加漏れは middleware が fail-closed で弾く＝到達不能）。
/// [`crate::routes::capability_router`] の登録と 1:1 対応させること。
pub(crate) const GATEWAY_ROUTES: &[GatewayRoute] = &[
    // 呼出主体の自己情報（app_id・granted_scopes・user sub）。能力スコープ不要。
    GatewayRoute {
        method: "GET",
        path: "/gw/whoami",
        scope: RouteScope::Public,
    },
    // --- data.*（アプリ所有テーブル束縛・Task 9.8） ---
    GatewayRoute {
        method: "GET",
        path: "/gw/data/tables",
        scope: RouteScope::Scoped(CapabilityScope::DataRead),
    },
    GatewayRoute {
        method: "GET",
        path: "/gw/data/tables/{table_id}/schema",
        scope: RouteScope::Scoped(CapabilityScope::DataSchema),
    },
    GatewayRoute {
        method: "GET",
        path: "/gw/data/tables/{table_id}/records",
        scope: RouteScope::Scoped(CapabilityScope::DataRead),
    },
    GatewayRoute {
        method: "POST",
        path: "/gw/data/tables/{table_id}/records",
        scope: RouteScope::Scoped(CapabilityScope::DataWrite),
    },
    GatewayRoute {
        method: "GET",
        path: "/gw/data/tables/{table_id}/records/{record_id}",
        scope: RouteScope::Scoped(CapabilityScope::DataRead),
    },
    GatewayRoute {
        method: "PATCH",
        path: "/gw/data/tables/{table_id}/records/{record_id}",
        scope: RouteScope::Scoped(CapabilityScope::DataWrite),
    },
    GatewayRoute {
        method: "DELETE",
        path: "/gw/data/tables/{table_id}/records/{record_id}",
        scope: RouteScope::Scoped(CapabilityScope::DataWrite),
    },
    GatewayRoute {
        method: "POST",
        path: "/gw/data/tables/{table_id}/query",
        scope: RouteScope::Scoped(CapabilityScope::DataRead),
    },
    GatewayRoute {
        method: "POST",
        path: "/gw/data/tables/{table_id}/records/{record_id}/transition",
        scope: RouteScope::Scoped(CapabilityScope::DataWrite),
    },
    // --- storage.*（個人 ReBAC・StorageService 委譲） ---
    GatewayRoute {
        method: "GET",
        path: "/gw/storage/nodes/{node_id}",
        scope: RouteScope::Scoped(CapabilityScope::StorageRead),
    },
    GatewayRoute {
        method: "GET",
        path: "/gw/storage/nodes/{node_id}/children",
        scope: RouteScope::Scoped(CapabilityScope::StorageRead),
    },
    GatewayRoute {
        method: "GET",
        path: "/gw/storage/nodes/{node_id}/download-url",
        scope: RouteScope::Scoped(CapabilityScope::StorageRead),
    },
    GatewayRoute {
        method: "POST",
        path: "/gw/storage/folders",
        scope: RouteScope::Scoped(CapabilityScope::StorageWrite),
    },
    // --- rag.query（permission-aware 検索） ---
    GatewayRoute {
        method: "POST",
        path: "/gw/rag/query",
        scope: RouteScope::Scoped(CapabilityScope::RagQuery),
    },
    // --- identity.read（本人の最小 identity） ---
    GatewayRoute {
        method: "GET",
        path: "/gw/identity/me",
        scope: RouteScope::Scoped(CapabilityScope::IdentityRead),
    },
    // --- events.subscribe（SSE ライブテール） ---
    GatewayRoute {
        method: "GET",
        path: "/gw/events/subscribe",
        scope: RouteScope::Scoped(CapabilityScope::EventsSubscribe),
    },
    // --- notify.send（通知台帳へ記録） ---
    GatewayRoute {
        method: "POST",
        path: "/gw/notify/send",
        scope: RouteScope::Scoped(CapabilityScope::NotifySend),
    },
    // --- llm.invoke / agent.invoke（ミニアプリ内 AI・Task 9.9） ---
    GatewayRoute {
        method: "POST",
        path: "/gw/ai/llm/invoke",
        scope: RouteScope::Scoped(CapabilityScope::LlmInvoke),
    },
    GatewayRoute {
        method: "POST",
        path: "/gw/ai/agent/invoke",
        scope: RouteScope::Scoped(CapabilityScope::AgentInvoke),
    },
    // --- B2 関数起動（Task 9.12・能力スコープ不要=関数自体がアプリのロジック。
    //     関数内の能力呼び出しは exchange 後のトークンで二重ゲートを通る） ---
    GatewayRoute {
        method: "POST",
        path: "/gw/apps/functions/{function}/invoke",
        scope: RouteScope::Public,
    },
];

/// メソッド＋マッチ済みパステンプレートからスコープ要件を引く（未宣言は `None`＝fail-closed 拒否）。
pub fn required_scope_for(method: &str, matched_path: &str) -> Option<RouteScope> {
    GATEWAY_ROUTES
        .iter()
        .find(|r| r.method.eq_ignore_ascii_case(method) && r.path == matched_path)
        .map(|r| r.scope)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_routes_resolve() {
        assert_eq!(
            required_scope_for("GET", "/gw/whoami"),
            Some(RouteScope::Public)
        );
        assert_eq!(
            required_scope_for("get", "/gw/data/tables"),
            Some(RouteScope::Scoped(CapabilityScope::DataRead))
        );
        assert_eq!(
            required_scope_for("POST", "/gw/data/tables/{table_id}/records"),
            Some(RouteScope::Scoped(CapabilityScope::DataWrite))
        );
        assert_eq!(
            required_scope_for("GET", "/gw/data/tables/{table_id}/schema"),
            Some(RouteScope::Scoped(CapabilityScope::DataSchema))
        );
        assert_eq!(
            required_scope_for("POST", "/gw/rag/query"),
            Some(RouteScope::Scoped(CapabilityScope::RagQuery))
        );
        assert_eq!(
            required_scope_for("GET", "/gw/identity/me"),
            Some(RouteScope::Scoped(CapabilityScope::IdentityRead))
        );
        assert_eq!(
            required_scope_for("GET", "/gw/events/subscribe"),
            Some(RouteScope::Scoped(CapabilityScope::EventsSubscribe))
        );
        assert_eq!(
            required_scope_for("POST", "/gw/notify/send"),
            Some(RouteScope::Scoped(CapabilityScope::NotifySend))
        );
        assert_eq!(
            required_scope_for("GET", "/gw/storage/nodes/{node_id}"),
            Some(RouteScope::Scoped(CapabilityScope::StorageRead))
        );
        assert_eq!(
            required_scope_for("POST", "/gw/storage/folders"),
            Some(RouteScope::Scoped(CapabilityScope::StorageWrite))
        );
        assert_eq!(
            required_scope_for("POST", "/gw/ai/llm/invoke"),
            Some(RouteScope::Scoped(CapabilityScope::LlmInvoke))
        );
        assert_eq!(
            required_scope_for("POST", "/gw/ai/agent/invoke"),
            Some(RouteScope::Scoped(CapabilityScope::AgentInvoke))
        );
    }

    #[test]
    fn write_routes_never_require_read_scope() {
        // 書込系メソッドのルートに read スコープを誤宣言していない（権限降格の防止）。
        for r in GATEWAY_ROUTES {
            if matches!(r.method, "POST" | "PATCH" | "PUT" | "DELETE") {
                assert!(
                    !matches!(
                        r.scope,
                        RouteScope::Scoped(
                            CapabilityScope::DataRead | CapabilityScope::StorageRead
                        )
                    ) || r.path.ends_with("/query"),
                    "書込メソッドに read スコープ: {} {}",
                    r.method,
                    r.path
                );
            }
        }
    }

    #[test]
    fn unknown_route_is_fail_closed() {
        // 表に無いパス/メソッドは None（middleware が拒否）。
        assert_eq!(required_scope_for("GET", "/gw/unknown"), None);
        assert_eq!(required_scope_for("POST", "/gw/whoami"), None);
    }

    #[test]
    fn no_duplicate_route_entries() {
        // (method, path) は一意（二重定義でスコープが曖昧にならない）。
        for (i, a) in GATEWAY_ROUTES.iter().enumerate() {
            for b in &GATEWAY_ROUTES[i + 1..] {
                assert!(
                    !(a.method == b.method && a.path == b.path),
                    "重複ルート: {} {}",
                    a.method,
                    a.path
                );
            }
        }
    }
}

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
pub(crate) const GATEWAY_ROUTES: &[GatewayRoute] = &[
    // 呼出主体の自己情報（app_id・granted_scopes・user sub）。能力スコープ不要。
    GatewayRoute {
        method: "GET",
        path: "/gw/whoami",
        scope: RouteScope::Public,
    },
    // 機構証明用: data.read スコープを要求するプローブ（PR7 で実能力アダプタへ置換）。
    GatewayRoute {
        method: "GET",
        path: "/gw/probe",
        scope: RouteScope::Scoped(CapabilityScope::DataRead),
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
            required_scope_for("get", "/gw/probe"),
            Some(RouteScope::Scoped(CapabilityScope::DataRead))
        );
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

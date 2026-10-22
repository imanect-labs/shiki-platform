//! Keycloak group パス → org / 部署の解決規則の単一定義（SAAS.2 / #811）。
//!
//! realm `shiki` は全テナント共用の単一 realm なので、group を素の org slug（`/acme`）で
//! 作ると同じ slug を使うテナント同士で group を共有してしまう（group に付けた client role・
//! IdP マッパー・属性がテナントをまたいで効く）。そこで org group はテナントで修飾した
//! `/tenants/<tenant>/<org>`、部署はその下（`/tenants/<tenant>/<org>/<dept>`）に置く。
//!
//! 解決規則（内部 API の `AuthContext` 構築・role 同期と、公開 API ゲートウェイで共用する）:
//! - **解決済み `tenant_id` の配下だけ**を見る。tenant の比較はセグメント単位
//!   （`/tenants/acme2/x` は tenant `acme` に一致しない）。
//! - テナントルート（`/tenants/<tenant>`）自体は org にしない（org 名が無い）。
//! - org 候補が複数ならソートして最小を採る（claim の並び順に依存させない）。
//! - 返す相対パス（`<org>` / `<org>/<dept>`）は従来の role id と同じ値になる
//!   （FGA の `role:<tenant>|acme/eng`・共有タプル・`directory_role` を変えずに済む）。
//! - [`GroupLayout::TenantScopedOrLegacy`]（single・オンプレ）だけは、自テナント配下の group が
//!   1 つも無いとき従来の直下 group（`/acme`・`/acme/eng`）を使う。`/tenants` 配下は
//!   他テナントのものとみなし、従来解釈には含めない。

/// テナント修飾 group を束ねるトップレベル group の名前（予約語）。
pub const TENANTS_GROUP: &str = "tenants";

/// group パスの解釈方式（テナンシーモードの写し）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupLayout {
    /// multi（SaaS）: `/tenants/<tenant>/...` 配下だけを org/部署とみなす。
    TenantScoped,
    /// single（オンプレ/cell）: 自テナント配下を優先し、無ければ従来の直下 group を使う。
    TenantScopedOrLegacy,
}

/// テナントルート group のパス（`/tenants/<tenant>`）。
pub fn tenant_group_path(tenant_id: &str) -> String {
    format!("/{TENANTS_GROUP}/{tenant_id}")
}

/// org group のパス（`/tenants/<tenant>/<org>`）。
pub fn org_group_path(tenant_id: &str, org: &str) -> String {
    format!("/{TENANTS_GROUP}/{tenant_id}/{org}")
}

/// groups claim のうち `tenant_id` に属するものを、テナントルートからの相対パス
/// （`<org>` / `<org>/<dept>`・先頭 `/` 無し）で返す。claim の並び順は保つ。
///
/// 空セグメントを含むパス（`/tenants/acme//x` 等）は壊れた値として無視する。
pub fn tenant_relative_groups(
    groups: &[String],
    tenant_id: &str,
    layout: GroupLayout,
) -> Vec<String> {
    let scoped: Vec<String> = groups
        .iter()
        .filter_map(|g| scoped_relative(g, tenant_id))
        .collect();
    if !scoped.is_empty() || layout == GroupLayout::TenantScoped {
        return scoped;
    }
    groups.iter().filter_map(|g| legacy_relative(g)).collect()
}

/// groups claim から org を解決する（該当無しは `None`・呼び出し側が既定 org へ倒す）。
pub fn resolve_org(groups: &[String], tenant_id: &str, layout: GroupLayout) -> Option<String> {
    tenant_relative_groups(groups, tenant_id, layout)
        .iter()
        .filter_map(|rel| rel.split('/').next())
        .min()
        .map(str::to_string)
}

/// 空セグメントを除かずに分割する（`/a//b` を壊れた値として検出するため）。
fn segments(path: &str) -> Option<Vec<&str>> {
    let trimmed = path.trim().strip_prefix('/').unwrap_or(path.trim());
    let segs: Vec<&str> = trimmed.split('/').collect();
    if segs.iter().any(|s| s.is_empty()) {
        return None;
    }
    Some(segs)
}

/// `/tenants/<tenant_id>/<rel...>` なら `<rel...>` を返す。
fn scoped_relative(path: &str, tenant_id: &str) -> Option<String> {
    let segs = segments(path)?;
    match segs.as_slice() {
        [root, tenant, rest @ ..] if *root == TENANTS_GROUP && *tenant == tenant_id => {
            (!rest.is_empty()).then(|| rest.join("/"))
        }
        _ => None,
    }
}

/// 従来形式（`/acme`・`/acme/eng`）の相対パス。`/tenants` 配下は含めない。
fn legacy_relative(path: &str) -> Option<String> {
    let segs = segments(path)?;
    (segs.first() != Some(&TENANTS_GROUP)).then(|| segs.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use GroupLayout::{TenantScoped, TenantScopedOrLegacy};

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn paths_are_tenant_qualified() {
        assert_eq!(tenant_group_path("acme"), "/tenants/acme");
        assert_eq!(org_group_path("acme", "sales"), "/tenants/acme/sales");
    }

    #[test]
    fn org_and_departments_from_own_tenant() {
        let groups = v(&["/tenants/acme/acme/eng", "/tenants/acme/acme"]);
        assert_eq!(
            resolve_org(&groups, "acme", TenantScoped).as_deref(),
            Some("acme")
        );
        // 相対パスは従来の role id と同じ値（FGA・共有タプル不変）。
        assert_eq!(
            tenant_relative_groups(&groups, "acme", TenantScoped),
            v(&["acme/eng", "acme"])
        );
    }

    #[test]
    fn other_tenant_group_is_not_an_org() {
        // tenant A の主体が `/tenants/B/x` を持っても org は x にならない。
        let groups = v(&["/tenants/b-corp/x"]);
        assert_eq!(resolve_org(&groups, "a-corp", TenantScoped), None);
        assert_eq!(resolve_org(&groups, "a-corp", TenantScopedOrLegacy), None);
        // 自テナント分と混在しても他テナント分は捨てる。
        let mixed = v(&["/tenants/b-corp/b/eng", "/tenants/a-corp/a"]);
        assert_eq!(
            tenant_relative_groups(&mixed, "a-corp", TenantScoped),
            v(&["a"])
        );
    }

    #[test]
    fn tenant_compare_is_segment_wise() {
        // `/tenants/acme2/x` は tenant `acme` に一致しない（前方一致で拾わない）。
        let groups = v(&["/tenants/acme2/x", "/tenants/acme2"]);
        assert_eq!(resolve_org(&groups, "acme", TenantScoped), None);
        assert!(tenant_relative_groups(&groups, "acme", TenantScoped).is_empty());
    }

    #[test]
    fn tenant_root_alone_is_not_an_org() {
        let groups = v(&["/tenants/acme", "/tenants"]);
        assert_eq!(resolve_org(&groups, "acme", TenantScoped), None);
        assert!(tenant_relative_groups(&groups, "acme", TenantScoped).is_empty());
    }

    #[test]
    fn multi_ignores_legacy_paths() {
        // multi では旧形式（テナント無修飾）の group だけなら org は解決しない（既定 org へ）。
        let groups = v(&["/acme", "/acme/eng"]);
        assert_eq!(resolve_org(&groups, "acme", TenantScoped), None);
        assert!(tenant_relative_groups(&groups, "acme", TenantScoped).is_empty());
    }

    #[test]
    fn other_tenant_department_is_not_a_role() {
        // `/tenants/B/b/eng` は自テナント（A）の部署 role にならない（single の従来解釈でも）。
        let groups = v(&["/tenants/b-corp/b/eng"]);
        assert!(tenant_relative_groups(&groups, "a-corp", TenantScoped).is_empty());
        assert!(tenant_relative_groups(&groups, "a-corp", TenantScopedOrLegacy).is_empty());
    }

    #[test]
    fn org_is_order_independent() {
        let a = v(&["/tenants/t/zeta", "/tenants/t/alpha/eng", "/tenants/t/mid"]);
        let mut b = a.clone();
        b.reverse();
        assert_eq!(resolve_org(&a, "t", TenantScoped).as_deref(), Some("alpha"));
        assert_eq!(resolve_org(&b, "t", TenantScoped).as_deref(), Some("alpha"));
        let legacy = v(&["/zeta", "/alpha"]);
        let mut legacy_rev = legacy.clone();
        legacy_rev.reverse();
        assert_eq!(
            resolve_org(&legacy, "t", TenantScopedOrLegacy),
            resolve_org(&legacy_rev, "t", TenantScopedOrLegacy)
        );
    }

    #[test]
    fn single_keeps_legacy_paths() {
        // single は自テナント配下が無ければ従来の直下 group を org/部署に使う。
        let groups = v(&["/acme/eng", "acme"]);
        assert_eq!(
            resolve_org(&groups, "default", TenantScopedOrLegacy).as_deref(),
            Some("acme")
        );
        assert_eq!(
            tenant_relative_groups(&groups, "default", TenantScopedOrLegacy),
            v(&["acme/eng", "acme"])
        );
    }

    #[test]
    fn single_prefers_tenant_scoped_paths() {
        // 自テナント配下があれば従来形式の group は使わない（混在時に旧 group を部署化しない）。
        let groups = v(&["/legacy", "/tenants/default/acme/eng"]);
        assert_eq!(
            tenant_relative_groups(&groups, "default", TenantScopedOrLegacy),
            v(&["acme/eng"])
        );
        assert_eq!(
            resolve_org(&groups, "default", TenantScopedOrLegacy).as_deref(),
            Some("acme")
        );
    }

    #[test]
    fn malformed_paths_are_ignored() {
        let groups = v(&["", "/", "/tenants/acme//x", "/tenants/acme/a/", " "]);
        assert!(tenant_relative_groups(&groups, "acme", TenantScoped).is_empty());
        assert!(tenant_relative_groups(&groups, "acme", TenantScopedOrLegacy).is_empty());
        assert_eq!(resolve_org(&[], "acme", TenantScopedOrLegacy), None);
    }
}

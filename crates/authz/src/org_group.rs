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
//!   1 つも無いとき従来の直下 group（`/acme`・`/acme/eng`）を使う。この従来解釈は #811 以前と
//!   **同じ結果**にする（org は claim の先頭要素の先頭セグメント・部署は先頭 `/` を除いたパス）。
//!   既存のオンプレ利用者の org を変えないため、ここではソートしない。例外は `/tenants` 配下で、
//!   他テナントのものとみなして従来解釈にも含めない（トップレベル group 名 `tenants` は予約）。

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
/// 自テナント配下では空セグメントを含むパス（`/tenants/acme//x` 等）を壊れた値として無視する。
/// 従来解釈（single のフォールバック）は #811 以前と同じく先頭の `/` を除くだけ。
pub fn tenant_relative_groups(
    groups: &[String],
    tenant_id: &str,
    layout: GroupLayout,
) -> Vec<String> {
    relative_groups(groups, tenant_id, layout).0
}

/// groups claim から org を解決する（該当無しは `None`・呼び出し側が既定 org へ倒す）。
///
/// 自テナント配下は候補をソートして最小を採る。従来解釈は #811 以前と同じく claim の
/// 先頭要素の先頭セグメント（オンプレ利用者の org を変えない）。
pub fn resolve_org(groups: &[String], tenant_id: &str, layout: GroupLayout) -> Option<String> {
    let (relative, source) = relative_groups(groups, tenant_id, layout);
    let mut orgs = relative.iter().filter_map(|rel| rel.split('/').next());
    match source {
        Source::TenantScoped => orgs.min(),
        Source::Legacy => orgs.next(),
    }
    .map(str::to_string)
}

/// 相対パスをどちらの解釈で得たか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    TenantScoped,
    Legacy,
}

fn relative_groups(
    groups: &[String],
    tenant_id: &str,
    layout: GroupLayout,
) -> (Vec<String>, Source) {
    let scoped: Vec<String> = groups
        .iter()
        .filter_map(|g| scoped_relative(g, tenant_id))
        .collect();
    if !scoped.is_empty() || layout == GroupLayout::TenantScoped {
        return (scoped, Source::TenantScoped);
    }
    let legacy = groups.iter().filter_map(|g| legacy_relative(g)).collect();
    (legacy, Source::Legacy)
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

/// 従来形式（`/acme`・`/acme/eng`）の相対パス。#811 以前の `trim_start_matches('/')` と同じ値で、
/// 空なら捨てる。`/tenants` 配下（先頭セグメントが予約語）だけは含めない。
fn legacy_relative(path: &str) -> Option<String> {
    let rel = path.trim_start_matches('/');
    let first = rel.split('/').next().unwrap_or_default();
    (!first.is_empty() && first != TENANTS_GROUP).then(|| rel.to_string())
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
        // single でも自テナント配下ならソート（新しい置き場所は順序非依存）。
        assert_eq!(
            resolve_org(&b, "t", TenantScopedOrLegacy).as_deref(),
            Some("alpha")
        );
    }

    /// #811 以前の `resolve_org`（claim の先頭要素の先頭セグメント）。
    fn pre_811_org(groups: &[String]) -> String {
        groups
            .iter()
            .filter_map(|g| g.trim_start_matches('/').split('/').next())
            .find(|seg| !seg.is_empty())
            .unwrap_or("default")
            .to_string()
    }

    #[test]
    fn single_legacy_matches_pre_811_resolution() {
        // `/tenants` を含まない claim なら、従来解釈は旧実装と完全に同じ org を返す。
        let cases: &[&[&str]] = &[
            &["/acme/eng", "/acme"],
            &["/beta/aaa", "/alpha/zzz"],
            &["/zeta", "/alpha"],
            &["acme", "/b"],
            &["", "/", "//x//y"],
            &[" sp ace", "/a"],
            &["/Beta", "/acme"],
            &[],
        ];
        for case in cases {
            let groups = v(case);
            let got = resolve_org(&groups, "default", TenantScopedOrLegacy)
                .unwrap_or_else(|| "default".into());
            assert_eq!(got, pre_811_org(&groups), "{case:?}");
        }
    }

    #[test]
    fn single_legacy_org_is_first_claim_element() {
        // 従来解釈は #811 以前と同じ「claim の先頭要素の先頭セグメント」（ソートしない）。
        // Keycloak の claim 順は安定なので、複数 org に属するオンプレ利用者の org を変えない。
        let l = TenantScopedOrLegacy;
        assert_eq!(
            resolve_org(&v(&["/beta/aaa", "/alpha/zzz"]), "default", l).as_deref(),
            Some("beta")
        );
        assert_eq!(
            resolve_org(&v(&["/acme", "/Beta"]), "default", l).as_deref(),
            Some("acme")
        );
        assert_eq!(
            resolve_org(&v(&["/Beta", "/acme"]), "default", l).as_deref(),
            Some("Beta")
        );
        // 空・スラッシュだけの要素は飛ばす（従来どおり）。空セグメントも従来どおり先頭で切る。
        assert_eq!(
            resolve_org(&v(&["", "/", "//acme//x"]), "default", l).as_deref(),
            Some("acme")
        );
        assert_eq!(
            tenant_relative_groups(&v(&["//acme//x", "/acme/"]), "default", l),
            v(&["acme//x", "acme/"])
        );
    }

    #[test]
    fn single_legacy_skips_reserved_tenants_group() {
        // `/tenants` 配下は他テナントのもの（従来解釈でも org にしない）。次の要素を採る。
        let groups = v(&["/tenants/other/x", "/tenants", "/acme"]);
        assert_eq!(
            resolve_org(&groups, "default", TenantScopedOrLegacy).as_deref(),
            Some("acme")
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
        let groups = v(&["", "/", "/tenants/acme//x", "/tenants/acme/a/"]);
        assert!(tenant_relative_groups(&groups, "acme", TenantScoped).is_empty());
        assert!(tenant_relative_groups(&groups, "acme", TenantScopedOrLegacy).is_empty());
        assert_eq!(resolve_org(&[], "acme", TenantScopedOrLegacy), None);
    }
}

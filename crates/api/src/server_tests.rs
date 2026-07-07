
use super::*;
use std::collections::BTreeSet;

/// route_table の (path, METHOD) 集合。
fn declared(policy_filter: impl Fn(AccessPolicy) -> bool) -> BTreeSet<(String, String)> {
    route_table()
        .iter()
        .filter(|d| policy_filter(d.policy))
        .flat_map(|d| {
            d.methods
                .iter()
                .map(|m| (d.path.to_string(), m.to_string()))
        })
        .collect()
}

/// OpenAPI 仕様の (path, METHOD) 集合。
fn openapi_ops() -> BTreeSet<(String, String)> {
    let spec: serde_json::Value =
        serde_json::from_str(&openapi::openapi_json()).expect("OpenAPI JSON");
    let mut out = BTreeSet::new();
    let paths = spec["paths"].as_object().expect("paths object");
    for (path, ops) in paths {
        for method in ops.as_object().expect("ops object").keys() {
            out.insert((path.clone(), method.to_uppercase()));
        }
    }
    out
}

#[test]
fn route_table_has_no_duplicate_ops() {
    // 同一 (path, method) の二重宣言はマージ時に panic するため表の段階で検出する。
    let mut seen = BTreeSet::new();
    for decl in route_table() {
        assert!(!decl.methods.is_empty(), "{}: methods が空", decl.path);
        for m in decl.methods {
            assert!(
                seen.insert((decl.path, *m)),
                "重複宣言: {} {}",
                m,
                decl.path
            );
        }
    }
}

/// 宣言的スコープマップと OpenAPI（codegen の正）の相互網羅（#91 M-1）。
///
/// - OpenAPI に載る全操作は route_table に**非 Public** で宣言されていること
///   （API を増やすときポリシー宣言を強制する）。
/// - 逆に、非 Public の全宣言は OpenAPI に載っていること
///   （utoipa 注釈＝TS 型生成の漏れを防ぐ）。
#[test]
fn route_table_matches_openapi() {
    let declared = declared(|p| p != AccessPolicy::Public);
    let spec = openapi_ops();
    let missing_policy: Vec<_> = spec.difference(&declared).collect();
    assert!(
        missing_policy.is_empty(),
        "OpenAPI にあるが route_table に非 Public 宣言が無い: {missing_policy:?}"
    );
    let missing_spec: Vec<_> = declared.difference(&spec).collect();
    assert!(
        missing_spec.is_empty(),
        "route_table にあるが OpenAPI（utoipa 注釈）に無い: {missing_spec:?}"
    );
}

#[test]
fn admin_routes_are_provisioner_scoped() {
    // /admin/* は必ず Provisioner ポリシー（BFF セッションで到達できない）こと。
    for decl in route_table() {
        if decl.path.starts_with("/admin/") {
            assert_eq!(
                decl.policy,
                AccessPolicy::Provisioner,
                "{} は Provisioner であること",
                decl.path
            );
        } else {
            assert_ne!(
                decl.policy,
                AccessPolicy::Provisioner,
                "{} が Provisioner になっている",
                decl.path
            );
        }
    }
}

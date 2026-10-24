//! Keycloak の group ツリー操作と旧 org group 移行（#811）の結合テスト（in-process モック）。
//!
//! group の親子・メンバーシップをメモリ上に持つモック Keycloak を立て、テナント修飾 group の
//! 冪等作成・subtree 削除・リネーム・`migrate-org-groups` の読み取り→計画→適用を検証する。
//! 実 Keycloak での挙動確認は PR の検証手順（26.2 を docker で起動）で行う。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::cognitive_complexity
)]

use std::sync::{Arc, Mutex};

use api::{
    config::{AuthConfig, Tenancy},
    keycloak_admin::{
        org_group_migration::{
            apply_tree, list_realm_groups, plan_tree, select_orgs, snapshot_tree, TenantRule,
        },
        KeycloakAdmin, RenameOutcome,
    },
};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post, put},
    Json, Router,
};
use serde_json::{json, Value};

#[derive(Clone)]
struct MockGroup {
    id: String,
    name: String,
    parent: Option<String>,
    realm_roles: Vec<String>,
}

#[derive(Clone)]
struct MockUser {
    id: String,
    username: String,
    tenant: Option<String>,
}

#[derive(Default)]
struct Kc {
    groups: Vec<MockGroup>,
    users: Vec<MockUser>,
    /// (user_id, group_id)
    members: Vec<(String, String)>,
    next_id: u32,
}

type St = Arc<Mutex<Kc>>;

impl Kc {
    fn path(&self, id: &str) -> String {
        let mut segs = Vec::new();
        let mut cur = self.groups.iter().find(|g| g.id == id);
        while let Some(g) = cur {
            segs.push(g.name.clone());
            cur = g
                .parent
                .as_ref()
                .and_then(|p| self.groups.iter().find(|x| &x.id == p));
        }
        segs.reverse();
        format!("/{}", segs.join("/"))
    }

    fn rep(&self, g: &MockGroup) -> Value {
        let subs = self
            .groups
            .iter()
            .filter(|c| c.parent.as_deref() == Some(&g.id))
            .count();
        json!({
            "id": g.id, "name": g.name, "path": self.path(&g.id),
            "subGroupCount": subs, "subGroups": [], "attributes": {},
            "realmRoles": g.realm_roles, "clientRoles": {},
        })
    }

    fn has_descendant_named(&self, id: &str, name: &str) -> bool {
        self.groups
            .iter()
            .filter(|c| c.parent.as_deref() == Some(id))
            .any(|c| c.name == name || self.has_descendant_named(&c.id, name))
    }

    fn create(&mut self, parent: Option<String>, name: &str) -> StatusCode {
        if self
            .groups
            .iter()
            .any(|g| g.parent == parent && g.name == name)
        {
            return StatusCode::CONFLICT;
        }
        self.next_id += 1;
        let id = format!("g{}", self.next_id);
        self.groups.push(MockGroup {
            id,
            name: name.into(),
            parent,
            realm_roles: vec![],
        });
        StatusCode::CREATED
    }

    fn delete_subtree(&mut self, id: &str) {
        let children: Vec<String> = self
            .groups
            .iter()
            .filter(|c| c.parent.as_deref() == Some(id))
            .map(|c| c.id.clone())
            .collect();
        for c in children {
            self.delete_subtree(&c);
        }
        self.groups.retain(|g| g.id != id);
        self.members.retain(|(_, g)| g != id);
    }

    fn add_group(&mut self, parent: Option<&str>, name: &str) -> String {
        self.create(parent.map(str::to_string), name);
        self.groups
            .iter()
            .find(|g| g.parent.as_deref() == parent && g.name == name)
            .unwrap()
            .id
            .clone()
    }

    fn add_user(&mut self, id: &str, tenant: Option<&str>, groups: &[&str]) {
        self.users.push(MockUser {
            id: id.into(),
            username: format!("{id}-name"),
            tenant: tenant.map(str::to_string),
        });
        for g in groups {
            self.members.push((id.into(), g.to_string()));
        }
    }

    fn user_paths(&self, user: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .members
            .iter()
            .filter(|(u, _)| u == user)
            .map(|(_, g)| self.path(g))
            .collect();
        out.sort();
        out
    }
}

/// `first`/`max` でページングする。`max` 省略時は `default_max`（Keycloak 26 は子 group 一覧が 10）。
fn page<T: Clone>(items: Vec<T>, q: &Value, default_max: usize) -> Vec<T> {
    let num = |k: &str, d: usize| {
        q.get(k)
            .and_then(Value::as_str)
            .and_then(|s| s.parse().ok())
            .unwrap_or(d)
    };
    items
        .into_iter()
        .skip(num("first", 0))
        .take(num("max", default_max))
        .collect()
}

/// Keycloak の `search`: `exact=true` なら名前の完全一致、それ以外は大小無視の部分一致。
fn name_matches(name: &str, search: &str, q: &Value) -> bool {
    if q.get("exact").and_then(Value::as_str) == Some("true") {
        name == search
    } else {
        name.to_lowercase().contains(&search.to_lowercase())
    }
}

async fn list_groups(
    State(st): State<St>,
    Path(parent): Path<Option<String>>,
    Query(q): Query<Value>,
) -> Json<Value> {
    let kc = st.lock().unwrap();
    let level: Vec<MockGroup> = kc
        .groups
        .iter()
        .filter(|g| g.parent == parent)
        .cloned()
        .collect();
    // 子 group 一覧は max 省略時 10 件（Keycloak 26 の既定）。トップレベルは十分大きく取る。
    let default_max = if parent.is_some() { 10 } else { 100 };
    let hits: Vec<MockGroup> = match q.get("search").and_then(Value::as_str) {
        // 実 Keycloak と同じく、トップレベル検索は子孫に一致を含む親も返す。
        Some(name) if parent.is_none() => level
            .into_iter()
            .filter(|g| name_matches(&g.name, name, &q) || kc.has_descendant_named(&g.id, name))
            .collect(),
        Some(name) => level
            .into_iter()
            .filter(|g| name_matches(&g.name, name, &q))
            .collect(),
        None => level,
    };
    Json(Value::Array(
        page(hits, &q, default_max)
            .iter()
            .map(|g| kc.rep(g))
            .collect(),
    ))
}

async fn spawn_mock(st: St) -> String {
    let token = post(|| async { Json(json!({ "access_token": "t", "expires_in": 60 })) });
    let top = get({
        let st = st.clone();
        move |q: Query<Value>| list_groups(State(st.clone()), Path(None), q)
    })
    .post({
        let st = st.clone();
        move |Json(b): Json<Value>| {
            let st = st.clone();
            async move {
                let name = b["name"].as_str().unwrap().to_string();
                st.lock().unwrap().create(None, &name)
            }
        }
    });
    let children = get({
        let st = st.clone();
        move |Path(id): Path<String>, q: Query<Value>| {
            list_groups(State(st.clone()), Path(Some(id)), q)
        }
    })
    .post({
        let st = st.clone();
        move |Path(id): Path<String>, Json(b): Json<Value>| {
            let st = st.clone();
            async move {
                let name = b["name"].as_str().unwrap().to_string();
                st.lock().unwrap().create(Some(id), &name)
            }
        }
    });
    let group = get({
        let st = st.clone();
        move |Path(id): Path<String>| {
            let st = st.clone();
            async move {
                let kc = st.lock().unwrap();
                match kc.groups.iter().find(|g| g.id == id) {
                    Some(g) => Json(kc.rep(g)).into_response(),
                    None => StatusCode::NOT_FOUND.into_response(),
                }
            }
        }
    })
    .put({
        let st = st.clone();
        move |Path(id): Path<String>, Json(b): Json<Value>| {
            let st = st.clone();
            async move {
                let mut kc = st.lock().unwrap();
                let name = b["name"].as_str().unwrap().to_string();
                let Some(parent) = kc
                    .groups
                    .iter()
                    .find(|g| g.id == id)
                    .map(|g| g.parent.clone())
                else {
                    return StatusCode::NOT_FOUND;
                };
                if kc
                    .groups
                    .iter()
                    .any(|g| g.parent == parent && g.name == name && g.id != id)
                {
                    return StatusCode::CONFLICT;
                }
                if let Some(g) = kc.groups.iter_mut().find(|g| g.id == id) {
                    g.name = name;
                }
                StatusCode::NO_CONTENT
            }
        }
    })
    .delete({
        let st = st.clone();
        move |Path(id): Path<String>| {
            let st = st.clone();
            async move {
                let mut kc = st.lock().unwrap();
                if !kc.groups.iter().any(|g| g.id == id) {
                    return StatusCode::NOT_FOUND;
                }
                kc.delete_subtree(&id);
                StatusCode::NO_CONTENT
            }
        }
    });
    let members = get({
        let st = st.clone();
        move |Path(id): Path<String>, Query(q): Query<Value>| {
            let st = st.clone();
            async move {
                let kc = st.lock().unwrap();
                let users: Vec<Value> = kc
                    .members
                    .iter()
                    .filter(|(_, g)| *g == id)
                    .filter_map(|(u, _)| kc.users.iter().find(|x| &x.id == u))
                    .map(|u| {
                        let attrs = u
                            .tenant
                            .as_ref()
                            .map_or(json!({}), |t| json!({ "tenant": [t] }));
                        json!({ "id": u.id, "username": u.username, "attributes": attrs })
                    })
                    .collect();
                Json(Value::Array(page(users, &q, 100)))
            }
        }
    });
    let membership = put({
        let st = st.clone();
        move |Path((u, g)): Path<(String, String)>| {
            let st = st.clone();
            async move {
                let mut kc = st.lock().unwrap();
                if !kc.members.contains(&(u.clone(), g.clone())) {
                    kc.members.push((u, g));
                }
                StatusCode::NO_CONTENT
            }
        }
    })
    .delete({
        let st = st.clone();
        move |Path((u, g)): Path<(String, String)>| {
            let st = st.clone();
            async move {
                // LDAP_ONLY の同期 group を模す: `ldap-` で始まるユーザーは外せない。
                if u.starts_with("ldap-") {
                    return StatusCode::BAD_REQUEST;
                }
                st.lock()
                    .unwrap()
                    .members
                    .retain(|m| m != &(u.clone(), g.clone()));
                StatusCode::NO_CONTENT
            }
        }
    });
    let empty = || get(|| async { Json(json!([])) });
    let app = Router::new()
        .route("/realms/shiki/protocol/openid-connect/token", token)
        .route("/admin/realms/shiki/groups", top)
        .route("/admin/realms/shiki/groups/{id}", group)
        .route("/admin/realms/shiki/groups/{id}/children", children)
        .route("/admin/realms/shiki/groups/{id}/members", members)
        .route("/admin/realms/shiki/users/{u}/groups/{g}", membership)
        // provisioner に view-identity-providers が無い realm を模す。
        .route(
            "/admin/realms/shiki/identity-provider/instances",
            get(|| async { StatusCode::FORBIDDEN }),
        )
        .route(
            "/admin/realms/shiki/components",
            get(|| async {
                Json(json!([
                    { "name": "ad-groups", "config": { "groups.path": ["/acme/eng"] } },
                    { "name": "other", "config": { "groups.path": ["/acme2"] } },
                ]))
            }),
        )
        .route("/admin/realms/shiki/default-groups", empty());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

fn auth_config(base: &str) -> AuthConfig {
    AuthConfig {
        issuer: format!("{base}/realms/shiki"),
        internal_base_url: Some(format!("{base}/realms/shiki")),
        jwks_uri: None,
        audience: "shiki-api".into(),
        jwks_ttl_secs: 300,
        client_id: "shiki-web".into(),
        client_secret: None,
        redirect_uri: "http://localhost:3000/auth/callback".into(),
        post_logout_redirect_uri: "http://localhost:3000/".into(),
        scopes: "openid profile".into(),
        tenancy: Tenancy::Multi,
        tenant_id: Some("default".into()),
        provisioner_client_id: Some("shiki-provisioner".into()),
        provisioner_client_secret: Some("dev-secret".into()),
        admin_base_url: None,
    }
}

fn paths(st: &St) -> Vec<String> {
    let kc = st.lock().unwrap();
    let mut out: Vec<String> = kc.groups.iter().map(|g| kc.path(&g.id)).collect();
    out.sort();
    out
}

#[tokio::test]
async fn org_group_is_tenant_qualified_and_idempotent() {
    let st: St = Arc::default();
    // 同じ slug の旧トップレベル group と、他テナントの同名 org group が既にある。
    {
        let mut kc = st.lock().unwrap();
        kc.add_group(None, "acme");
        let t = kc.add_group(None, "tenants");
        let other = kc.add_group(Some(&t), "other");
        kc.add_group(Some(&other), "acme");
    }
    let base = spawn_mock(st.clone()).await;
    let http = reqwest::Client::new();
    let auth = auth_config(&base);
    let kc = KeycloakAdmin::from_config(&http, &auth).unwrap();
    let id = kc.ensure_org_group("acme-t", "acme").await.unwrap();
    assert_eq!(kc.ensure_org_group("acme-t", "acme").await.unwrap(), id);
    assert_eq!(
        paths(&st),
        vec![
            "/acme",
            "/tenants",
            "/tenants/acme-t",
            "/tenants/acme-t/acme",
            "/tenants/other",
            "/tenants/other/acme",
        ]
    );
    // 旧トップレベル `/acme` や他テナントの `/tenants/other/acme` を取り違えない。
    let found = kc
        .find_group_path(&["tenants", "acme-t", "acme"])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.id, id);
    assert_eq!(found.path, "/tenants/acme-t/acme");
    assert!(kc
        .find_group_path(&["tenants", "missing", "acme"])
        .await
        .unwrap()
        .is_none());
    assert!(kc.ensure_group_path(&[]).await.is_err());
}

#[tokio::test]
async fn delete_tenant_groups_keeps_other_tenants() {
    let st: St = Arc::default();
    {
        let mut kc = st.lock().unwrap();
        let t = kc.add_group(None, "tenants");
        for tenant in ["a", "b"] {
            let root = kc.add_group(Some(&t), tenant);
            let org = kc.add_group(Some(&root), "shared");
            kc.add_group(Some(&org), "eng");
        }
    }
    let base = spawn_mock(st.clone()).await;
    let http = reqwest::Client::new();
    let auth = auth_config(&base);
    let kc = KeycloakAdmin::from_config(&http, &auth).unwrap();
    // 同じ org slug `shared` を使う他テナント b の group は残る（共有判定は不要）。
    assert!(kc.delete_tenant_groups("a").await.unwrap());
    assert!(!kc.delete_tenant_groups("a").await.unwrap());
    assert_eq!(
        paths(&st),
        vec![
            "/tenants",
            "/tenants/b",
            "/tenants/b/shared",
            "/tenants/b/shared/eng"
        ]
    );
}

#[tokio::test]
async fn rename_tenant_group_moves_subtree() {
    let st: St = Arc::default();
    let eng;
    {
        let mut kc = st.lock().unwrap();
        let t = kc.add_group(None, "tenants");
        let from = kc.add_group(Some(&t), "from");
        let org = kc.add_group(Some(&from), "acme");
        eng = kc.add_group(Some(&org), "eng");
        kc.add_group(Some(&t), "taken");
        kc.add_user("u1", Some("from"), &[eng.as_str()]);
    }
    let base = spawn_mock(st.clone()).await;
    let http = reqwest::Client::new();
    let auth = auth_config(&base);
    let kc = KeycloakAdmin::from_config(&http, &auth).unwrap();
    assert_eq!(
        kc.rename_tenant_group("from", "taken").await.unwrap(),
        RenameOutcome::TargetExists
    );
    assert_eq!(
        kc.rename_tenant_group("from", "to").await.unwrap(),
        RenameOutcome::Renamed
    );
    assert_eq!(
        kc.rename_tenant_group("from", "to").await.unwrap(),
        RenameOutcome::Missing
    );
    // メンバーシップは group id に付くので、配下ごと新しいパスになる。
    assert_eq!(
        st.lock().unwrap().user_paths("u1"),
        vec!["/tenants/to/acme/eng"]
    );
}

/// 一覧 → 選定 → 読み取り → 計画 → 適用（CLI と同じ順）で 1 回分を流す。
async fn migrate_once(kc: &KeycloakAdmin<'_>, registry: &[&str]) -> Vec<String> {
    let realm = list_realm_groups(kc).await.unwrap();
    let names: Vec<String> = realm.legacy.iter().map(|g| g.name.clone()).collect();
    let registry: std::collections::BTreeSet<String> =
        registry.iter().map(|s| s.to_string()).collect();
    let sel = select_orgs(&names, &registry, &realm.migrated, &[], &[]);
    let mut failed = Vec::new();
    for slug in &sel.selected {
        let top = realm.legacy.iter().find(|g| &g.name == slug).unwrap();
        let tree = snapshot_tree(kc, top).await.unwrap();
        let already = realm.migrated.get(slug).cloned().unwrap_or_default();
        let plan = plan_tree(&tree, &TenantRule::Attribute, &[], &already);
        failed.extend(apply_tree(kc, &plan).await.unwrap().failed_removals);
    }
    failed
}

#[tokio::test]
async fn migrate_legacy_groups_end_to_end() {
    let st: St = Arc::default();
    {
        let mut kc = st.lock().unwrap();
        // `/acme` を 2 テナントが共有（t1: u1, t2: u2）。部署 `/acme/eng` も持つ。
        let acme = kc.add_group(None, "acme");
        let eng = kc.add_group(Some(&acme), "eng");
        if let Some(g) = kc.groups.iter_mut().find(|g| g.id == acme) {
            g.realm_roles = vec!["offline_access".into()];
        }
        kc.add_user("u1", Some("t1"), &[acme.as_str(), eng.as_str()]);
        kc.add_user("u2", Some("t2"), &[eng.as_str()]);
        // `/orphan` には tenant 属性の無いメンバーが居る → 保持。
        let orphan = kc.add_group(None, "orphan");
        kc.add_user("u3", None, &[orphan.as_str()]);
        // 運用 group（登録簿に無い）→ 対象外。`/tenants/t1/aaa-ops` に複製されると org が入れ替わる。
        let ops = kc.add_group(None, "aaa-ops");
        kc.add_user("u5", Some("t1"), &[ops.as_str()]);
        // LDAP 同期 group（メンバーを外せない）→ 新 group へは入れ、旧 group は保持して報告。
        let ldap = kc.add_group(None, "ldapco");
        kc.add_user("ldap-u", Some("t4"), &[ldap.as_str()]);
        // 既存の新形式 group は対象外。前回実行で移動だけ済んだ空の `/done` は削除まで進む。
        let t = kc.add_group(None, "tenants");
        let t3 = kc.add_group(Some(&t), "t3");
        let keep = kc.add_group(Some(&t3), "keep");
        kc.add_user("u4", Some("t3"), &[keep.as_str()]);
        kc.add_group(Some(&t3), "done");
        kc.add_group(None, "done");
    }
    let base = spawn_mock(st.clone()).await;
    let http = reqwest::Client::new();
    let auth = auth_config(&base);
    let kc = KeycloakAdmin::from_config(&http, &auth).unwrap();

    let realm = list_realm_groups(&kc).await.unwrap();
    let names: Vec<String> = realm.legacy.iter().map(|g| g.name.clone()).collect();
    assert_eq!(names, vec!["acme", "orphan", "aaa-ops", "ldapco", "done"]);
    assert_eq!(realm.migrated.get("done"), Some(&vec!["t3".to_string()]));
    let registry = ["acme", "orphan", "ldapco"];
    let sel = select_orgs(
        &names,
        &registry.iter().map(|s| s.to_string()).collect(),
        &realm.migrated,
        &[],
        &[],
    );
    assert_eq!(sel.selected, vec!["acme", "done", "ldapco", "orphan"]);
    assert_eq!(sel.skipped.len(), 1);
    assert_eq!(sel.skipped[0].0, "aaa-ops");

    let acme_top = realm.legacy.iter().find(|g| g.name == "acme").unwrap();
    let acme = snapshot_tree(&kc, acme_top).await.unwrap();
    let node_paths: Vec<String> = acme.nodes.iter().map(|n| n.segments.join("/")).collect();
    assert_eq!(node_paths, vec!["acme", "acme/eng"]);
    assert_eq!(
        plan_tree(&acme, &TenantRule::Attribute, &[], &[]).role_mappings,
        1
    );

    // 参照検査は種別ごとに独立: IdP（403）が見られなくても LDAP マッパーの参照は報告する。
    let refs = kc.group_path_references(&["/acme".into()]).await;
    assert_eq!(refs.found, vec!["LDAP マッパー ad-groups: /acme/eng"]);
    assert_eq!(refs.unchecked.len(), 1);
    assert!(refs.unchecked[0].starts_with("IdP マッパー"));

    let failed = migrate_once(&kc, &registry).await;
    assert_eq!(failed.len(), 1);
    assert!(failed[0].starts_with("ldap-u-name (/ldapco)"));
    // 再実行しても収束する（冪等）。
    let failed = migrate_once(&kc, &registry).await;
    assert_eq!(failed.len(), 1);

    {
        let kc_state = st.lock().unwrap();
        assert_eq!(
            kc_state.user_paths("u1"),
            vec!["/tenants/t1/acme", "/tenants/t1/acme/eng"]
        );
        assert_eq!(kc_state.user_paths("u2"), vec!["/tenants/t2/acme/eng"]);
        // tenant を決められない u3 は動かさず、旧 `/orphan` も残す。
        assert_eq!(kc_state.user_paths("u3"), vec!["/orphan"]);
        assert_eq!(kc_state.user_paths("u4"), vec!["/tenants/t3/keep"]);
        // 対象外の運用 group は触らない。
        assert_eq!(kc_state.user_paths("u5"), vec!["/aaa-ops"]);
        assert_eq!(
            kc_state.user_paths("ldap-u"),
            vec!["/ldapco", "/tenants/t4/ldapco"]
        );
    }
    // 旧 `/acme` は subtree ごと消え、テナントごとに複製された。空の `/done` も消えた。
    assert_eq!(
        paths(&st),
        vec![
            "/aaa-ops",
            "/ldapco",
            "/orphan",
            "/tenants",
            "/tenants/t1",
            "/tenants/t1/acme",
            "/tenants/t1/acme/eng",
            "/tenants/t2",
            "/tenants/t2/acme",
            "/tenants/t2/acme/eng",
            "/tenants/t3",
            "/tenants/t3/done",
            "/tenants/t3/keep",
            "/tenants/t4",
            "/tenants/t4/ldapco",
        ]
    );
}

#[tokio::test]
async fn group_listing_reads_past_keycloak_default_page() {
    // Keycloak 26 の子 group 一覧は max 省略時 10 件。12 テナント・12 兄弟でも取りこぼさない。
    let st: St = Arc::default();
    {
        let mut kc = st.lock().unwrap();
        let t = kc.add_group(None, "tenants");
        for i in 0..12 {
            let tenant = kc.add_group(Some(&t), &format!("t{i:02}"));
            kc.add_group(Some(&tenant), "org");
        }
        // 部分一致では `t1` が `t10`・`t11` にも当たる。exact で `t11` だけを引く。
    }
    let base = spawn_mock(st.clone()).await;
    let http = reqwest::Client::new();
    let auth = auth_config(&base);
    let kc = KeycloakAdmin::from_config(&http, &auth).unwrap();
    let realm = list_realm_groups(&kc).await.unwrap();
    assert_eq!(realm.migrated.get("org").map(Vec::len), Some(12));
    let found = kc
        .find_group_path(&["tenants", "t11", "org"])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.path, "/tenants/t11/org");
    assert!(kc
        .find_group_path(&["tenants", "t1", "org"])
        .await
        .unwrap()
        .is_none());
}

/// 実 Keycloak での確認（`KEYCLOAK_TEST_URL`＝realm `shiki` を import 済みの Keycloak の base URL。
/// 無ければスキップ）。provisioner は realm import の dev 資格情報を使う。
#[tokio::test]
async fn real_keycloak_tenant_group_lifecycle() {
    let Ok(base) = std::env::var("KEYCLOAK_TEST_URL") else {
        eprintln!("KEYCLOAK_TEST_URL 未設定のためスキップ");
        return;
    };
    let mut auth = auth_config(&base);
    auth.provisioner_client_secret = Some("shiki-provisioner-dev-secret".into());
    let http = reqwest::Client::new();
    let kc = KeycloakAdmin::from_config(&http, &auth).unwrap();
    let tenant = format!("it{}", uuid::Uuid::new_v4().simple());
    let renamed = format!("{tenant}-r");

    // 作成は冪等で、初期 admin はテナント修飾の org group に入る。
    let org_id = kc.ensure_org_group(&tenant, "acme").await.unwrap();
    assert_eq!(kc.ensure_org_group(&tenant, "acme").await.unwrap(), org_id);
    let username = format!("{tenant}-admin");
    let (user_id, _) = kc
        .ensure_tenant_admin(
            &tenant,
            "acme",
            &username,
            &format!("{username}@x.example"),
            "Tmp-pass-1",
        )
        .await
        .unwrap();
    let members = kc.group_members(&org_id).await.unwrap();
    assert_eq!(
        members.iter().map(|u| u.id.as_str()).collect::<Vec<_>>(),
        vec![user_id.as_str()]
    );

    // リネームで配下ごと移る（メンバーシップは group id に付く）。
    assert_eq!(
        kc.rename_tenant_group(&tenant, &renamed).await.unwrap(),
        RenameOutcome::Renamed
    );
    let moved = kc
        .find_group_path(&["tenants", &renamed, "acme"])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(moved.id, org_id);
    assert_eq!(moved.path, format!("/tenants/{renamed}/acme"));
    assert!(kc
        .find_group_path(&["tenants", &tenant])
        .await
        .unwrap()
        .is_none());

    // 撤去は自テナントの subtree だけ。
    assert!(kc.delete_tenant_groups(&renamed).await.unwrap());
    assert!(kc
        .find_group_path(&["tenants", &renamed])
        .await
        .unwrap()
        .is_none());
    assert!(kc.find_group_path(&["tenants"]).await.unwrap().is_some());
    kc.delete_user(&user_id).await.unwrap();
}

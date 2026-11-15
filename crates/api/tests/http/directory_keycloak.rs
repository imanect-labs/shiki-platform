//! 管理RESTの契約を再現するテスト専用Keycloak。実装からは参照しない。
use super::*;
use axum::{
    extract::{Request, State},
    response::IntoResponse,
    Json, Router,
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Mutex};
#[derive(Default)]
pub(super) struct KeycloakFixture {
    pub components: Vec<Value>,
    pub groups: Vec<Value>,
    pub writes: Vec<Value>,
    pub fail: bool,
}
impl KeycloakFixture {
    fn path(&self, parent: Option<&str>, name: &str) -> String {
        format!(
            "{}/{}",
            parent
                .and_then(|id| self.groups.iter().find(|g| g["id"] == id))
                .and_then(|g| g["path"].as_str())
                .unwrap_or(""),
            name
        )
    }
    pub(super) fn add_group(
        &mut self,
        parent: Option<&str>,
        name: &str,
        attributes: Value,
    ) -> String {
        let id = format!("group-{}", self.groups.len());
        self.groups.push(json!({"id":id,"name":name,"parent":parent,"path":self.path(parent, name),"subGroupCount":0,"attributes":attributes}));
        id
    }
}
pub(super) async fn spawn() -> (String, Arc<Mutex<KeycloakFixture>>) {
    let fixture = Arc::new(Mutex::new(KeycloakFixture::default()));
    let app = Router::new()
        .fallback(axum::routing::any(handle))
        .with_state(fixture.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}"), fixture)
}
async fn handle(
    State(state): State<Arc<Mutex<KeycloakFixture>>>,
    request: Request,
) -> axum::response::Response {
    let method = request.method().to_string();
    let path = request.uri().path().to_string();
    let query: BTreeMap<String, String> =
        url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    let bytes = axum::body::to_bytes(request.into_body(), 2_000_000)
        .await
        .unwrap();
    let mut body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let mut state = state.lock().unwrap();
    if path.ends_with("/token") {
        return Json(json!({"access_token":"admin-token","expires_in":300})).into_response();
    }
    if method != "GET" {
        state
            .writes
            .push(json!({"method":method,"path":path,"body":body}));
    }
    if state.fail {
        return (StatusCode::BAD_GATEWAY, "sensitive-bind-secret").into_response();
    }
    let root = "/admin/realms/shiki";
    let path = path.strip_prefix(root).unwrap_or(&path);
    if path.is_empty() {
        return Json(json!({"id":"realm-id"})).into_response();
    }
    if path == "/testLDAPConnection" {
        return StatusCode::NO_CONTENT.into_response();
    }
    if path.starts_with("/user-storage/") {
        assert_eq!(
            query.get("direction").map(String::as_str),
            Some("fedToKeycloak")
        );
        return Json(json!({"failed":0})).into_response();
    }
    if path == "/components" {
        if method == "GET" {
            let matches: Vec<_> = state
                .components
                .iter()
                .filter(|c| {
                    query.get("type").is_none_or(|v| c["providerType"] == *v)
                        && query.get("name").is_none_or(|v| c["name"] == *v)
                        && query.get("parent").is_none_or(|v| c["parentId"] == *v)
                })
                .cloned()
                .collect();
            return Json(matches).into_response();
        }
        body["id"] = json!(format!("component-{}", state.components.len()));
        state.components.push(body);
        return StatusCode::CREATED.into_response();
    }
    if let Some(encoded) = path.strip_prefix("/components/") {
        let decoded = percent_encoding::percent_decode_str(encoded)
            .decode_utf8()
            .unwrap();
        let id = decoded.as_ref();
        if method == "DELETE" {
            state.components.retain(|c| c["id"] != id);
            return StatusCode::NO_CONTENT.into_response();
        }
        let Some(existing) = state.components.iter_mut().find(|c| c["id"] == id) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        if body["config"]["bindCredential"][0] == "**********" {
            body["config"]["bindCredential"] = existing["config"]["bindCredential"].clone();
        }
        body["id"] = json!(id);
        *existing = body;
        return StatusCode::NO_CONTENT.into_response();
    }
    if path.starts_with("/groups") {
        let tail = path.trim_start_matches("/groups").trim_start_matches('/');
        let decoded = percent_encoding::percent_decode_str(tail)
            .decode_utf8()
            .unwrap()
            .to_string();
        let (parent, listing) = if decoded.is_empty() {
            (None, true)
        } else if let Some(id) = decoded.strip_suffix("/children") {
            (Some(id), true)
        } else {
            (Some(decoded.as_str()), false)
        };
        if listing {
            if method == "POST" {
                let name = body["name"].as_str().unwrap();
                if state
                    .groups
                    .iter()
                    .any(|g| g["parent"].as_str() == parent && g["name"] == name)
                {
                    return StatusCode::CONFLICT.into_response();
                }
                state.add_group(parent, name, json!({}));
                return StatusCode::CREATED.into_response();
            }
            let first = query.get("first").and_then(|v| v.parse().ok()).unwrap_or(0);
            let limit = query.get("max").and_then(|v| v.parse().ok()).unwrap_or(100);
            let matches: Vec<_> = state
                .groups
                .iter()
                .filter(|g| {
                    g["parent"].as_str() == parent
                        && query.get("search").is_none_or(|name| g["name"] == *name)
                })
                .skip(first)
                .take(limit)
                .map(|g| {
                    let mut rep = g.clone();
                    rep["subGroupCount"] = json!(state
                        .groups
                        .iter()
                        .filter(|child| child["parent"] == g["id"])
                        .count());
                    rep
                })
                .collect();
            return Json(matches).into_response();
        }
        if method == "DELETE" {
            let prefix = state
                .groups
                .iter()
                .find(|g| g["id"].as_str() == parent)
                .and_then(|g| g["path"].as_str())
                .map(str::to_string);
            if let Some(prefix) = prefix {
                state.groups.retain(|g| {
                    let p = g["path"].as_str().unwrap();
                    p != prefix && !p.starts_with(&format!("{prefix}/"))
                });
            }
            return StatusCode::NO_CONTENT.into_response();
        }
        if let Some(group) = state.groups.iter_mut().find(|g| g["id"].as_str() == parent) {
            if method == "PUT" {
                group["attributes"] = body["attributes"].clone();
                group["name"] = body["name"].clone();
                return StatusCode::NO_CONTENT.into_response();
            }
            return Json(group.clone()).into_response();
        }
    }
    StatusCode::NOT_FOUND.into_response()
}

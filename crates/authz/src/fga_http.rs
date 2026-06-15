//! OpenFGA HTTP API の薄い直叩きクライアント（公式 SDK 不使用）。
//!
//! Phase 0 で必要な操作のみ実装する: store の一覧/作成、authorization model の
//! 読み書き、check、tuple の write。チョークポイントの内部実装であり、
//! 外には [`crate::client::AuthzClient`] トレイトだけを公開する。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::AuthzError;

/// OpenFGA への低レベル HTTP クライアント。
#[derive(Clone)]
pub struct FgaHttp {
    http: reqwest::Client,
    base_url: String,
}

#[derive(Debug, Deserialize)]
struct Store {
    id: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct ListStoresResponse {
    #[serde(default)]
    stores: Vec<Store>,
}

#[derive(Debug, Deserialize)]
struct CreateStoreResponse {
    id: String,
}

#[derive(Debug, Deserialize)]
struct AuthorizationModel {
    id: String,
}

#[derive(Debug, Deserialize)]
struct ReadModelsResponse {
    #[serde(default)]
    authorization_models: Vec<AuthorizationModel>,
}

#[derive(Debug, Deserialize)]
struct WriteModelResponse {
    authorization_model_id: String,
}

#[derive(Debug, Serialize)]
struct CheckRequest<'a> {
    tuple_key: TupleKey<'a>,
    authorization_model_id: &'a str,
}

#[derive(Debug, Serialize)]
struct TupleKey<'a> {
    user: &'a str,
    relation: &'a str,
    object: &'a str,
}

#[derive(Debug, Deserialize)]
struct CheckResponse {
    #[serde(default)]
    allowed: bool,
}

impl FgaHttp {
    pub fn new(http: reqwest::Client, base_url: impl Into<String>) -> Self {
        let base_url = base_url.into();
        FgaHttp {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// 指定名の store の id を返す（無ければ `None`）。
    ///
    /// ListStores の `name` クエリで完全一致絞り込みを行う（OpenFGA v1.4+）。
    /// 全件ページングに頼らないため、store 数が 1 ページを超えても取りこぼさない。
    pub async fn find_store(&self, name: &str) -> Result<Option<String>, AuthzError> {
        let url = format!("{}/stores", self.base_url);
        let resp = self.http.get(&url).query(&[("name", name)]).send().await?;
        let resp = ensure_ok(resp).await?;
        let parsed: ListStoresResponse = resp.json().await?;
        Ok(parsed
            .stores
            .into_iter()
            .find(|s| s.name == name)
            .map(|s| s.id))
    }

    pub async fn create_store(&self, name: &str) -> Result<String, AuthzError> {
        let url = format!("{}/stores", self.base_url);
        let resp = self
            .http
            .post(&url)
            .json(&serde_json::json!({ "name": name }))
            .send()
            .await?;
        let resp = ensure_ok(resp).await?;
        let parsed: CreateStoreResponse = resp.json().await?;
        Ok(parsed.id)
    }

    /// 最新の authorization model id を返す（OpenFGA は新しい順で返す）。
    pub async fn latest_model_id(&self, store_id: &str) -> Result<Option<String>, AuthzError> {
        let url = format!(
            "{}/stores/{}/authorization-models?page_size=1",
            self.base_url, store_id
        );
        let resp = self.http.get(&url).send().await?;
        let resp = ensure_ok(resp).await?;
        let parsed: ReadModelsResponse = resp.json().await?;
        Ok(parsed.authorization_models.into_iter().next().map(|m| m.id))
    }

    /// authorization model 本体（type_definitions 等を含む JSON）を取得する。
    pub async fn get_model(&self, store_id: &str, model_id: &str) -> Result<Value, AuthzError> {
        let url = format!(
            "{}/stores/{}/authorization-models/{}",
            self.base_url, store_id, model_id
        );
        let resp = self.http.get(&url).send().await?;
        let resp = ensure_ok(resp).await?;
        let mut body: Value = resp.json().await?;
        // {"authorization_model": {...}} を取り出す。
        Ok(body
            .get_mut("authorization_model")
            .map(Value::take)
            .unwrap_or(Value::Null))
    }

    /// authorization model を書き込み、新しい model id を返す。
    pub async fn write_model(&self, store_id: &str, model: &Value) -> Result<String, AuthzError> {
        let url = format!("{}/stores/{}/authorization-models", self.base_url, store_id);
        let resp = self.http.post(&url).json(model).send().await?;
        let resp = ensure_ok(resp).await?;
        let parsed: WriteModelResponse = resp.json().await?;
        Ok(parsed.authorization_model_id)
    }

    pub async fn check(
        &self,
        store_id: &str,
        model_id: &str,
        user: &str,
        relation: &str,
        object: &str,
    ) -> Result<bool, AuthzError> {
        let url = format!("{}/stores/{}/check", self.base_url, store_id);
        let body = CheckRequest {
            tuple_key: TupleKey {
                user,
                relation,
                object,
            },
            authorization_model_id: model_id,
        };
        let resp = self.http.post(&url).json(&body).send().await?;
        let resp = ensure_ok(resp).await?;
        let parsed: CheckResponse = resp.json().await?;
        Ok(parsed.allowed)
    }

    /// tuple を書き込む（主にテスト・初期データ投入用）。
    pub async fn write_tuple(
        &self,
        store_id: &str,
        model_id: &str,
        user: &str,
        relation: &str,
        object: &str,
    ) -> Result<(), AuthzError> {
        let url = format!("{}/stores/{}/write", self.base_url, store_id);
        let body = serde_json::json!({
            "writes": { "tuple_keys": [{ "user": user, "relation": relation, "object": object }] },
            "authorization_model_id": model_id,
        });
        let resp = self.http.post(&url).json(&body).send().await?;
        ensure_ok(resp).await?;
        Ok(())
    }
}

async fn ensure_ok(resp: reqwest::Response) -> Result<reqwest::Response, AuthzError> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    Err(AuthzError::Unexpected { status, body })
}

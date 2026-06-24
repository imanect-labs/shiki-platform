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
    /// PIT-11: 共有/共有解除を即時に反映させるため強整合で問い合わせる。
    consistency: &'a str,
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

/// OpenFGA Read API の応答 1 件（`object#relation@user` の実タプル）。
#[derive(Debug, Clone, Deserialize)]
pub struct ReadTupleKey {
    pub user: String,
    pub relation: String,
    pub object: String,
}

#[derive(Debug, Deserialize)]
struct ReadTuple {
    key: ReadTupleKey,
}

#[derive(Debug, Deserialize)]
struct ReadResponse {
    #[serde(default)]
    tuples: Vec<ReadTuple>,
    #[serde(default)]
    continuation_token: String,
}

#[derive(Debug, Deserialize)]
struct ListObjectsResponse {
    #[serde(default)]
    objects: Vec<String>,
}

/// 強整合（書込直後の剥奪/付与を即座に反映）。PIT-11 の `HIGHER_CONSISTENCY`。
const HIGHER_CONSISTENCY: &str = "HIGHER_CONSISTENCY";

/// Read API の 1 ページの最大件数（共有相手は通常少数だが上限ループの歩幅）。
const READ_PAGE_SIZE: u32 = 100;

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

    #[allow(clippy::too_many_arguments)] // check に必要な識別子＋整合性一式。
    pub async fn check(
        &self,
        store_id: &str,
        model_id: &str,
        user: &str,
        relation: &str,
        object: &str,
        consistency: &str,
    ) -> Result<bool, AuthzError> {
        let url = format!("{}/stores/{}/check", self.base_url, store_id);
        let body = CheckRequest {
            tuple_key: TupleKey {
                user,
                relation,
                object,
            },
            authorization_model_id: model_id,
            consistency,
        };
        let resp = self.http.post(&url).json(&body).send().await?;
        let resp = ensure_ok(resp).await?;
        let parsed: CheckResponse = resp.json().await?;
        Ok(parsed.allowed)
    }

    /// オブジェクトに張られた tuple を列挙する（共有相手一覧）。
    ///
    /// `relation` 指定時はその relation のみ、`None` なら object の全 relation を返す。
    /// continuation_token を辿って全ページを集める（共有相手は通常少数）。
    pub async fn read_tuples(
        &self,
        store_id: &str,
        object: &str,
        relation: Option<&str>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        let url = format!("{}/stores/{}/read", self.base_url, store_id);
        let mut out = Vec::new();
        let mut token = String::new();
        loop {
            // tuple_key は object 必須。relation は任意（None で object の全 relation）。
            let mut tuple_key = serde_json::Map::new();
            tuple_key.insert("object".into(), Value::from(object));
            if let Some(r) = relation {
                tuple_key.insert("relation".into(), Value::from(r));
            }
            let mut body = serde_json::json!({
                "tuple_key": tuple_key,
                "page_size": READ_PAGE_SIZE,
            });
            if !token.is_empty() {
                body["continuation_token"] = Value::from(token.clone());
            }
            let resp = self.http.post(&url).json(&body).send().await?;
            let resp = ensure_ok(resp).await?;
            let parsed: ReadResponse = resp.json().await?;
            out.extend(parsed.tuples.into_iter().map(|t| t.key));
            if parsed.continuation_token.is_empty() {
                break;
            }
            token = parsed.continuation_token;
        }
        Ok(out)
    }

    /// `user` が `relation` を持つ `type` のオブジェクト id 一覧（共有された一覧）。
    ///
    /// ReBAC の継承（部署メンバー・親フォルダ）も解決した実効集合を返す。
    pub async fn list_objects(
        &self,
        store_id: &str,
        model_id: &str,
        object_type: &str,
        relation: &str,
        user: &str,
    ) -> Result<Vec<String>, AuthzError> {
        let url = format!("{}/stores/{}/list-objects", self.base_url, store_id);
        let body = serde_json::json!({
            "authorization_model_id": model_id,
            "type": object_type,
            "relation": relation,
            "user": user,
            "consistency": HIGHER_CONSISTENCY,
        });
        let resp = self.http.post(&url).json(&body).send().await?;
        let resp = ensure_ok(resp).await?;
        let parsed: ListObjectsResponse = resp.json().await?;
        Ok(parsed.objects)
    }

    /// tuple を書き込む（owner/parent 付与・初期データ投入等）。**実際に書き込んだら `true`**、
    /// 既存で no-op なら `false` を返す。
    ///
    /// **冪等**: 既に存在する tuple の再書込（OpenFGA は重複を 400 で拒否）は成功扱いにする。
    /// これにより失敗した tuple 書込を同一操作の再試行で安全に修復できる（dual-write の収束性）。
    /// 返す bool は補償ロールバックを「実変更時のみ」に限定するために使う（冪等 no-op を巻き戻さない）。
    pub async fn write_tuple(
        &self,
        store_id: &str,
        model_id: &str,
        user: &str,
        relation: &str,
        object: &str,
    ) -> Result<bool, AuthzError> {
        let url = format!("{}/stores/{}/write", self.base_url, store_id);
        let body = serde_json::json!({
            "writes": { "tuple_keys": [{ "user": user, "relation": relation, "object": object }] },
            "authorization_model_id": model_id,
        });
        let resp = self.http.post(&url).json(&body).send().await?;
        ensure_ok_idempotent(resp, "already exists").await
    }

    /// tuple を剥奪する（共有解除・ノード削除等）。**実際に削除したら `true`**、
    /// 不在で no-op なら `false` を返す。
    ///
    /// **冪等**: 存在しない tuple の削除（OpenFGA は 400 で拒否）は成功扱いにする。
    pub async fn delete_tuple(
        &self,
        store_id: &str,
        model_id: &str,
        user: &str,
        relation: &str,
        object: &str,
    ) -> Result<bool, AuthzError> {
        let url = format!("{}/stores/{}/write", self.base_url, store_id);
        let body = serde_json::json!({
            "deletes": { "tuple_keys": [{ "user": user, "relation": relation, "object": object }] },
            "authorization_model_id": model_id,
        });
        let resp = self.http.post(&url).json(&body).send().await?;
        ensure_ok_idempotent(resp, "does not exist").await
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

/// OpenFGA が重複書込/不在削除に使う検証エラーコード。
const FGA_INVALID_INPUT_CODE: &str = "write_failed_due_to_invalid_input";

/// write/delete の冪等化: 2xx なら **実変更ありで `true`**。400 の **構造化 `code`** が
/// 検証エラーで、かつ `message` に `idempotent_marker`（"already exists" / "does not exist"）を
/// 含むなら **no-op 成功で `false`**。それ以外は `Err`。
///
/// OpenFGA は重複書込と不在削除を同一 `code` で返すため、両者の区別には `message` を併用する。
/// 生の本文部分一致でなく `code` でゲートすることで、無関係な 400 を握り潰す事故を減らす
/// （`code` 自体が将来変わればフェイルクローズ＝エラーになる側に倒れる）。返す bool は
/// 「実際に ACL を変えたか」を表し、補償ロールバックを実変更時のみに限定するのに使う。
async fn ensure_ok_idempotent(
    resp: reqwest::Response,
    idempotent_marker: &str,
) -> Result<bool, AuthzError> {
    if resp.status().is_success() {
        return Ok(true);
    }
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    if status == 400 {
        if let Ok(parsed) = serde_json::from_str::<Value>(&body) {
            let code = parsed
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let message = parsed
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if code == FGA_INVALID_INPUT_CODE && message.contains(idempotent_marker) {
                return Ok(false);
            }
        }
    }
    Err(AuthzError::Unexpected { status, body })
}

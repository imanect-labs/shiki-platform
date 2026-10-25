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
    #[serde(default)]
    continuation_token: String,
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

#[derive(Debug, Serialize)]
struct BatchCheckItem<'a> {
    tuple_key: TupleKey<'a>,
    /// 応答の対応付けに使う id（`^[\w\d-]{1,36}$`）。入力の添字をそのまま使う。
    correlation_id: String,
}

#[derive(Debug, Serialize)]
struct BatchCheckRequest<'a> {
    checks: Vec<BatchCheckItem<'a>>,
    authorization_model_id: &'a str,
    consistency: &'a str,
}

#[derive(Debug, Deserialize)]
struct BatchCheckResponse {
    #[serde(default)]
    result: std::collections::HashMap<String, BatchCheckSingle>,
}

#[derive(Debug, Deserialize)]
struct BatchCheckSingle {
    #[serde(default)]
    allowed: bool,
    #[serde(default)]
    error: Option<Value>,
}

/// BatchCheck 1 回あたりの最大問数（OpenFGA の既定 `maxChecksPerBatchCheck`）。
pub const MAX_CHECKS_PER_BATCH: usize = 50;

/// BatchCheck の応答を入力順の判定列へ戻す。
///
/// 1 問でもエラー・欠落があれば全体をエラーにする（単発 `check` の失敗と同じく fail-closed。
/// 「判定できなかった問い」を拒否扱いで黙って続けると、上位で権限不足と通信障害を区別できない）。
fn decode_batch_check(resp: BatchCheckResponse, n: usize) -> Result<Vec<bool>, AuthzError> {
    let mut result = resp.result;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let Some(single) = result.remove(&i.to_string()) else {
            return Err(AuthzError::Unexpected {
                status: 200,
                body: format!("batch-check の応答に correlation_id {i} がありません"),
            });
        };
        if let Some(err) = single.error {
            return Err(AuthzError::Unexpected {
                status: 200,
                body: format!("batch-check の問い {i} が失敗しました: {err}"),
            });
        }
        out.push(single.allowed);
    }
    Ok(out)
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
    /// ListStores の `name` クエリで完全一致絞り込みを試みつつ（OpenFGA v1.8+）、
    /// フィルタ未対応版が全 store を返す場合に備えて continuation_token で全ページを
    /// 走査する。1 ページ目だけ見て「無い」と誤判定すると **起動のたびに同名 store を
    /// 新規作成**し、既存 tuple（権限）を全て置き去りにする事故になる。
    pub async fn find_store(&self, name: &str) -> Result<Option<String>, AuthzError> {
        let url = format!("{}/stores", self.base_url);
        let mut continuation = String::new();
        loop {
            let mut query: Vec<(&str, &str)> = vec![("name", name), ("page_size", "100")];
            if !continuation.is_empty() {
                query.push(("continuation_token", &continuation));
            }
            let resp = self.http.get(&url).query(&query).send().await?;
            let resp = ensure_ok(resp).await?;
            let parsed: ListStoresResponse = resp.json().await?;
            if let Some(store) = parsed.stores.into_iter().find(|s| s.name == name) {
                return Ok(Some(store.id));
            }
            if parsed.continuation_token.is_empty() {
                return Ok(None);
            }
            continuation = parsed.continuation_token;
        }
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
            .map_or(Value::Null, Value::take))
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

    /// 複数の `(user, relation, object)` を **1 往復**で判定する（OpenFGA v1.8+ の BatchCheck）。
    ///
    /// 結果は `items` と同じ順。1 回の問数は [`MAX_CHECKS_PER_BATCH`] 以下であること
    /// （分割は呼び出し側＝[`crate::client::OpenFgaClient`] が行う）。
    pub async fn batch_check(
        &self,
        store_id: &str,
        model_id: &str,
        items: &[(&str, &str, &str)],
        consistency: &str,
    ) -> Result<Vec<bool>, AuthzError> {
        if items.is_empty() {
            return Ok(Vec::new());
        }
        if items.len() > MAX_CHECKS_PER_BATCH {
            return Err(AuthzError::Unexpected {
                status: 0,
                body: format!(
                    "batch-check の問数 {} が上限 {MAX_CHECKS_PER_BATCH} を超えています",
                    items.len()
                ),
            });
        }
        let url = format!("{}/stores/{}/batch-check", self.base_url, store_id);
        let body = BatchCheckRequest {
            checks: items
                .iter()
                .enumerate()
                .map(|(i, (user, relation, object))| BatchCheckItem {
                    tuple_key: TupleKey {
                        user,
                        relation,
                        object,
                    },
                    correlation_id: i.to_string(),
                })
                .collect(),
            authorization_model_id: model_id,
            consistency,
        };
        let resp = self.http.post(&url).json(&body).send().await?;
        let resp = ensure_ok(resp).await?;
        let parsed: BatchCheckResponse = resp.json().await?;
        decode_batch_check(parsed, items.len())
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

    /// 複数 tuple を deletes 配列でまとめて剥奪する（テナント撤去・SAAS.2）。
    ///
    /// OpenFGA の write API は 1 リクエスト最大 100 tuple のため 100 件ずつチャンクする。
    /// 剥奪を試行して成功した件数を返す。並行削除等で「既に不在」のチャンクは no-op 成功
    /// （0 加算）とし、呼び出し側の再読取ループで収束させる。
    pub async fn delete_tuples_batch(
        &self,
        store_id: &str,
        model_id: &str,
        tuples: &[ReadTupleKey],
    ) -> Result<u32, AuthzError> {
        let url = format!("{}/stores/{}/write", self.base_url, store_id);
        let mut deleted: u32 = 0;
        for chunk in tuples.chunks(100) {
            let keys: Vec<Value> = chunk
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "user": t.user, "relation": t.relation, "object": t.object
                    })
                })
                .collect();
            let body = serde_json::json!({
                "deletes": { "tuple_keys": keys },
                "authorization_model_id": model_id,
            });
            let resp = self.http.post(&url).json(&body).send().await?;
            if ensure_ok_idempotent(resp, "does not exist").await? {
                deleted += chunk.len() as u32;
            }
        }
        Ok(deleted)
    }

    /// `user` が**直接タプル**を持つ `object_type` のオブジェクト識別子一覧
    /// （role reconciliation・SAAS.2）。
    ///
    /// Read API の tuple_key に user＋object type プレフィクス（例 `role:`）を渡す形。
    /// list-objects と違い継承を展開しない（直接タプルのみ）。
    pub async fn read_subject_objects(
        &self,
        store_id: &str,
        user: &str,
        object_type: &str,
    ) -> Result<Vec<String>, AuthzError> {
        let url = format!("{}/stores/{}/read", self.base_url, store_id);
        let mut out = Vec::new();
        let mut token = String::new();
        loop {
            let mut body = serde_json::json!({
                // object は「type のみ（id 無し）」形式。user 指定と組で直接タプルを引く。
                "tuple_key": { "user": user, "object": format!("{object_type}:") },
                "page_size": READ_PAGE_SIZE,
            });
            if !token.is_empty() {
                body["continuation_token"] = Value::from(token.clone());
            }
            let resp = self.http.post(&url).json(&body).send().await?;
            let resp = ensure_ok(resp).await?;
            let parsed: ReadResponse = resp.json().await?;
            out.extend(parsed.tuples.into_iter().map(|t| t.key.object));
            if parsed.continuation_token.is_empty() {
                break;
            }
            token = parsed.continuation_token;
        }
        Ok(out)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(v: Value, n: usize) -> Result<Vec<bool>, AuthzError> {
        decode_batch_check(serde_json::from_value(v).unwrap(), n)
    }

    #[test]
    fn batch_check_decodes_in_input_order() {
        let v = serde_json::json!({ "result": {
            "1": { "allowed": false },
            "0": { "allowed": true },
            "2": { "allowed": true }
        }});
        assert_eq!(decode(v, 3).unwrap(), vec![true, false, true]);
    }

    #[test]
    fn batch_check_item_error_fails_closed() {
        let v = serde_json::json!({ "result": {
            "0": { "allowed": true },
            "1": { "error": { "inputError": "validation_error", "message": "x" } }
        }});
        assert!(decode(v, 2).is_err(), "1 問の失敗で全体をエラーにする");
    }

    #[test]
    fn batch_check_missing_correlation_fails_closed() {
        let v = serde_json::json!({ "result": { "0": { "allowed": true } } });
        assert!(
            decode(v, 2).is_err(),
            "欠けた問いを拒否扱いで黙って続けない"
        );
    }

    #[test]
    fn batch_check_request_shape() {
        let body = BatchCheckRequest {
            checks: vec![BatchCheckItem {
                tuple_key: TupleKey {
                    user: "user:t|a",
                    relation: "viewer",
                    object: "data_table:t|x",
                },
                correlation_id: "0".into(),
            }],
            authorization_model_id: "m",
            consistency: HIGHER_CONSISTENCY,
        };
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(v["checks"][0]["tuple_key"]["object"], "data_table:t|x");
        assert_eq!(v["checks"][0]["correlation_id"], "0");
        assert_eq!(v["consistency"], "HIGHER_CONSISTENCY");
    }
}

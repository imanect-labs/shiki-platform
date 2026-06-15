//! 認可チョークポイント: [`AuthzClient`] トレイトと OpenFGA 実装。
//!
//! アプリ本体は具象 [`OpenFgaClient`] ではなく `dyn AuthzClient` に依存し、
//! 認可判定を単一の [`AuthzClient::check`] に帰着させる。

use async_trait::async_trait;
use serde_json::Value;

use crate::{
    error::AuthzError,
    fga_http::FgaHttp,
    model,
    object::{FgaObject, Subject},
    vocab::Relation,
};

/// 認可判定の単一エントリポイント（単一チョークポイント）。
#[async_trait]
pub trait AuthzClient: Send + Sync {
    /// `subject` が `object` に対して `relation` を持つか判定する。
    async fn check(
        &self,
        subject: &Subject,
        relation: Relation,
        object: &FgaObject,
    ) -> Result<bool, AuthzError>;
}

/// OpenFGA への接続設定。
#[derive(Debug, Clone)]
pub struct OpenFgaConfig {
    /// OpenFGA HTTP API のベース URL（例: `http://openfga:8080`）。
    pub base_url: String,
    /// store 名（起動時に自己発見 or 作成）。
    pub store_name: String,
}

/// OpenFGA を backend とする [`AuthzClient`] 実装。
pub struct OpenFgaClient {
    fga: FgaHttp,
    store_id: String,
    model_id: String,
}

impl OpenFgaClient {
    /// store と authorization model を冪等に用意して接続する。
    ///
    /// `model_json` は `crates/authz/model/authorization-model.json` の内容
    /// （human レビュー済みの正本）を呼び出し側で読み込んで渡す。
    pub async fn connect(
        http: reqwest::Client,
        config: &OpenFgaConfig,
        model_json: &Value,
    ) -> Result<Self, AuthzError> {
        let fga = FgaHttp::new(http, &config.base_url);
        let (store_id, model_id) =
            model::ensure_store_and_model(&fga, &config.store_name, model_json).await?;
        Ok(OpenFgaClient {
            fga,
            store_id,
            model_id,
        })
    }

    /// テスト等で tuple を投入する。
    pub async fn write_tuple(
        &self,
        subject: &Subject,
        relation: Relation,
        object: &FgaObject,
    ) -> Result<(), AuthzError> {
        self.fga
            .write_tuple(
                &self.store_id,
                &self.model_id,
                subject.as_str(),
                relation.as_str(),
                object.as_str(),
            )
            .await
    }

    pub fn store_id(&self) -> &str {
        &self.store_id
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }
}

#[async_trait]
impl AuthzClient for OpenFgaClient {
    async fn check(
        &self,
        subject: &Subject,
        relation: Relation,
        object: &FgaObject,
    ) -> Result<bool, AuthzError> {
        self.fga
            .check(
                &self.store_id,
                &self.model_id,
                subject.as_str(),
                relation.as_str(),
                object.as_str(),
            )
            .await
    }
}

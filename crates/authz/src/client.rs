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
///
/// 判定（`check`）に加え、ReBAC タプルの付与/剥奪（`write_tuple`/`delete_tuple`）も
/// このトレイト裏に閉じ込める。StorageService 等はファイル作成時の `owner`/`parent`
/// タプル書き込みをここ経由で行い、OpenFGA 直叩きを排する。
#[async_trait]
pub trait AuthzClient: Send + Sync {
    /// `subject` が `object` に対して `relation` を持つか判定する。
    async fn check(
        &self,
        subject: &Subject,
        relation: Relation,
        object: &FgaObject,
    ) -> Result<bool, AuthzError>;

    /// `object#relation@subject` のタプルを付与する。
    ///
    /// **冪等**: 既に存在するタプルの再付与は成功扱いとする（失敗した書込を同一操作の
    /// 再試行で安全に修復できるようにするため）。
    async fn write_tuple(
        &self,
        subject: &Subject,
        relation: Relation,
        object: &FgaObject,
    ) -> Result<(), AuthzError>;

    /// `object#relation@subject` のタプルを剥奪する（共有解除・ノード削除等で使う）。
    ///
    /// **冪等**: 存在しないタプルの剥奪は成功扱いとする。
    async fn delete_tuple(
        &self,
        subject: &Subject,
        relation: Relation,
        object: &FgaObject,
    ) -> Result<(), AuthzError>;
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

    async fn write_tuple(
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

    async fn delete_tuple(
        &self,
        subject: &Subject,
        relation: Relation,
        object: &FgaObject,
    ) -> Result<(), AuthzError> {
        self.fga
            .delete_tuple(
                &self.store_id,
                &self.model_id,
                subject.as_str(),
                relation.as_str(),
                object.as_str(),
            )
            .await
    }
}

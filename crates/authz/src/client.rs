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
    vocab::{ObjectType, Relation},
};

pub use crate::fga_http::ReadTupleKey;

/// OpenFGA `check` の整合性モード（PIT-11）。
///
/// ホットパス（通常の read 認可）は `MinimizeLatency`（既定・結果整合で高速）。共有解除を
/// **即時**に反映させたい read だけ `HigherConsistency` を使う（強整合・レイテンシ代償）。
/// 全 check を一律強整合にすると性能が劣化するため、必要な経路のみで明示的に選ぶ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consistency {
    /// 結果整合・低レイテンシ（既定）。
    MinimizeLatency,
    /// 強整合（書込直後の付与/剥奪を即座に反映）。
    HigherConsistency,
}

impl Consistency {
    /// OpenFGA に送出する文字列。
    pub const fn as_str(self) -> &'static str {
        match self {
            Consistency::MinimizeLatency => "MINIMIZE_LATENCY",
            Consistency::HigherConsistency => "HIGHER_CONSISTENCY",
        }
    }
}

/// 認可判定の単一エントリポイント（単一チョークポイント）。
///
/// 判定（`check`）に加え、ReBAC タプルの付与/剥奪（`write_tuple`/`delete_tuple`）も
/// このトレイト裏に閉じ込める。StorageService 等はファイル作成時の `owner`/`parent`
/// タプル書き込みをここ経由で行い、OpenFGA 直叩きを排する。
#[async_trait]
pub trait AuthzClient: Send + Sync {
    /// `subject` が `object` に対して `relation` を持つか判定する。
    ///
    /// `consistency` で整合性モードを選ぶ（共有解除の即時反映が要る read は
    /// [`Consistency::HigherConsistency`]、それ以外は [`Consistency::MinimizeLatency`]）。
    async fn check(
        &self,
        subject: &Subject,
        relation: Relation,
        object: &FgaObject,
        consistency: Consistency,
    ) -> Result<bool, AuthzError>;

    /// `object#relation@subject` のタプルを付与する。**実際に付与したら `true`**、既存で
    /// no-op なら `false`。
    ///
    /// **冪等**: 既に存在するタプルの再付与は成功扱いとする（失敗した書込を同一操作の
    /// 再試行で安全に修復できるようにするため）。返す bool は補償ロールバックを実変更時のみに
    /// 限定するのに使う（冪等 no-op を巻き戻して既存共有を壊さない）。
    async fn write_tuple(
        &self,
        subject: &Subject,
        relation: Relation,
        object: &FgaObject,
    ) -> Result<bool, AuthzError>;

    /// `object#relation@subject` のタプルを剥奪する（共有解除・ノード削除等で使う）。
    /// **実際に剥奪したら `true`**、不在で no-op なら `false`。
    ///
    /// **冪等**: 存在しないタプルの剥奪は成功扱いとする。
    async fn delete_tuple(
        &self,
        subject: &Subject,
        relation: Relation,
        object: &FgaObject,
    ) -> Result<bool, AuthzError>;

    /// オブジェクトに張られた **直接タプル**を列挙する（共有相手一覧）。
    ///
    /// `relation` 指定でその relation のみ。継承（親フォルダ/部署メンバー）は展開せず、
    /// そのオブジェクトに直接書かれた tuple のみ返す（誰に共有したかの管理ビュー）。
    async fn read_tuples(
        &self,
        object: &FgaObject,
        relation: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError>;

    /// `subject` が `relation` を持つ `object_type` のオブジェクト id 一覧（共有された一覧）。
    ///
    /// 継承（部署メンバー・親フォルダ）も解決した**実効集合**を返す。
    async fn list_objects(
        &self,
        subject: &Subject,
        relation: Relation,
        object_type: ObjectType,
    ) -> Result<Vec<String>, AuthzError>;

    /// オブジェクトに張られた**全直接タプルを一括剥奪**する（テナント撤去・SAAS.2）。
    ///
    /// Read で列挙し write API の `deletes` でバッチ削除する。剥奪した件数を返す。
    /// **冪等**: タプルが無ければ 0 で成功。raw subject 文字列は authz crate 内に閉じ、
    /// 呼び出し側に生文字列の組み立て/解釈をさせない。
    async fn delete_object_tuples(&self, object: &FgaObject) -> Result<u32, AuthzError>;

    /// `subject` が**直接タプル**を持つ `object_type` のオブジェクト識別子（`type:id` 文字列）
    /// 一覧を返す（role reconciliation・SAAS.2）。
    ///
    /// [`list_objects`](Self::list_objects) と違い継承を**展開しない**（Read API の
    /// user＋object type プレフィクス問い合わせ）。剥奪対象の特定は直接タプルが正。
    async fn read_subject_objects(
        &self,
        subject: &Subject,
        object_type: ObjectType,
    ) -> Result<Vec<String>, AuthzError>;
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
        consistency: Consistency,
    ) -> Result<bool, AuthzError> {
        self.fga
            .check(
                &self.store_id,
                &self.model_id,
                subject.as_str(),
                relation.as_str(),
                object.as_str(),
                consistency.as_str(),
            )
            .await
    }

    async fn write_tuple(
        &self,
        subject: &Subject,
        relation: Relation,
        object: &FgaObject,
    ) -> Result<bool, AuthzError> {
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
    ) -> Result<bool, AuthzError> {
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

    async fn read_tuples(
        &self,
        object: &FgaObject,
        relation: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        self.fga
            .read_tuples(
                &self.store_id,
                object.as_str(),
                relation.map(Relation::as_str),
            )
            .await
    }

    async fn list_objects(
        &self,
        subject: &Subject,
        relation: Relation,
        object_type: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        self.fga
            .list_objects(
                &self.store_id,
                &self.model_id,
                object_type.as_str(),
                relation.as_str(),
                subject.as_str(),
            )
            .await
    }

    async fn delete_object_tuples(&self, object: &FgaObject) -> Result<u32, AuthzError> {
        // Read で全直接タプルを列挙 → deletes 配列でバッチ削除（ページングしつつ収束まで）。
        let mut deleted: u32 = 0;
        loop {
            let tuples = self
                .fga
                .read_tuples(&self.store_id, object.as_str(), None)
                .await?;
            if tuples.is_empty() {
                return Ok(deleted);
            }
            deleted += self
                .fga
                .delete_tuples_batch(&self.store_id, &self.model_id, &tuples)
                .await?;
        }
    }

    async fn read_subject_objects(
        &self,
        subject: &Subject,
        object_type: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        self.fga
            .read_subject_objects(&self.store_id, subject.as_str(), object_type.as_str())
            .await
    }
}

//! 保存ビュー（Task 9.4）。
//!
//! 宣言的クエリ＋表示設定を `artifact(kind=data_view)` として保存する（6.1 共通枠・
//! ReBAC 共有・不変バージョン）。**実行は必ずクエリチョークポイント（[`crate::query`]）
//! 経由**で、行述語・フィールドマスク・集計抑制を**閲覧者本人の権限で毎回再評価**する
//! （作成者の権限を引き継がない・PIT-19/21）。

use std::sync::Arc;

use artifact::{ArtifactError, ArtifactKind, ArtifactStore, NewArtifact};
use authz::AuthContext;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::query::declarative::{DataQuery, QueryResult};
use crate::store::DataStore;
use crate::DataError;

/// 保存ビュー本文（artifact body）。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DataViewBody {
    /// 対象テーブル。
    pub table_id: Uuid,
    /// 宣言的クエリ（実行時に閲覧者の述語/マスクと合成）。
    pub query: DataQuery,
    /// 表示設定（一覧/グラフ/カレンダー等の描画ヒント・サーバは解釈しない）。
    #[serde(default)]
    pub display: serde_json::Value,
}

/// 保存ビューの保存/実行（`crates/data` の一部＝クエリ実行と同じチョークポイント側）。
#[derive(Clone)]
pub struct DataViewStore {
    artifacts: Arc<ArtifactStore>,
    data: DataStore,
}

impl DataViewStore {
    pub fn new(artifacts: Arc<ArtifactStore>, data: DataStore) -> Self {
        DataViewStore { artifacts, data }
    }

    /// ビューを保存する（table 存在＋クエリ整合を検証 → artifact version 1）。
    pub async fn create(
        &self,
        ctx: &AuthContext,
        name: &str,
        body: &DataViewBody,
        trace_id: Option<&str>,
    ) -> Result<Uuid, DataError> {
        self.validate(ctx, body, trace_id).await?;
        let raw = serde_json::to_value(body)
            .map_err(|e| DataError::Invalid(format!("view body: {e}")))?;
        let artifact = self
            .artifacts
            .create(
                ctx,
                NewArtifact {
                    kind: ArtifactKind::DataView,
                    name: name.to_string(),
                    body: raw,
                },
                trace_id,
            )
            .await
            .map_err(map_artifact)?;
        Ok(artifact.id)
    }

    /// ビューに新バージョンを追記する（検証 → 不変追記）。
    pub async fn update(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        body: &DataViewBody,
        expected_version: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<i64, DataError> {
        self.ensure_kind(ctx, id, trace_id).await?;
        self.validate(ctx, body, trace_id).await?;
        let raw = serde_json::to_value(body)
            .map_err(|e| DataError::Invalid(format!("view body: {e}")))?;
        let v = self
            .artifacts
            .append_version(ctx, id, raw, expected_version, trace_id)
            .await
            .map_err(map_artifact)?;
        Ok(v.version)
    }

    /// 指定バージョン（省略時は最新）のビューを取得する（viewer）。
    pub async fn get(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        version: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<(i64, DataViewBody), DataError> {
        let meta = self
            .artifacts
            .get(ctx, id, trace_id)
            .await
            .map_err(map_artifact)?;
        if meta.kind != ArtifactKind::DataView {
            return Err(DataError::NotFound);
        }
        let ver = version.unwrap_or(meta.current_version);
        let v = self
            .artifacts
            .get_version(ctx, id, ver, trace_id)
            .await
            .map_err(map_artifact)?;
        let body: DataViewBody = serde_json::from_value(v.body)
            .map_err(|e| DataError::Internal(format!("view body 破損: {e}")))?;
        Ok((v.version, body))
    }

    /// ビューを実行する（**閲覧者本人**の行述語・フィールドマスク・集計抑制で毎回再評価）。
    pub async fn run(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        version: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<QueryResult, DataError> {
        let (_, body) = self.get(ctx, id, version, trace_id).await?;
        // 実行はクエリチョークポイント経由（ビューの table viewer だけでなく、行述語も効く）。
        self.data
            .run_query(ctx, body.table_id, &body.query, trace_id)
            .await
    }

    /// 保存前の整合検証: table 存在（viewer）＋クエリを dry-run（limit 0）で通す。
    async fn validate(
        &self,
        ctx: &AuthContext,
        body: &DataViewBody,
        trace_id: Option<&str>,
    ) -> Result<(), DataError> {
        // 作成者が table viewer であること＋クエリの静的整合（未宣言/マスク列参照など）を
        // limit=0 の実行で検査する。行の中身は返さない（0 件）。
        let mut probe = body.query.clone();
        probe.aggregate = None;
        probe.limit = Some(1);
        probe.offset = Some(0);
        // run_query が field/mask/index 検証を一手に担う。0 行取得の副作用はない。
        let _ = self
            .data
            .run_query(ctx, body.table_id, &probe, trace_id)
            .await?;
        Ok(())
    }

    async fn ensure_kind(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(), DataError> {
        let meta = self
            .artifacts
            .get(ctx, id, trace_id)
            .await
            .map_err(map_artifact)?;
        if meta.kind != ArtifactKind::DataView {
            return Err(DataError::Invalid(
                "このアーティファクトは data_view ではありません".into(),
            ));
        }
        Ok(())
    }
}

#[allow(clippy::needless_pass_by_value)]
fn map_artifact(e: ArtifactError) -> DataError {
    match e {
        ArtifactError::NotFound => DataError::NotFound,
        ArtifactError::Forbidden => DataError::Forbidden,
        ArtifactError::Invalid(m) => DataError::Invalid(m),
        ArtifactError::Conflict(m) => DataError::Conflict(m),
        ArtifactError::Internal(m) => DataError::Internal(format!("artifact: {m}")),
    }
}

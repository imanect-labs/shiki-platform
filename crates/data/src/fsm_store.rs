//! FSM 定義の保存/解決（Task 9.10）。
//!
//! FSM を `artifact(kind=fsm)` として保存する（6.1 枠・ReBAC 共有・不変バージョン）。
//! 保存時に対象テーブルのスキーマ（status_field の options＝states）と照合して検証する。
//! 遷移実行は [`DataStore::transition_record`] へ委譲する（本ストアは定義の解決のみ）。

use std::sync::Arc;

use artifact::{ArtifactError, ArtifactKind, ArtifactStore, NewArtifact};
use authz::AuthContext;
use uuid::Uuid;

use crate::fsm::{validate_fsm, FsmBody};
use crate::store::DataStore;
use crate::DataError;

/// FSM 定義の保存/解決（`crates/data` の一部・遷移実行と同じチョークポイント側）。
#[derive(Clone)]
pub struct FsmStore {
    artifacts: Arc<ArtifactStore>,
    data: DataStore,
}

impl FsmStore {
    pub fn new(artifacts: Arc<ArtifactStore>, data: DataStore) -> Self {
        FsmStore { artifacts, data }
    }

    /// FSM を保存する（対象テーブルのスキーマと照合検証 → artifact version 1）。
    pub async fn create(
        &self,
        ctx: &AuthContext,
        name: &str,
        table_id: Uuid,
        body: &FsmBody,
        trace_id: Option<&str>,
    ) -> Result<Uuid, DataError> {
        self.validate_against_table(ctx, table_id, body, trace_id)
            .await?;
        let raw =
            serde_json::to_value(body).map_err(|e| DataError::Invalid(format!("fsm body: {e}")))?;
        let a = self
            .artifacts
            .create(
                ctx,
                NewArtifact {
                    kind: ArtifactKind::Fsm,
                    name: name.to_string(),
                    body: raw,
                },
                trace_id,
            )
            .await
            .map_err(map_artifact)?;
        Ok(a.id)
    }

    /// FSM に新バージョンを追記する（検証 → 不変追記）。
    pub async fn update(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        table_id: Uuid,
        body: &FsmBody,
        expected_version: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<i64, DataError> {
        self.ensure_kind(ctx, id, trace_id).await?;
        self.validate_against_table(ctx, table_id, body, trace_id)
            .await?;
        let raw =
            serde_json::to_value(body).map_err(|e| DataError::Invalid(format!("fsm body: {e}")))?;
        let v = self
            .artifacts
            .append_version(ctx, id, raw, expected_version, trace_id)
            .await
            .map_err(map_artifact)?;
        Ok(v.version)
    }

    /// 指定バージョン（省略時最新）の FSM body を取得する（viewer）。
    pub async fn get(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        version: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<(i64, FsmBody), DataError> {
        let meta = self
            .artifacts
            .get(ctx, id, trace_id)
            .await
            .map_err(map_artifact)?;
        if meta.kind != ArtifactKind::Fsm {
            return Err(DataError::NotFound);
        }
        let ver = version.unwrap_or(meta.current_version);
        let v = self
            .artifacts
            .get_version(ctx, id, ver, trace_id)
            .await
            .map_err(map_artifact)?;
        let body: FsmBody = serde_json::from_value(v.body)
            .map_err(|e| DataError::Internal(format!("fsm body 破損: {e}")))?;
        Ok((v.version, body))
    }

    async fn validate_against_table(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        body: &FsmBody,
        trace_id: Option<&str>,
    ) -> Result<(), DataError> {
        // テーブル viewer＋生存を確認し、そのスキーマで FSM を検証する。
        let table = self.data.get_table(ctx, table_id, trace_id).await?;
        validate_fsm(body, &table.schema)
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
        if meta.kind != ArtifactKind::Fsm {
            return Err(DataError::Invalid(
                "このアーティファクトは fsm ではありません".into(),
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

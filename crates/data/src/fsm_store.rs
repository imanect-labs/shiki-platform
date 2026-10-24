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

    /// テーブルのレコードを状態遷移する（HTTP・ワークフロー・スクリプト・ゲートウェイの**単一の入口**）。
    ///
    /// テーブルにピンされた FSM（`fsm_ref` の版）は**テーブル側の参照として**読む: 呼出主体に FSM の
    /// アーティファクト自体の閲覧権限は要求せず、テーブルの editor であることを確かめてから読む
    /// （FSM の作成者でないテーブル editor が遷移できなかった・検証 C-P1-1）。行の可視性・actor 述語・
    /// ガードは [`DataStore::transition_record`] が当該行で評価する。
    #[allow(clippy::too_many_arguments)] // 主体・テーブル・行・遷移先・rev・trace は全て別物。
    pub async fn transition(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        record_id: Uuid,
        to: &str,
        expected_rev: i64,
        trace_id: Option<&str>,
    ) -> Result<crate::model::DataRecord, DataError> {
        let table = self.data.get_table(ctx, table_id, trace_id).await?;
        let not_managed = || {
            DataError::input_detail(
                crate::InvalidCode::FsmInvalid,
                None,
                "このテーブルは状態遷移（FSM）で管理されていません",
            )
        };
        let fsm_ref = table.schema.fsm_ref.clone().ok_or_else(not_managed)?;
        let status_field = table.schema.status_field.clone().ok_or_else(not_managed)?;
        // FSM の本文を読む前にテーブルの editor を確かめる（閲覧者に定義を読ませない）。
        self.data
            .authorize_table(
                ctx,
                table_id,
                authz::Relation::Editor,
                "data.record.transition",
                trace_id,
            )
            .await?;
        let v = self
            .artifacts
            .get_version_pinned(
                ctx,
                fsm_ref.artifact_id,
                fsm_ref.version,
                ArtifactKind::Fsm,
                &format!("data_table:{table_id}"),
                trace_id,
            )
            .await
            .map_err(map_artifact)?;
        let fsm: FsmBody = serde_json::from_value(v.body)
            .map_err(|e| DataError::Internal(format!("fsm body 破損: {e}")))?;
        self.data
            .transition_record(
                ctx,
                table_id,
                record_id,
                to,
                expected_rev,
                &fsm,
                &status_field,
                trace_id,
            )
            .await
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

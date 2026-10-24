//! FSM 管理テーブルの作成時の状態と、いま実行できる遷移（検証 2-C-P2-3・2-C-P2-4）。
//!
//! - **作成時の状態は FSM の初期状態（`states` の先頭）に固定する**。省略すれば初期状態を入れ、
//!   違う状態を指定したら 400 `fsm_invalid`（1 件の作成・一括取込の作成行・チャット・ワークフロー・
//!   スクリプト・ゲートウェイの全経路が [`DataStore::create_record`] / 取込の検証段を通る）。
//!   取込の更新行（upsert）は 1 件の更新と同じ入口で状態の直接変更を拒否する。
//! - **遷移の候補**: グリッドの状態セルが「いまの状態から閲覧者が実行できる遷移先」だけをメニューに出す
//!   ための読取（[`crate::FsmStore::allowed_transitions`]）。判定は遷移 API と同じ（行の可視性・遷移の定義・
//!   actor 述語を当該行で評価）で、表示の出し分けのためだけに使う（実行は遷移 API が改めて認可する）。
//!
//! FSM の本文はテーブルにピンされた版を**テーブル側の参照として**読む（[`crate::FsmStore`] と同じ）。
//! 読み口は [`crate::FsmStore::new`] がストアの複製間で共有する枠に登録する。未登録（FSM の保存先が無い
//! 構成）で FSM 管理テーブルに作成しようとしたら失敗させる（初期状態の検査を素通りさせない）。

use authz::{AuthContext, Relation};
use serde::Serialize;
use serde_json::{Map, Value};
use storage::audit::AuditAction;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::field_key::decode_record;
use crate::fsm::FsmBody;
use crate::model::DataTable;
use crate::store::DataStore;
use crate::{DataError, InvalidCode};

/// レコードのいまの状態と、閲覧者が実行できる遷移先。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct AllowedTransitions {
    /// 状態の列の参照名。
    pub status_field: String,
    /// いまの状態（未設定なら null）。
    pub current: Option<String>,
    /// いまの rev（遷移 API の `expected_rev` に渡す）。
    pub rev: i64,
    /// 閲覧者がいま実行できる遷移先（FSM の定義順・重複なし）。空なら遷移できない。
    pub transitions: Vec<String>,
}

/// FSM 管理テーブルの状態の列と初期状態。
pub(crate) struct FsmInitial {
    pub status_field: String,
    pub initial: String,
}

impl FsmInitial {
    /// 作成の値に初期状態を当てる（省略は初期状態を入れる・違う状態は 400）。
    pub(crate) fn apply(&self, data: &mut Map<String, Value>) -> Result<(), DataError> {
        match data.get(&self.status_field) {
            None | Some(Value::Null) => {
                data.insert(
                    self.status_field.clone(),
                    Value::String(self.initial.clone()),
                );
                Ok(())
            }
            Some(Value::String(s)) if *s == self.initial => Ok(()),
            Some(_) => Err(DataError::input_detail(
                InvalidCode::FsmInvalid,
                Some(&self.status_field),
                format!(
                    "作成時の状態は「{}」だけです（状態は作成後に遷移で変えてください）",
                    self.initial
                ),
            )),
        }
    }
}

impl DataStore {
    /// テーブルにピンされた FSM の本文（テーブル側の参照として読む・FSM が無ければ `None`）。
    pub(crate) async fn pinned_fsm(
        &self,
        ctx: &AuthContext,
        table: &DataTable,
        trace_id: Option<&str>,
    ) -> Result<Option<(FsmBody, String)>, DataError> {
        let (Some(fsm_ref), Some(status)) = (&table.schema.fsm_ref, &table.schema.status_field)
        else {
            return Ok(None);
        };
        let artifacts = self.fsm_artifacts.get().ok_or_else(|| {
            DataError::Internal("FSM の定義を読む口が登録されていません（FsmStore 未構成）".into())
        })?;
        let v = artifacts
            .get_version_pinned(
                ctx,
                fsm_ref.artifact_id,
                fsm_ref.version,
                artifact::ArtifactKind::Fsm,
                &format!("data_table:{}", table.id),
                trace_id,
            )
            .await
            .map_err(crate::fsm_store::map_artifact)?;
        let body: FsmBody = serde_json::from_value(v.body)
            .map_err(|e| DataError::Internal(format!("fsm body 破損: {e}")))?;
        Ok(Some((body, status.clone())))
    }

    /// FSM 管理テーブルの初期状態（FSM の `states` の先頭・FSM が無ければ `None`）。
    pub(crate) async fn fsm_initial(
        &self,
        ctx: &AuthContext,
        table: &DataTable,
        trace_id: Option<&str>,
    ) -> Result<Option<FsmInitial>, DataError> {
        let Some((body, status_field)) = self.pinned_fsm(ctx, table, trace_id).await? else {
            return Ok(None);
        };
        let initial = body
            .states
            .first()
            .cloned()
            .ok_or_else(|| DataError::Internal("fsm: states が空です".into()))?;
        Ok(Some(FsmInitial {
            status_field,
            initial,
        }))
    }

    /// レコードのいまの状態と、閲覧者が実行できる遷移先（editor・不可視行は 404・状態の列が読めなければ
    /// 存在しない列と同じ 403）。
    pub(crate) async fn allowed_transitions_with(
        &self,
        ctx: &AuthContext,
        table: &DataTable,
        record_id: Uuid,
        fsm: &FsmBody,
        status_field: &str,
    ) -> Result<AllowedTransitions, DataError> {
        let az = self.authz_memo();
        if self
            .masked_fields(ctx, &az, table)
            .await?
            .contains(status_field)
        {
            return Err(DataError::FieldUnavailable);
        }
        let row = self
            .select_visible_by_id(ctx, &az, table, record_id)
            .await?
            .ok_or(DataError::NotFound)?;
        let data = decode_record(&table.schema, &row.data.0);
        let current = data
            .get(status_field)
            .and_then(Value::as_str)
            .map(str::to_string);
        let mut transitions: Vec<String> = Vec::new();
        if let Some(from) = &current {
            for t in fsm.transitions.iter().filter(|t| &t.from == from) {
                if transitions.contains(&t.to) {
                    continue;
                }
                if self
                    .actor_allows(ctx, &az, table, record_id, &t.actor)
                    .await?
                {
                    transitions.push(t.to.clone());
                }
            }
        }
        Ok(AllowedTransitions {
            status_field: status_field.to_string(),
            current,
            rev: row.rev,
            transitions,
        })
    }

    /// テーブルの editor を確かめて生きているテーブルを返す（遷移の候補の入口）。
    pub(crate) async fn table_for_transition(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<DataTable, DataError> {
        self.require(
            ctx,
            table_id,
            Relation::Editor,
            AuditAction::DataRecordTransition,
            trace_id,
        )
        .await?;
        self.fetch_live(ctx, table_id).await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn create_status_is_pinned_to_the_initial_state() {
        let fi = FsmInitial {
            status_field: "status".into(),
            initial: "draft".into(),
        };
        let mut m = Map::new();
        fi.apply(&mut m).unwrap();
        assert_eq!(m["status"], json!("draft"));
        let mut m: Map<String, Value> = serde_json::from_value(json!({"status": null})).unwrap();
        fi.apply(&mut m).unwrap();
        assert_eq!(m["status"], json!("draft"));
        let mut m: Map<String, Value> = serde_json::from_value(json!({"status": "draft"})).unwrap();
        assert!(fi.apply(&mut m).is_ok());
        let mut m: Map<String, Value> =
            serde_json::from_value(json!({"status": "approved"})).unwrap();
        let e = fi.apply(&mut m).unwrap_err();
        assert_eq!(e.invalid_code(), Some(InvalidCode::FsmInvalid));
    }
}

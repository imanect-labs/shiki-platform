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

use authz::AuthContext;
use serde::Serialize;
use serde_json::{Map, Value};
use storage::audit::AuditAction;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::creator_grant::GrantNeed;
use crate::field_key::decode_record;
use crate::fsm::FsmBody;
use crate::model::DataTable;
use crate::store::DataStore;
use crate::table_access::WriteAccess;
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

/// ワークフローの承認（human.approval）が停止時に読む行の状態（[`crate::FsmStore::approval_target`]・GAP-08）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalTargetView {
    /// 状態の列の参照名。
    pub status_field: String,
    /// いまの状態（未設定なら None）。
    pub current: Option<String>,
    /// いまの rev。
    pub rev: i64,
    /// FSM でいまの状態から進める状態（遷移の定義だけ・actor 述語は評価しない・定義順・重複なし）。
    pub targets: Vec<String>,
    /// ユーザー・ロールの列のうち主体に読めるもの（参照名 → (ロールの列か, 値)）。マスクされた列は入らない。
    pub refs: std::collections::BTreeMap<String, (bool, Option<String>)>,
}

/// FSM 管理テーブルの状態の列と初期状態。
///
/// 作成の検査（[`Self::apply`]）は、1 件の作成とワークフローの保存時の照合（`data.record.create`・
/// `data.record.upsert` の値に書いた状態・BUG-251）が同じこの関数で行う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsmInitial {
    /// 状態の列の参照名。
    pub status_field: String,
    /// 作成時の状態（FSM の `states` の先頭）。
    pub initial: String,
}

/// FSM 管理テーブルの状態の列への**直接の書き込み**（更新の値に状態の列を書く）を断る（遷移 API を迂回させない）。
///
/// 1 件の更新・取込の更新行（[`DataStore::reject_fsm_status_write`]）と、ワークフローの保存時の照合
/// （`data.record.update` の変更する値と足す数・`data.record.upsert` の書き換えの値・BUG-251）が同じこの関数で決める。
/// 読む人の見え方の定義（状態の列が読めなければ `status_field` が無い・[`DataStore::get_table_for_viewer`]）を
/// 渡すと、読めない状態の列は状態の列として扱わない。
pub fn reject_status_write<'a>(
    schema: &crate::model::TableSchema,
    fields: impl Iterator<Item = &'a String>,
) -> Result<(), DataError> {
    let (Some(_), Some(status)) = (&schema.fsm_ref, &schema.status_field) else {
        return Ok(());
    };
    for f in fields {
        if f == status {
            // 利用者向けの文言（列は書き手が名指ししたもの・表示名で示す・検証 2-C-P2-4）。
            let label = schema
                .field(status)
                .map(|d| d.display_name.as_str())
                .filter(|n| !n.trim().is_empty())
                .unwrap_or(status);
            return Err(DataError::input_detail(
                InvalidCode::FsmInvalid,
                Some(status),
                format!(
                    "「{label}」は状態の遷移でだけ変えられます（遷移できる先から選んでください）"
                ),
            ));
        }
    }
    Ok(())
}

impl FsmInitial {
    /// 作成の値に初期状態を当てる（省略は初期状態を入れる・違う状態は 400）。
    pub fn apply(&self, data: &mut Map<String, Value>) -> Result<(), DataError> {
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
    ///
    /// FSM の本文はテーブルにピンされた版をテーブル側の参照として読む（読む人の権限は `table` を開けたことで確かめて
    /// あること）。読む人の見え方の定義（状態の列が読めなければ `status_field` が無い）を渡すと `None`。
    pub async fn fsm_initial(
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

    /// レコードのいまの状態と、閲覧者が実行できる遷移先（editor か作成者として許した追加だけの人・不可視行は
    /// 404・状態の列が読めなければ存在しない列と同じ 403）。
    ///
    /// 追加だけの人（[`WriteAccess::Creator`]）は自分の行に絞って引き、進められる人のうち作成者として許した部分で
    /// 判定する（遷移と同じ・D-127）。
    #[allow(clippy::too_many_arguments)] // 主体・テーブル・第 1 層・行・定義・状態の列は全て別物。
    pub(crate) async fn allowed_transitions_with(
        &self,
        ctx: &AuthContext,
        table: &DataTable,
        access: &WriteAccess,
        record_id: Uuid,
        fsm: &FsmBody,
        status_field: &str,
    ) -> Result<AllowedTransitions, DataError> {
        let az = self.authz_memo();
        let view = access.view(table);
        if self
            .masked_fields(ctx, &az, &view)
            .await?
            .contains(status_field)
        {
            return Err(DataError::FieldUnavailable);
        }
        let row = self
            .select_visible_by_id(ctx, &az, &view, record_id)
            .await?
            .ok_or(DataError::NotFound)?;
        let data = decode_record(&table.schema, &row.data.0);
        let current = data
            .get(status_field)
            .and_then(Value::as_str)
            .map(str::to_string);
        let mut transitions: Vec<String> = Vec::new();
        if let Some(from) = &current {
            let mut tried: Vec<&str> = Vec::new();
            for t in fsm.transitions.iter().filter(|t| &t.from == from) {
                if tried.contains(&t.to.as_str()) {
                    continue;
                }
                tried.push(&t.to);
                let ok = match access {
                    WriteAccess::Editor => {
                        self.may_transition(ctx, &az, table, record_id, fsm, from, &t.to)
                            .await?
                    }
                    WriteAccess::Creator(g) => {
                        self.creator_may_transition(ctx, &az, table, record_id, g, from, &t.to)
                            .await?
                    }
                };
                if ok {
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

    /// 承認の停止時に読む行の状態（テーブルの viewer・行述語・フィールドマスク込み・不可視行は 404・状態の列が
    /// 読めなければ 403 `FieldUnavailable`）。
    ///
    /// 進める状態は FSM の遷移の定義だけを見る（actor 述語は評価しない）。誰が判断できるかは判断の遷移
    /// （[`crate::FsmStore::transition`]）が承認者本人で改めて決めるので、ここは「判断の行き先が定義に
    /// あるか」を確かめる材料に限る。
    pub(crate) async fn approval_target_with(
        &self,
        ctx: &AuthContext,
        table: &DataTable,
        record_id: Uuid,
        fsm: &FsmBody,
        status_field: &str,
    ) -> Result<ApprovalTargetView, DataError> {
        use crate::model::FieldType;
        let az = self.authz_memo();
        let masked = self.masked_fields(ctx, &az, table).await?;
        if masked.contains(status_field) {
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
        let mut targets: Vec<String> = Vec::new();
        if let Some(from) = &current {
            for t in fsm.transitions.iter().filter(|t| &t.from == from) {
                if !targets.contains(&t.to) {
                    targets.push(t.to.clone());
                }
            }
        }
        let refs = table
            .schema
            .fields
            .iter()
            .filter(|f| matches!(f.field_type, FieldType::UserRef | FieldType::RoleRef))
            .filter(|f| !masked.contains(&f.key))
            .map(|f| {
                let v = data.get(&f.key).and_then(Value::as_str).map(str::to_string);
                (f.key.clone(), (f.field_type == FieldType::RoleRef, v))
            })
            .collect();
        Ok(ApprovalTargetView {
            status_field: status_field.to_string(),
            current,
            rev: row.rev,
            targets,
            refs,
        })
    }

    /// 主体がレコードを `from` から `to` へ進められるか（遷移と遷移の候補の**同じ判定**・BUG-153）。
    ///
    /// 同じ組の進め方が複数ある定義（前から保存されていたもの）では、どれか 1 つの進められる人に当てはまれば
    /// 進められる。組が無ければ偽。進められる人は当該行で評価する（[`Self::actor_allows`]）。
    #[allow(clippy::too_many_arguments)] // 主体・メモ・テーブル・行・定義・組は全て別物。
    pub(crate) async fn may_transition(
        &self,
        ctx: &AuthContext,
        az: &crate::policy::memo::AuthzMemo<'_>,
        table: &DataTable,
        record_id: Uuid,
        fsm: &FsmBody,
        from: &str,
        to: &str,
    ) -> Result<bool, DataError> {
        for t in fsm.transitions_between(from, to) {
            if self
                .actor_allows(ctx, az, table, record_id, &t.actor)
                .await?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// 状態を進める・遷移の候補の入口: テーブルの editor か、追加だけで作成者として進め方を許したテーブル
    /// （D-127）を確かめて、生きているテーブルを返す。それ以外は、レコードを引く前に表の権限で断る（BUG-155）。
    pub(crate) async fn table_for_transition(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(DataTable, WriteAccess), DataError> {
        self.write_access(
            &self.authz_memo(),
            ctx,
            table_id,
            AuditAction::DataRecordTransition,
            GrantNeed::Move,
            trace_id,
        )
        .await
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

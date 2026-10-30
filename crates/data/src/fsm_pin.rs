//! テーブルが指す状態の進め方（`fsm_ref`）と、状態の列の整合（GAP-44）。
//!
//! テーブルの作成とスキーマの改訂で、次のどちらかが変わるときに確かめる。
//!
//! - **指す定義が変わる**（`fsm_ref` を新しく付ける・別の定義か別の版に替える）: 指す人がその定義を読めること
//!   （artifact の viewer）。テーブルの定義は持ち主へ参照として読ませる（[`crate::FsmStore`] の
//!   `table_fsm`）ので、読めない定義を自分のテーブルに付けて中身（進められる人の条件）を読む経路を作らない。
//!   `fsm_ref` はテーブルの閲覧者に見えるので、ほかのテーブルの定義の id は分かりうる。
//! - **指す定義か状態の列の選択肢が変わる**: 指す版の状態と、状態の列の選択肢が同じ集合であること
//!   （[`crate::fsm::validate_fsm`]）。選択肢だけを変えると、状態の進め方に無い状態へレコードが進み、
//!   選択肢に無い値が状態の列に入る。選択肢は「状態の進め方」の保存（[`crate::FsmStore`] の
//!   `save_for_table`）で定義と一緒に変える。
//!
//! どちらも変わらない改訂（ほかの列・表示名・索引・並び）は確かめない（前からある食い違いで、関係のない
//! 改訂を止めない）。

use std::collections::BTreeSet;

use authz::AuthContext;

use crate::fsm::FsmBody;
use crate::fsm_store::map_artifact;
use crate::model::TableSchema;
use crate::store::DataStore;
use crate::{DataError, InvalidCode};

/// 状態の列（参照名と選択肢の集合）。
fn status_of(schema: &TableSchema) -> Option<(&str, BTreeSet<&str>)> {
    let key = schema.status_field.as_deref()?;
    let opts = schema
        .field(key)
        .map(|f| f.options.iter().map(String::as_str).collect())
        .unwrap_or_default();
    Some((key, opts))
}

impl DataStore {
    /// `prev`（作成なら `None`）から `next` への変更で、指す状態の進め方と状態の列が合うかを確かめる。
    pub(crate) async fn check_fsm_pin(
        &self,
        ctx: &AuthContext,
        table_id: Option<uuid::Uuid>,
        prev: Option<&TableSchema>,
        next: &TableSchema,
        trace_id: Option<&str>,
    ) -> Result<(), DataError> {
        let Some(r) = &next.fsm_ref else {
            return Ok(());
        };
        let prev_ref = prev.and_then(|p| p.fsm_ref.as_ref());
        let ref_changed = prev_ref != Some(r);
        let status_changed = prev.is_none_or(|p| status_of(p) != status_of(next));
        if !ref_changed && !status_changed {
            return Ok(());
        }
        let invalid = |m: &str| DataError::input_detail(InvalidCode::FsmInvalid, None, m);
        let artifacts = self.fsm_artifacts.get().ok_or_else(|| {
            DataError::Internal("FSM の定義を読む口が登録されていません（FsmStore 未構成）".into())
        })?;
        const MISSING: &str =
            "指定した状態の進め方が見つからないか、見る権限がありません。「状態の進め方」で作り直してください。";
        if ref_changed {
            // 見えない・無い・FSM でないは同じ文言（存在を明かさない）。同じ定義の版だけを替えるときも確かめる
            // （定義の共有を外された人が、新しい版を指してテーブル側の参照として中身を読まないように）。
            match artifacts.get(ctx, r.artifact_id, trace_id).await {
                Ok(meta) if meta.kind == artifact::ArtifactKind::Fsm => {}
                Ok(_)
                | Err(artifact::ArtifactError::NotFound | artifact::ArtifactError::Forbidden) => {
                    return Err(invalid(MISSING));
                }
                Err(e) => return Err(map_artifact(e)),
            }
        }
        let holder = table_id.map_or_else(
            || "data_table:new".to_string(),
            |id| format!("data_table:{id}"),
        );
        let v = match artifacts
            .get_version_pinned(
                ctx,
                r.artifact_id,
                r.version,
                artifact::ArtifactKind::Fsm,
                &holder,
                trace_id,
            )
            .await
        {
            Ok(v) => v,
            Err(artifact::ArtifactError::NotFound) => return Err(invalid(MISSING)),
            Err(e) => return Err(map_artifact(e)),
        };
        let body: FsmBody = serde_json::from_value(v.body)
            .map_err(|e| DataError::Internal(format!("fsm body 破損: {e}")))?;
        if !ref_changed {
            // 選択肢だけを変えた（状態の進め方はそのまま）: 定義と合わなくなるなら、画面の呼び名で理由を言う。
            if let Some((key, opts)) = status_of(next) {
                let states: BTreeSet<&str> = body.states.iter().map(String::as_str).collect();
                if opts != states || prev.and_then(|p| p.status_field.as_deref()) != Some(key) {
                    let label = next.field(key).map_or(key, crate::model::FieldDef::label);
                    return Err(DataError::input_detail(
                        InvalidCode::FsmInvalid,
                        Some(key),
                        format!(
                            "「{label}」は状態の進め方で使っている列です。選択肢は「状態の進め方」で変えてください。"
                        ),
                    ));
                }
            }
        }
        crate::fsm::validate_fsm(&body, next)
    }
}

//! 束ねた部品（ワークフロー・保存ビュー）の新しいバージョンと、上げると変わるところ（GAP-11）。
//!
//! アプリは部品を版で固定して束ねる（再現性）。部品が改訂されても、アプリの画面とボタンは固定した版のまま動く。
//! 作る人がアプリのエディターで固定した版を最新に上げるときに、上げると何が変わるかを見せる（公開の前の差・
//! [`crate::miniapp_publish::diff_bodies`] と同じく、差の種類はここで決め、画面は種類ごとに言うだけ）。
//!
//! - 呼んだ人（エディターを開いた人）の権限で読む。読めない部品（削除された・見る権限がない・種類の違うもの）は
//!   `readable: false` で、最新の版も差も返さない（存在を区別しない・BUG-80 と同じ扱い）。
//! - ワークフローの差は入力項目（画面の部品と突き合わせる手がかり・[`crate::miniapp_inputs::input_hints`]）の変化。
//!   処理の中身の差は言わない（ワークフローのエディターで見る）。
//! - 保存ビューの差は、対象のテーブル・絞り込みと並べ替え（クエリ）・表示の設定のどれが変わるか。列の名前や条件の
//!   値は返さない。

use artifact::{ArtifactError, ArtifactKind};
use authz::AuthContext;
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::miniapp::ComponentPin;
use crate::miniapp_inputs::{input_hints, InputHintType, WorkflowInputHint};
use crate::miniapp_store::MiniAppStore;
use crate::store::GuiError;

/// 部品を最新のバージョンに上げると変わるところ 1 つ（`kind` で判別）。
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PinDiff {
    /// 入力項目が増える（ワークフロー）。`label` は入力項目の表示名（決めていなければ `null`）。
    InputAdded {
        key: String,
        label: Option<String>,
        required: bool,
    },
    /// 入力項目がなくなる（ワークフロー）。
    InputRemoved { key: String, label: Option<String> },
    /// 入力項目が必須になる（`required: true`）・必須でなくなる（ワークフロー）。
    InputRequired {
        key: String,
        label: Option<String>,
        required: bool,
    },
    /// 入力項目が受ける値の種類が変わる（ワークフロー）。
    InputType {
        key: String,
        label: Option<String>,
        from: InputHintType,
        to: InputHintType,
    },
    /// 入力項目の選択肢が変わる（ワークフロー）。
    InputOptions { key: String, label: Option<String> },
    /// 対象のテーブルが変わる（保存ビュー）。
    ViewTable,
    /// 絞り込み・並べ替え・取り出す列が変わる（保存ビュー）。
    ViewQuery,
    /// 列の並び・見出し・書式などの表示の設定が変わる（保存ビュー）。
    ViewDisplay,
}

/// 束ねた部品 1 つの、新しいバージョンの有無と差（ピンの順）。
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct PinUpdate {
    pub artifact_id: Uuid,
    /// 固定している版。
    pub version: i64,
    /// 呼んだ人がその部品を読めたか（削除された・見る権限がない・種類の違うものは false）。
    pub readable: bool,
    /// 呼んだ人が読める最新の版（読めないときは `null`）。
    pub latest_version: Option<i64>,
    /// 最新の版に上げると変わるところ（最新の版が固定している版より新しいときだけ）。
    pub changes: Vec<PinDiff>,
    /// 新しい版はあるが、変わるところを調べられなかった（固定している版の本文が無いなど）。`changes` が空でも
    /// 「変わらない」とは言わない。
    pub diff_unknown: bool,
}

impl MiniAppStore {
    /// 束ねる部品（`kind` はワークフローか保存ビュー）ごとに、呼んだ人が読める最新の版と、上げると変わるところを返す
    /// （ピンの順・GAP-11）。読めない部品は `readable: false`。ほかの誤りは返す（読めないと決めつけない）。
    pub async fn pin_updates(
        &self,
        ctx: &AuthContext,
        kind: ArtifactKind,
        pins: &[ComponentPin],
        trace_id: Option<&str>,
    ) -> Result<Vec<PinUpdate>, GuiError> {
        let mut out = Vec::with_capacity(pins.len());
        for pin in pins {
            out.push(self.pin_update(ctx, kind, pin, trace_id).await?);
        }
        Ok(out)
    }

    async fn pin_update(
        &self,
        ctx: &AuthContext,
        kind: ArtifactKind,
        pin: &ComponentPin,
        trace_id: Option<&str>,
    ) -> Result<PinUpdate, GuiError> {
        let unreadable = PinUpdate {
            artifact_id: pin.artifact_id,
            version: pin.version,
            readable: false,
            latest_version: None,
            changes: Vec::new(),
            diff_unknown: false,
        };
        let meta = match self.artifacts.get(ctx, pin.artifact_id, trace_id).await {
            Ok(m) if m.kind == kind => m,
            Ok(_) | Err(ArtifactError::NotFound | ArtifactError::Forbidden) => {
                return Ok(unreadable)
            }
            Err(e) => return Err(e.into()),
        };
        let latest = meta.current_version;
        let (changes, diff_unknown) = if latest > pin.version {
            // 本体の viewer と種類は確かめたので、2 つの版の本文だけを読む。
            let from = self
                .pinned_body(ctx, pin.artifact_id, pin.version, trace_id)
                .await?;
            let to = self
                .pinned_body(ctx, pin.artifact_id, latest, trace_id)
                .await?;
            match (from, to) {
                (Some(from), Some(to)) => (diff_versions(kind, &from, &to), false),
                // 固定している版が無い（壊れたピン）など。差は言えない（最新の版は返し、調べられなかったと言う）。
                _ => (Vec::new(), true),
            }
        } else {
            (Vec::new(), false)
        };
        Ok(PinUpdate {
            artifact_id: pin.artifact_id,
            version: pin.version,
            readable: true,
            latest_version: Some(latest),
            changes,
            diff_unknown,
        })
    }
}

impl MiniAppStore {
    /// 版の本文を呼んだ人の権限で読む。無い・読めない版は `None`（存在を区別しない）。
    async fn pinned_body(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        version: i64,
        trace_id: Option<&str>,
    ) -> Result<Option<Value>, GuiError> {
        match self.artifacts.get_version(ctx, id, version, trace_id).await {
            Ok(v) => Ok(Some(v.body)),
            Err(ArtifactError::NotFound | ArtifactError::Forbidden) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

/// 2 つの版の本文の差（種類ごと）。
#[must_use]
pub fn diff_versions(kind: ArtifactKind, from: &Value, to: &Value) -> Vec<PinDiff> {
    match kind {
        ArtifactKind::Workflow => input_changes(
            &input_hints(from.get("input_schema")).unwrap_or_default(),
            &input_hints(to.get("input_schema")).unwrap_or_default(),
        ),
        ArtifactKind::DataView => {
            let mut out = Vec::new();
            for (field, diff) in [
                ("table_id", PinDiff::ViewTable),
                ("query", PinDiff::ViewQuery),
                ("display", PinDiff::ViewDisplay),
            ] {
                if from.get(field) != to.get(field) {
                    out.push(diff);
                }
            }
            out
        }
        _ => Vec::new(),
    }
}

/// 入力項目の手がかりの差（新しい版の並び、なくなったものは最後）。
fn input_changes(from: &[WorkflowInputHint], to: &[WorkflowInputHint]) -> Vec<PinDiff> {
    let mut out = Vec::new();
    for new in to {
        let label = new.label.clone();
        let Some(old) = from.iter().find(|h| h.key == new.key) else {
            out.push(PinDiff::InputAdded {
                key: new.key.clone(),
                label,
                required: new.required,
            });
            continue;
        };
        if old.kind != new.kind {
            out.push(PinDiff::InputType {
                key: new.key.clone(),
                label: label.clone(),
                from: old.kind,
                to: new.kind,
            });
        } else if old.options != new.options || old.multi != new.multi {
            out.push(PinDiff::InputOptions {
                key: new.key.clone(),
                label: label.clone(),
            });
        }
        if old.required != new.required {
            out.push(PinDiff::InputRequired {
                key: new.key.clone(),
                label,
                required: new.required,
            });
        }
    }
    for old in from {
        if !to.iter().any(|h| h.key == old.key) {
            out.push(PinDiff::InputRemoved {
                key: old.key.clone(),
                label: old.label.clone(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn wf(schema: Value) -> Value {
        json!({ "ir_version": 1, "name": "expense", "input_schema": schema })
    }

    /// 入力項目の増減・必須・種類・選択肢の変化を、新しい版の並びで返す。
    #[test]
    fn workflow_diff_lists_input_changes() {
        let from = wf(json!({
            "type": "object", "required": ["amount"],
            "properties": {
                "amount": { "type": "integer", "title": "金額" },
                "category": { "type": "string", "enum": ["交通費", "交際費"] },
                "memo": { "type": "string", "title": "メモ" }
            }
        }));
        let to = wf(json!({
            "type": "object", "required": ["title", "memo"],
            "x-order": ["title", "amount", "category", "memo"],
            "properties": {
                "title": { "type": "string", "title": "件名" },
                "amount": { "type": "string", "title": "金額" },
                "category": { "type": "string", "enum": ["交通費", "交際費", "会議費"] },
                "memo": { "type": "string", "title": "メモ" }
            }
        }));
        let diff = diff_versions(ArtifactKind::Workflow, &from, &to);
        assert_eq!(
            diff,
            vec![
                PinDiff::InputAdded {
                    key: "title".into(),
                    label: Some("件名".into()),
                    required: true
                },
                PinDiff::InputType {
                    key: "amount".into(),
                    label: Some("金額".into()),
                    from: InputHintType::Number,
                    to: InputHintType::Text
                },
                PinDiff::InputRequired {
                    key: "amount".into(),
                    label: Some("金額".into()),
                    required: false
                },
                PinDiff::InputOptions {
                    key: "category".into(),
                    label: None
                },
                PinDiff::InputRequired {
                    key: "memo".into(),
                    label: Some("メモ".into()),
                    required: true
                },
            ]
        );
        // なくなった入力項目は最後。入力項目を決めていない版は、空として比べる。
        let gone = diff_versions(
            ArtifactKind::Workflow,
            &from,
            &json!({ "ir_version": 1, "name": "expense" }),
        );
        assert_eq!(gone.len(), 3);
        assert!(gone
            .iter()
            .all(|d| matches!(d, PinDiff::InputRemoved { .. })));
        // 同じ入力項目なら差は無い。
        assert!(diff_versions(ArtifactKind::Workflow, &from, &from.clone()).is_empty());
    }

    /// 保存ビューは、テーブル・クエリ・表示の設定のどれが変わるかだけを言う（列の名前や値は返さない）。
    #[test]
    fn view_diff_names_only_the_parts_that_change() {
        let table = Uuid::from_u128(1);
        let from =
            json!({ "table_id": table, "query": { "limit": 10 }, "display": { "kind": "table" } });
        let to =
            json!({ "table_id": table, "query": { "limit": 20 }, "display": { "kind": "table" } });
        assert_eq!(
            diff_versions(ArtifactKind::DataView, &from, &to),
            vec![PinDiff::ViewQuery]
        );
        let moved = json!({ "table_id": Uuid::from_u128(2), "query": { "limit": 10 } });
        assert_eq!(
            diff_versions(ArtifactKind::DataView, &from, &moved),
            vec![PinDiff::ViewTable, PinDiff::ViewDisplay]
        );
    }
}

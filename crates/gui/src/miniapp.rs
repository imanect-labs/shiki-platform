//! ミニアプリの body スキーマ＋検証（Task 6.10 / miniapp-platform §6）。
//!
//! ミニアプリ = skill ＋ UI スペック ＋ ワークフローの**バージョン固定参照**を束ねた
//! アーティファクト（kind=mini_app）。テーブル（構造化データ）は Phase 9 合流後に追加。
//! 部品は常に明示ピン（再現性）で、共有はミニアプリ本体の ReBAC が正
//! （部品の個別共有は不要 — 解決はバンドル権限チョークポイント
//! [`ArtifactStore::get_version_via_bundle`] を通る）。

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::validate::GuiValidationError;

/// ミニアプリが束ねられるワークフロー数の上限。
pub const MAX_WORKFLOWS: usize = 20;

/// 部品への固定参照（常に明示ピン）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ComponentPin {
    pub artifact_id: Uuid,
    /// JSON 経由の数値のため TS では number（bigint にしない）。
    #[ts(type = "number")]
    pub version: i64,
}

/// 名前付きワークフロー参照（UI スペックの束縛照合・表示用の別名つき）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct NamedComponentPin {
    /// 表示・参照用の別名（ミニアプリ内で一意）。
    pub alias: String,
    pub artifact_id: Uuid,
    /// JSON 経由の数値のため TS では number（bigint にしない）。
    #[ts(type = "number")]
    pub version: i64,
}

/// ミニアプリ本文（artifact kind=mini_app の body JSONB）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MiniAppBody {
    pub description: String,
    /// 初期描画する UI スペック（kind=ui_spec・必須）。
    pub ui_spec: ComponentPin,
    /// 初期コンテキストに適用する skill（kind=skill・任意）。
    #[serde(default)]
    pub skill: Option<ComponentPin>,
    /// 束ねるワークフロー（kind=workflow・UI スペックの workflow 束縛はこの集合に限る）。
    #[serde(default)]
    pub workflows: Vec<NamedComponentPin>,
}

/// mini_app body の構造検証（同期・純粋）。参照の存在・kind・権限は
/// [`MiniAppStore`](crate::miniapp_store::MiniAppStore) が作成者権限で解決する。
pub fn validate_miniapp_body(
    raw: &serde_json::Value,
) -> Result<MiniAppBody, Vec<GuiValidationError>> {
    let body: MiniAppBody = match serde_path_to_error::deserialize(raw.clone()) {
        Ok(body) => body,
        Err(e) => {
            let path = e.path().to_string();
            let err = GuiValidationError::new("miniapp.schema_violation", e.inner().to_string());
            return Err(vec![if path.is_empty() || path == "." {
                err
            } else {
                err.at(path)
            }]);
        }
    };
    let mut errors = Vec::new();
    if body.description.trim().is_empty() {
        errors.push(GuiValidationError::new(
            "miniapp.empty_description",
            "description は必須です",
        ));
    }
    if body.workflows.len() > MAX_WORKFLOWS {
        errors.push(
            GuiValidationError::new(
                "miniapp.too_many_workflows",
                format!("workflows は最大 {MAX_WORKFLOWS} 件"),
            )
            .at("workflows"),
        );
    }
    let mut aliases = std::collections::HashSet::new();
    let mut ids = std::collections::HashSet::new();
    for (i, wf) in body.workflows.iter().enumerate() {
        let path = format!("workflows[{i}]");
        if wf.alias.trim().is_empty() || wf.alias.chars().count() > 64 {
            errors.push(
                GuiValidationError::new("miniapp.invalid_alias", "alias は 1〜64 文字").at(&path),
            );
        }
        if !aliases.insert(wf.alias.clone()) {
            errors.push(
                GuiValidationError::new(
                    "miniapp.duplicate_alias",
                    format!("alias '{}' が重複しています", wf.alias),
                )
                .at(&path),
            );
        }
        if !ids.insert(wf.artifact_id) {
            errors.push(
                GuiValidationError::new(
                    "miniapp.duplicate_workflow",
                    "同じワークフローを複数回束ねています",
                )
                .at(&path),
            );
        }
        if wf.version < 1 {
            errors.push(
                GuiValidationError::new("miniapp.invalid_version", "version は 1 以上").at(&path),
            );
        }
    }
    if body.ui_spec.version < 1 {
        errors.push(
            GuiValidationError::new("miniapp.invalid_version", "version は 1 以上").at("ui_spec"),
        );
    }
    if let Some(skill) = &body.skill {
        if skill.version < 1 {
            errors.push(
                GuiValidationError::new("miniapp.invalid_version", "version は 1 以上").at("skill"),
            );
        }
    }
    if errors.is_empty() {
        Ok(body)
    } else {
        Err(errors)
    }
}

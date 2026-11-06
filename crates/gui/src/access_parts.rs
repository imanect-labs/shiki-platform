//! アプリの部品とワークフローが要る権限を挙げる（GAP-37・判定は [`crate::access::AccessChecker`]）。
//!
//! - 束ねたワークフロー（ボタンの実行先）: IR から、手動の実行で本人に要る権限
//!   （[`workflow_engine::RegistrationService::access_needs`]・単一の定義）。
//! - 束ねた保存ビュー（画面の表）: ビューのテーブルを読む。
//! - 束ねたスキル（AI に聞く）: 参照するフォルダー・ファイルと参考資料を読む（見られない文書は AI の答えに使われない）。
//!
//! 部品の定義はバンドル権限でまとめて読む（[`artifact::ArtifactStore::get_versions_via_bundle`]・監査 1 行）。ピンの照合は
//! ここ（保存済みの版の本文のピンだけを読む）が担う。

use artifact::ArtifactKind;
use authz::AuthContext;
use serde_json::Value;
use uuid::Uuid;
use workflow_engine::{AccessKind, AccessObject, AccessUnchecked, RegistrationService, WorkflowIr};

use crate::access::{AccessPart, PartNeed, PartNeeds};
use crate::miniapp_store::MiniAppStore;
use crate::skill::{validate_skill_body, SkillBody};
use crate::store::GuiError;

/// ワークフローそのものが要る権限（共有されたワークフローを手動で実行する人・部品の名前は持たない）。
#[must_use]
pub fn workflow_needs(ir: &WorkflowIr) -> PartNeeds {
    PartNeeds {
        part: None,
        needs: ir_needs(ir),
        unchecked: ir_gaps(ir),
        unavailable: false,
    }
}

fn ir_needs(ir: &WorkflowIr) -> Vec<PartNeed> {
    RegistrationService::access_needs(ir)
        .into_iter()
        .map(|n| PartNeed {
            object: n.object,
            id: n.object_id,
            kind: n.kind,
            conditional: n.conditional,
        })
        .collect()
}

/// 前もって確かめられないブロック（GAP-82・[`RegistrationService::access_gaps`]）。
fn ir_gaps(ir: &WorkflowIr) -> Vec<(String, AccessUnchecked)> {
    RegistrationService::access_gaps(ir)
        .into_iter()
        .map(|g| (g.block, g.kind))
        .collect()
}

/// 保存ビューが要る権限（ビューのテーブルを読む）。
fn view_needs(view: &data::DataViewBody) -> Vec<PartNeed> {
    vec![PartNeed::always(
        AccessObject::Table,
        Some(view.table_id),
        AccessKind::Read,
    )]
}

/// スキルが要る権限（参照するフォルダー・ファイルと参考資料を読む）。
fn skill_needs(skill: &SkillBody) -> Vec<PartNeed> {
    let mut needs = Vec::new();
    if let Some(scope) = &skill.knowledge_scope {
        needs.extend(
            scope
                .folders
                .iter()
                .map(|id| PartNeed::always(AccessObject::Folder, Some(*id), AccessKind::Read)),
        );
        needs.extend(
            scope
                .files
                .iter()
                .map(|id| PartNeed::always(AccessObject::File, Some(*id), AccessKind::Read)),
        );
    }
    needs.extend(
        skill
            .references
            .iter()
            .map(|id| PartNeed::always(AccessObject::File, Some(*id), AccessKind::Read)),
    );
    needs
}

/// 部品の版の本文が要る権限（ワークフロー・保存ビュー・スキル・アプリの確かめと同じ定義）と、前もって確かめられない
/// ブロックがあるか（GAP-82・ワークフローだけ）。本文を読めない・ほかの種類なら `None`（部品の版を上げる差・GAP-11）。
pub(crate) fn body_needs(kind: ArtifactKind, body: &Value) -> Option<(Vec<PartNeed>, bool)> {
    match kind {
        ArtifactKind::Workflow => WorkflowIr::from_json(body)
            .ok()
            .map(|ir| (ir_needs(&ir), !ir_gaps(&ir).is_empty())),
        ArtifactKind::DataView => data::DataViewBody::from_stored(body.clone())
            .ok()
            .map(|v| (view_needs(&v), false)),
        ArtifactKind::Skill => validate_skill_body(body)
            .ok()
            .map(|s| (skill_needs(&s), false)),
        _ => None,
    }
}

/// 読めなかった部品（消えた・本文が壊れている）。
fn unavailable(part: AccessPart) -> PartNeeds {
    PartNeeds {
        part: Some(part),
        unavailable: true,
        ..PartNeeds::default()
    }
}

impl MiniAppStore {
    /// アプリの部品（束ねたワークフロー・保存ビュー・スキル）が要る権限と、確かめた版。
    ///
    /// 版は開くときと同じ決まり（[`Self::load_saved`]・省けば公開した版、使う人は公開した版だけ）。部品の定義はバンドル
    /// 権限でまとめて読み、`purpose` を監査に残す。読めない部品は `unavailable` にして、ほかの部品は確かめる。
    pub async fn part_needs(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
        version: Option<i64>,
        purpose: &str,
        trace_id: Option<&str>,
    ) -> Result<(i64, Vec<PartNeeds>), GuiError> {
        let (_, version, body) = self.load_saved(ctx, app_id, version, trace_id).await?;
        let mut pins: Vec<(Uuid, i64)> = Vec::new();
        pins.extend(body.workflows.iter().map(|w| (w.artifact_id, w.version)));
        pins.extend(body.data_views.iter().map(|v| (v.artifact_id, v.version)));
        pins.extend(body.skill.iter().map(|s| (s.artifact_id, s.version)));
        let read = self
            .artifacts
            .get_versions_via_bundle(ctx, app_id, &pins, purpose, trace_id)
            .await?;
        let body_of = |id: Uuid, version: i64, kind: ArtifactKind| {
            read.get(&(id, version))
                .filter(|(k, _)| *k == kind)
                .map(|(_, b)| b)
        };
        let mut out = Vec::new();
        for w in &body.workflows {
            let part = AccessPart::Workflow {
                alias: w.alias.clone(),
            };
            let ir = body_of(w.artifact_id, w.version, ArtifactKind::Workflow)
                .and_then(|b| WorkflowIr::from_json(b).ok());
            out.push(match ir {
                Some(ir) => PartNeeds {
                    part: Some(part),
                    needs: ir_needs(&ir),
                    unchecked: ir_gaps(&ir),
                    unavailable: false,
                },
                None => unavailable(part),
            });
        }
        for v in &body.data_views {
            let part = AccessPart::DataView {
                alias: v.alias.clone(),
            };
            let view = body_of(v.artifact_id, v.version, ArtifactKind::DataView)
                .and_then(|b| data::DataViewBody::from_stored(b.clone()).ok());
            out.push(match view {
                Some(view) => PartNeeds {
                    part: Some(part),
                    needs: view_needs(&view),
                    ..PartNeeds::default()
                },
                None => unavailable(part),
            });
        }
        if let Some(pin) = body.skill {
            let skill = body_of(pin.artifact_id, pin.version, ArtifactKind::Skill)
                .and_then(|b| validate_skill_body(b).ok());
            out.push(match skill {
                Some(skill) => PartNeeds {
                    part: Some(AccessPart::Skill),
                    needs: skill_needs(&skill),
                    ..PartNeeds::default()
                },
                None => unavailable(AccessPart::Skill),
            });
        }
        Ok((version, out))
    }
}

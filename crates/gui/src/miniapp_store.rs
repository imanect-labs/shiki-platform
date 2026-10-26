//! ミニアプリの保存・解決（Task 6.10）。
//!
//! 保存時は**作成者の権限**で全ピン（skill/workflow/data_view）の存在・kind・viewer を検証する。画面の
//! ボタンの実行先がバンドルの別名に限られることは body の検証（[`validate_miniapp_body`]）が見る。
//! 解決（実行）時は**ミニアプリ本体の viewer**のみを要求し、部品はバンドル権限
//! チョークポイント（[`ArtifactStore::get_version_via_bundle`]）で読む — 共有相手は
//! 部品を個別共有されなくても実行できる（miniapp-platform §7）。
//!
//! 解決で読む部品は任意の skill のピンだけ（ワークフロー・保存ビューは、ボタン・表が使うときに読む）。
//! skill が消えた・本文が壊れているだけならアプリ全体を止めず、skill だけを使えないと返す
//! （[`ResolvedSkill::Unavailable`]・BUG-17）。

use std::sync::Arc;
use storage::audit::AuditAction;

use artifact::{ArtifactError, ArtifactKind, ArtifactStore, NewArtifact};
use authz::AuthContext;
use serde_json::json;
use sqlx::PgPool;
use storage::audit::{AuditEntry, AuditRecorder, Decision};
use uuid::Uuid;

use crate::miniapp::{validate_miniapp_body, ComponentPin, MiniAppBody};
use crate::skill::{validate_skill_body, SkillBody};
use crate::store::GuiError;
use crate::validate::GuiValidationError;

/// 解決済みミニアプリ（実行面が使う一式・全て検証済み）。
#[derive(Debug, Clone)]
pub struct ResolvedMiniApp {
    pub id: Uuid,
    pub version: i64,
    /// 検証済みの本文（画面のボタンの実行先は `workflows` の別名に限る）。
    pub body: MiniAppBody,
    /// 束ねた skill（任意のピン）。
    pub skill: ResolvedSkill,
}

impl ResolvedMiniApp {
    /// 使えない部品（壊れた任意のピン）の理由。`path` が部品の位置（`skill`）。
    pub fn unavailable(&self) -> Vec<GuiValidationError> {
        self.skill.unavailable().cloned().into_iter().collect()
    }
}

/// 束ねた skill（任意のピン）の解決結果（BUG-17）。
#[derive(Debug, Clone)]
pub enum ResolvedSkill {
    /// ピンが無い。
    None,
    /// バンドル権限で読め、本文の検証も通った。
    Ready(Box<SkillBody>),
    /// ピンはあるが使えない（skill が削除された・本文が壊れている）。アプリは開けて、skill だけが使えない。
    ///
    /// **ピンが無い（[`ResolvedSkill::None`]）のと同じに扱わないこと**。skill の知識スコープ・許可ツールは
    /// 絞る側なので、外して動かすと広がる（skill の適用は fail-closed・Task 6.7）。
    Unavailable(GuiValidationError),
}

impl ResolvedSkill {
    /// 使える skill の本文（ピンが無い・使えないときは `None`）。
    pub fn ready(&self) -> Option<&SkillBody> {
        match self {
            ResolvedSkill::Ready(body) => Some(body.as_ref()),
            ResolvedSkill::None | ResolvedSkill::Unavailable(_) => None,
        }
    }

    /// 使えない理由（使えないときだけ）。
    pub fn unavailable(&self) -> Option<&GuiValidationError> {
        match self {
            ResolvedSkill::Unavailable(reason) => Some(reason),
            ResolvedSkill::None | ResolvedSkill::Ready(_) => None,
        }
    }
}

/// ミニアプリの保存/解決。
#[derive(Clone)]
pub struct MiniAppStore {
    pub(crate) artifacts: Arc<ArtifactStore>,
    pub(crate) audit: AuditRecorder,
    /// 画面のボタンの起動の記録（`miniapp_action_run`）。
    pub(crate) db: PgPool,
}

impl MiniAppStore {
    pub fn new(artifacts: Arc<ArtifactStore>, db: PgPool) -> Self {
        MiniAppStore {
            artifacts,
            audit: AuditRecorder::new(db.clone()),
            db,
        }
    }

    /// 新しいミニアプリを保存する（構造検証＋ピン解決 → artifact version 1）。
    pub async fn create(
        &self,
        ctx: &AuthContext,
        name: &str,
        raw_body: &serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<(Uuid, MiniAppBody), GuiError> {
        let body = self.validate_pins(ctx, raw_body, trace_id).await?;
        let artifact = self
            .artifacts
            .create(
                ctx,
                NewArtifact {
                    kind: ArtifactKind::MiniApp,
                    name: name.to_string(),
                    body: raw_body.clone(),
                },
                trace_id,
            )
            .await?;
        Ok((artifact.id, body))
    }

    /// 既存ミニアプリに新バージョンを追記する。
    pub async fn update(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        raw_body: &serde_json::Value,
        expected_version: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<(i64, MiniAppBody), GuiError> {
        let meta = self.artifacts.get(ctx, id, trace_id).await?;
        if meta.kind != ArtifactKind::MiniApp {
            return Err(GuiError::Validation(vec![GuiValidationError::new(
                "miniapp.kind_mismatch",
                "このアーティファクトは mini_app ではありません",
            )]));
        }
        let body = self.validate_pins(ctx, raw_body, trace_id).await?;
        let version = self
            .artifacts
            .append_version(ctx, id, raw_body.clone(), expected_version, trace_id)
            .await?;
        Ok((version.version, body))
    }

    /// ミニアプリを解決する（本体 viewer → 部品はバンドル権限で読み込み・再検証）。
    ///
    /// `version` 省略時は current。戻りは全て検証済み（実行面はこの結果のみを信用する）。
    pub async fn resolve(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        version: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<ResolvedMiniApp, GuiError> {
        let (version, body) = self.load_saved(ctx, id, version, trace_id).await?;

        let skill = match body.skill {
            Some(pin) => self.resolve_skill(ctx, id, pin, trace_id).await?,
            None => ResolvedSkill::None,
        };

        // 解決を監査に残す（誰が・どの版を・どのピン集合で実行面へ持ち出したか・Task 6.12）。
        // 使えなかった部品も残す（壊れたピンのまま開いた事実を追えるように・BUG-17）。
        let unavailable: Vec<serde_json::Value> = skill
            .unavailable()
            .map(|e| json!({ "path": e.path, "code": e.code }))
            .into_iter()
            .collect();
        let entry = AuditEntry {
            action: AuditAction::MiniappResolve,
            object_type: "artifact",
            object_id: &id.to_string(),
            decision: Decision::Allow,
            trace_id,
            metadata: json!({
                "version": version,
                "skill": body.skill,
                "workflows": body.workflows,
                "data_views": body.data_views,
                "unavailable": unavailable,
            }),
        };
        if let Err(e) = self.audit.record(ctx, entry).await {
            tracing::warn!(error = %e, "miniapp.resolve の監査記録に失敗");
        }

        Ok(ResolvedMiniApp {
            id,
            version,
            body,
            skill,
        })
    }

    /// 保存済みの版の本文を読み、検証し直す（本体の viewer・kind・版の実在）。部品は読まず、監査も残さない。
    ///
    /// [`Self::resolve`] の前段で、画面の表の描画・ページ送りはこれだけを毎回通す（BUG-24）。権限は呼ぶたびに
    /// 確かめ直す（剥奪は次の呼び出しから効く）。開いたことの監査（`miniapp.resolve`）は開いたときの解決が残し、
    /// 表の 1 回ごとの操作は `ui_action.invoke` が残すので、ページを送るたびに解決をやり直して監査を重ねない。
    pub(crate) async fn load_saved(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        version: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<(i64, MiniAppBody), GuiError> {
        let meta = self.artifacts.get(ctx, id, trace_id).await?;
        if meta.kind != ArtifactKind::MiniApp {
            return Err(GuiError::Artifact(ArtifactError::NotFound));
        }
        let version = version.unwrap_or(meta.current_version);
        let v = self
            .artifacts
            .get_version(ctx, id, version, trace_id)
            .await?;
        // 描画前再検証: 保存済みでも壊れた本文（旧形式を含む）を実行面へ流さない。
        let body = validate_miniapp_body(&v.body).map_err(GuiError::Validation)?;
        Ok((version, body))
    }

    /// 任意の skill のピンをバンドル権限で読む（BUG-17）。
    ///
    /// skill が消えた（論理削除・版が無い）・本文が今の検証を通らないだけなら、アプリ全体を「見つかりません」に
    /// せず skill だけを使えないと返す。権限の不足と内部の誤りは隠さずに返す（本体の viewer は直前に
    /// 確かめているので、ここでの不足は剥奪との競合で、アプリごと開けないのが正しい）。
    async fn resolve_skill(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
        pin: ComponentPin,
        trace_id: Option<&str>,
    ) -> Result<ResolvedSkill, GuiError> {
        let read = self
            .artifacts
            .get_version_via_bundle(ctx, app_id, pin.artifact_id, pin.version, trace_id)
            .await;
        let reason = match read {
            Ok(v) => match validate_skill_body(&v.body) {
                Ok(body) => return Ok(ResolvedSkill::Ready(Box::new(body))),
                Err(errors) => {
                    tracing::warn!(
                        app_id = %app_id,
                        skill_id = %pin.artifact_id,
                        version = pin.version,
                        errors = ?errors,
                        "ミニアプリの skill の本文が検証を通らない（skill だけを使えないとする）"
                    );
                    GuiValidationError::new(
                        "miniapp.skill_broken",
                        format!(
                            "スキル（v{}）の本文を読めません（古い形式か、壊れています）",
                            pin.version
                        ),
                    )
                }
            },
            Err(ArtifactError::NotFound) => GuiValidationError::new(
                "miniapp.pin_unresolved",
                format!(
                    "スキル（v{}）が見つかりません（削除されたか、その版がありません）",
                    pin.version
                ),
            ),
            Err(e) => return Err(e.into()),
        };
        Ok(ResolvedSkill::Unavailable(reason.at("skill")))
    }

    /// 保存時のピン解決（**作成者の権限**で存在・kind・viewer を検証する）。
    async fn validate_pins(
        &self,
        ctx: &AuthContext,
        raw_body: &serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<MiniAppBody, GuiError> {
        let body = validate_miniapp_body(raw_body).map_err(GuiError::Validation)?;
        let mut errors = Vec::new();
        if let Some(pin) = &body.skill {
            if let Err(e) = self
                .check_pin(ctx, pin, ArtifactKind::Skill, trace_id)
                .await
            {
                errors.push(e.at("skill"));
            }
        }
        for (i, wf) in body.workflows.iter().enumerate() {
            let pin = ComponentPin {
                artifact_id: wf.artifact_id,
                version: wf.version,
            };
            if let Err(e) = self
                .check_pin(ctx, &pin, ArtifactKind::Workflow, trace_id)
                .await
            {
                errors.push(e.at(format!("workflows[{i}]")));
            }
        }
        // 保存ビュー（Task 15.7）: 保存する人が**その版を読める**こと（viewer・kind・版の実在）を
        // 確かめて版に固定する。データの権限はここでは広げない（実行時に閲覧者本人で毎回評価する）。
        for (i, view) in body.data_views.iter().enumerate() {
            let pin = ComponentPin {
                artifact_id: view.artifact_id,
                version: view.version,
            };
            let path = format!("data_views[{i}]");
            match self
                .check_pin(ctx, &pin, ArtifactKind::DataView, trace_id)
                .await
            {
                Ok(raw) => {
                    if data::DataViewBody::from_stored(raw).is_err() {
                        errors.push(
                            GuiValidationError::new(
                                "miniapp.data_view_broken",
                                format!(
                                    "保存ビュー {}（v{}）の本文を読めません",
                                    view.artifact_id, view.version
                                ),
                            )
                            .at(path),
                        );
                    }
                }
                Err(e) => errors.push(e.at(path)),
            }
        }
        if errors.is_empty() {
            Ok(body)
        } else {
            Err(GuiError::Validation(errors))
        }
    }

    /// 1 ピンの存在・kind・viewer 検証（作成者権限・存在秘匿の同一メッセージ）。
    async fn check_pin(
        &self,
        ctx: &AuthContext,
        pin: &ComponentPin,
        kind: ArtifactKind,
        trace_id: Option<&str>,
    ) -> Result<serde_json::Value, GuiValidationError> {
        let unresolved = || {
            GuiValidationError::new(
                "miniapp.pin_unresolved",
                format!(
                    "参照 {}（kind={}）が見つからないか権限がありません",
                    pin.artifact_id,
                    kind.as_str()
                ),
            )
        };
        let meta = self
            .artifacts
            .get(ctx, pin.artifact_id, trace_id)
            .await
            .map_err(|e| match e {
                ArtifactError::NotFound | ArtifactError::Forbidden => unresolved(),
                other => GuiValidationError::new(
                    "miniapp.pin_resolve_error",
                    format!("参照解決に失敗しました: {other}"),
                ),
            })?;
        if meta.kind != kind {
            return Err(unresolved());
        }
        let v = self
            .artifacts
            .get_version(ctx, pin.artifact_id, pin.version, trace_id)
            .await
            .map_err(|e| match e {
                ArtifactError::NotFound | ArtifactError::Forbidden => unresolved(),
                other => GuiValidationError::new(
                    "miniapp.pin_resolve_error",
                    format!("参照解決に失敗しました: {other}"),
                ),
            })?;
        Ok(v.body)
    }
}

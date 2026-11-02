//! ミニアプリの保存・解決（Task 6.10）。
//!
//! 保存時は**保存する人の権限**でピン（skill/workflow/data_view）の存在・kind・viewer を検証する。確かめるのは
//! 足したピンと、対象か版を変えたピンだけで、前の版と同じピンは確かめ直さない（共同編集者が、自分の読めない部品を
//! 束ねたアプリを、その部品に触れずに保存できる・D-16 の (d)・BUG-80）。画面のボタンの実行先がバンドルの別名に
//! 限られることは body の検証（[`validate_miniapp_body`]）が見る。
//!
//! 保存は**下書きの版**になる（D-16 の (a)・GAP-36）。使う人（editor でない viewer）が開く・押すのは公開した版
//! だけで（`artifact.published_version`・[`ArtifactStore::get_version`] が守る）、editor は下書きを試せる。公開は
//! [`crate::miniapp_publish`]。画面の書き損じ（[`screen_problems`]）は、下書きには注意つきで保存でき、公開と
//! AI の経路（[`SaveMode::Strict`]）では止める（D-16 の (c)）。
//! 解決（実行）時は**ミニアプリ本体の viewer**のみを要求し、部品はバンドル権限
//! チョークポイント（[`ArtifactStore::get_version_via_bundle`]）で読む — 共有相手は
//! 部品を個別共有されなくても実行できる（miniapp-platform §7）。
//!
//! 解決で読む部品は任意の skill のピンだけ（ワークフロー・保存ビューは、ボタン・表が使うときに読む）。
//! skill が消えた・本文が壊れているだけならアプリ全体を止めず、skill だけを使えないと返す
//! （[`ResolvedSkill::Unavailable`]・BUG-17）。

use std::sync::Arc;
use storage::audit::AuditAction;

use artifact::{Artifact, ArtifactError, ArtifactKind, ArtifactStore, NewArtifact};
use authz::{AuthContext, Relation};
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::PgPool;
use storage::audit::{AuditEntry, AuditRecorder, Decision};
use uuid::Uuid;

use crate::miniapp::{
    screen_problems, validate_miniapp_body, ComponentPin, MiniAppBody, NamedComponentPin,
};
use crate::skill::{validate_skill_body, SkillBody};
use crate::store::GuiError;
use crate::validate::GuiValidationError;

/// 保存の確かめ方（D-16 の (c)）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveMode {
    /// 人が画面から保存する（下書き）。画面の書き損じは注意（[`SavedMiniApp::warnings`]）として返し、保存する。
    Draft,
    /// AI の経路（会話の回答をアプリとして保存する・AI が作る道具）。画面の書き損じも誤りとして止める
    /// （公開の前の確かめと同じ・[`crate::miniapp_publish`]）。
    Strict,
}

/// 保存した版（下書き）。
#[derive(Debug, Clone)]
pub struct SavedMiniApp {
    pub id: Uuid,
    pub version: i64,
    pub body: MiniAppBody,
    /// 公開した版（保存しても変わらない。新しいアプリはまだ公開していないので `None`・D-16）。
    pub published_version: Option<i64>,
    /// 画面の書き損じ（保存はできた。直すまで公開できない・[`screen_problems`]）。
    pub warnings: Vec<GuiValidationError>,
}

/// 解決済みミニアプリ（実行面が使う一式・全て検証済み）。
#[derive(Debug, Clone)]
pub struct ResolvedMiniApp {
    pub id: Uuid,
    pub version: i64,
    /// 検証済みの本文（画面のボタンの実行先は `workflows` の別名に限る）。
    pub body: MiniAppBody,
    /// 束ねた skill（任意のピン）。
    pub skill: ResolvedSkill,
    /// 公開した版（まだ公開していなければ `None`・D-16）。`version` と違えば、開いたのは下書き（editor だけ）。
    pub published_version: Option<i64>,
    /// 前に公開していた版（公開を戻す先）。
    pub previous_published_version: Option<i64>,
    pub published_at: Option<DateTime<Utc>>,
    /// 最新の版（下書きを含む）。
    pub latest_version: i64,
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

    /// 新しいミニアプリを保存する（構造検証＋ピン解決 → artifact version 1）。まだ公開していない（D-16）。
    pub async fn create(
        &self,
        ctx: &AuthContext,
        name: &str,
        raw_body: &serde_json::Value,
        mode: SaveMode,
        trace_id: Option<&str>,
    ) -> Result<SavedMiniApp, GuiError> {
        let (body, warnings) = self.check_body(ctx, raw_body, None, mode, trace_id).await?;
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
        Ok(SavedMiniApp {
            id: artifact.id,
            version: artifact.current_version,
            body,
            published_version: artifact.published_version,
            warnings,
        })
    }

    /// 既存ミニアプリに新バージョン（下書き）を追記する。公開した版は変えない（D-16）。
    ///
    /// ピンは、`expected_version`（無ければ最新の版）の本文と比べて、足したものと対象か版を変えたものだけを
    /// 保存する人の権限で確かめる（BUG-80）。保存できるのはアプリの editor（追記と同じ）で、前の版を読む前に確かめる。
    pub async fn update(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        raw_body: &serde_json::Value,
        expected_version: Option<i64>,
        mode: SaveMode,
        trace_id: Option<&str>,
    ) -> Result<SavedMiniApp, GuiError> {
        let meta = self.artifacts.get(ctx, id, trace_id).await?;
        if meta.kind != ArtifactKind::MiniApp {
            return Err(GuiError::Validation(vec![GuiValidationError::new(
                "miniapp.kind_mismatch",
                "このアーティファクトは mini_app ではありません",
            )]));
        }
        self.artifacts
            .require_relation(
                ctx,
                id,
                Relation::Editor,
                AuditAction::ArtifactVersionAppend,
                trace_id,
            )
            .await?;
        let base = expected_version.unwrap_or(meta.current_version);
        // 比べる前の版。無い版（expected_version の誤り）や今の検証を通らない版なら、すべてのピンを確かめる
        // （追記は版の競合で断る）。
        let previous = match self.artifacts.get_version(ctx, id, base, trace_id).await {
            Ok(v) => validate_miniapp_body(&v.body).ok(),
            Err(ArtifactError::NotFound) => None,
            Err(e) => return Err(e.into()),
        };
        let (body, warnings) = self
            .check_body(ctx, raw_body, previous.as_ref(), mode, trace_id)
            .await?;
        let version = self
            .artifacts
            .append_version(ctx, id, raw_body.clone(), expected_version, trace_id)
            .await?;
        Ok(SavedMiniApp {
            id,
            version: version.version,
            body,
            published_version: meta.published_version,
            warnings,
        })
    }

    /// 保存の前の確かめ（構造・画面の書き損じ・ピン）。画面の書き損じは、下書きなら注意として返し、
    /// [`SaveMode::Strict`] なら誤りに加える。
    async fn check_body(
        &self,
        ctx: &AuthContext,
        raw_body: &serde_json::Value,
        previous: Option<&MiniAppBody>,
        mode: SaveMode,
        trace_id: Option<&str>,
    ) -> Result<(MiniAppBody, Vec<GuiValidationError>), GuiError> {
        let body = validate_miniapp_body(raw_body).map_err(GuiError::Validation)?;
        let problems = screen_problems(&body);
        let mut errors = self.validate_pins(ctx, &body, previous, trace_id).await;
        let warnings = match mode {
            SaveMode::Draft => problems,
            SaveMode::Strict => {
                errors.extend(problems);
                Vec::new()
            }
        };
        if errors.is_empty() {
            Ok((body, warnings))
        } else {
            Err(GuiError::Validation(errors))
        }
    }

    /// ミニアプリを解決する（本体 viewer → 部品はバンドル権限で読み込み・再検証）。
    ///
    /// `version` 省略時は公開した版（まだ公開していなければ最新の版。開けるのは editor だけ・D-16）。使う人（editor で
    /// ない viewer）は公開した版しか開けない（[`ArtifactError::NotPublished`]）。戻りは全て検証済み（実行面はこの
    /// 結果のみを信用する）。
    pub async fn resolve(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        version: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<ResolvedMiniApp, GuiError> {
        let (meta, version, body) = self.load_saved(ctx, id, version, trace_id).await?;

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
            published_version: meta.published_version,
            previous_published_version: meta.previous_published_version,
            published_at: meta.published_at,
            latest_version: meta.current_version,
        })
    }

    /// 保存済みの版の本文を読み、検証し直す（本体の viewer・kind・版の実在）。部品は読まず、監査も残さない。
    ///
    /// [`Self::resolve`] の前段で、画面の表の描画・ページ送りはこれだけを毎回通す（BUG-24）。権限は呼ぶたびに
    /// 確かめ直す（剥奪は次の呼び出しから効く）。開いたことの監査（`miniapp.resolve`）は開いたときの解決が残し、
    /// 表の 1 回ごとの操作は `ui_action.invoke` が残すので、ページを送るたびに解決をやり直して監査を重ねない。
    ///
    /// `version` を省くと公開した版（まだ公開していなければ最新の版）。使う人（editor でない viewer）が読めるのは
    /// 公開した版だけで、ほかの版・まだ公開していないアプリは [`ArtifactError::NotPublished`]（D-16）。
    pub(crate) async fn load_saved(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        version: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<(Artifact, i64, MiniAppBody), GuiError> {
        let meta = self.artifacts.get(ctx, id, trace_id).await?;
        if meta.kind != ArtifactKind::MiniApp {
            return Err(GuiError::Artifact(ArtifactError::NotFound));
        }
        let version = version
            .or(meta.published_version)
            .unwrap_or(meta.current_version);
        let v = self
            .artifacts
            .get_version(ctx, id, version, trace_id)
            .await?;
        // 描画前再検証: 保存済みでも壊れた本文（旧形式を含む）を実行面へ流さない。
        let body = validate_miniapp_body(&v.body).map_err(GuiError::Validation)?;
        Ok((meta, version, body))
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

    /// 保存時のピン解決（**保存する人の権限**で存在・kind・viewer を検証する）。誤りの一覧を返す（無ければ空）。
    ///
    /// 確かめるのは、前の版（`previous`）に無いピン（足したもの・対象か版を変えたもの）だけ（D-16 の (d)・
    /// BUG-80）。前の版と同じピンは、その版を保存した人が確かめてあり、版は不変なので確かめ直さない（共同編集者が
    /// 読めない部品も、触れなければそのまま保存できる）。
    async fn validate_pins(
        &self,
        ctx: &AuthContext,
        body: &MiniAppBody,
        previous: Option<&MiniAppBody>,
        trace_id: Option<&str>,
    ) -> Vec<GuiValidationError> {
        let mut errors = Vec::new();
        if let Some(pin) = body
            .skill
            .filter(|p| previous.and_then(|b| b.skill) != Some(*p))
        {
            let what = format!("スキル（v{}）", pin.version);
            if let Err(e) = self
                .check_pin(ctx, &pin, ArtifactKind::Skill, &what, trace_id)
                .await
            {
                errors.push(e.at("skill"));
            }
        }
        for (i, wf) in body.workflows.iter().enumerate() {
            if unchanged(previous.map(|b| b.workflows.as_slice()), wf) {
                continue;
            }
            let pin = ComponentPin {
                artifact_id: wf.artifact_id,
                version: wf.version,
            };
            let what = format!("ワークフロー「{}」（v{}）", wf.alias, wf.version);
            if let Err(e) = self
                .check_pin(ctx, &pin, ArtifactKind::Workflow, &what, trace_id)
                .await
            {
                errors.push(e.at(format!("workflows[{i}]")));
            }
        }
        // 保存ビュー（Task 15.7）: 保存する人が**その版を読める**こと（viewer・kind・版の実在）を
        // 確かめて版に固定する。データの権限はここでは広げない（実行時に閲覧者本人で毎回評価する）。
        for (i, view) in body.data_views.iter().enumerate() {
            if unchanged(previous.map(|b| b.data_views.as_slice()), view) {
                continue;
            }
            let pin = ComponentPin {
                artifact_id: view.artifact_id,
                version: view.version,
            };
            let path = format!("data_views[{i}]");
            let what = format!("保存ビュー「{}」（v{}）", view.alias, view.version);
            match self
                .check_pin(ctx, &pin, ArtifactKind::DataView, &what, trace_id)
                .await
            {
                Ok(raw) => {
                    if data::DataViewBody::from_stored(raw).is_err() {
                        errors.push(
                            GuiValidationError::new(
                                "miniapp.data_view_broken",
                                format!("{what}の本文を読めません（古い形式か、壊れています）"),
                            )
                            .at(path),
                        );
                    }
                }
                Err(e) => errors.push(e.at(path)),
            }
        }
        errors
    }

    /// 1 ピンの存在・kind・viewer 検証（作成者権限・存在秘匿の同一メッセージ）。
    ///
    /// `what` は誤りの文言で部品を指す言い方（例: `ワークフロー「expense」（v1）`）。文言は利用者に出るので、
    /// 部品の id は入れない（BUG-80）。無い・kind が違う・読めないは同じ文言にする（存在を区別させない）。
    async fn check_pin(
        &self,
        ctx: &AuthContext,
        pin: &ComponentPin,
        kind: ArtifactKind,
        what: &str,
        trace_id: Option<&str>,
    ) -> Result<serde_json::Value, GuiValidationError> {
        let unresolved = || {
            GuiValidationError::new(
                "miniapp.pin_unresolved",
                format!("{what}が見つからないか、あなたには読めません"),
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

/// 前の版にも同じ部品の同じ版のピンがあるか（別名は問わない。別名だけを変えたピンも確かめ直さない・BUG-80）。
fn unchanged(previous: Option<&[NamedComponentPin]>, pin: &NamedComponentPin) -> bool {
    previous.is_some_and(|pins| {
        pins.iter()
            .any(|p| p.artifact_id == pin.artifact_id && p.version == pin.version)
    })
}

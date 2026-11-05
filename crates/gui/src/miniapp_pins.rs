//! 束ねた部品（ワークフロー・保存ビュー・スキル）の新しいバージョンと、上げると変わるところ（GAP-11・GAP-86）。
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
//! - スキルの差は、設定（説明・AI への指示・参照するフォルダー・使ってよいツール・モデル・お手本の対話・スクリプト・
//!   参考資料・コマンド）のどれが変わるか。中身は返さない（GAP-86）。
//!
//! 差の種類を決めるのは [`diff`]（純関数）の 1 か所。

use artifact::{ArtifactError, ArtifactKind};
use authz::AuthContext;
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::miniapp::ComponentPin;
use crate::miniapp_inputs::InputHintType;
use crate::miniapp_store::MiniAppStore;
use crate::store::GuiError;

mod diff;
pub use diff::{diff_versions, VersionDiff};

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
    /// スキルの設定のうち 1 つが変わる（中身は返さない・GAP-86）。
    Skill { part: SkillPart },
}

/// スキルの設定（変わるところを言う単位・[`PinDiff::Skill`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SkillPart {
    /// 説明。
    Description,
    /// AI への指示（指示文）。
    Instructions,
    /// 参照するフォルダーとファイル。
    KnowledgeScope,
    /// 使ってよいツール。
    AllowedTools,
    /// 使うモデルとその設定。
    Model,
    /// お手本の対話。
    FewShot,
    /// スクリプト。
    Scripts,
    /// 参考資料。
    References,
    /// チャットで呼ぶコマンド。
    Command,
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
    /// 束ねる部品（`kind` はワークフロー・保存ビュー・スキル）ごとに、呼んだ人が読める最新の版と、上げると変わるところを
    /// 返す（ピンの順・GAP-11・GAP-86）。読めない部品は `readable: false`。ほかの誤りは返す（読めないと決めつけない）。
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
                (Some(from), Some(to)) => {
                    let d = diff_versions(kind, &from, &to);
                    (d.changes, d.unknown)
                }
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

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
//! - ワークフローの差には、使ってよい操作（`declared_scopes`）の増減を添える（GAP-11）。
//! - どの部品にも、使う人に要る権限（[`crate::access`] と同じ定義・テーブル・ファイル・フォルダーへの読む・書く）の
//!   増減を添える（GAP-11）。対象の名前は呼んだ人に見えるものだけを言い（[`AccessChecker::visible_targets`]）、
//!   見えない対象は種類だけで言う。だれの権限も問わない（他人の権限を問う範囲・D-146 は変えない）。
//!
//! 差の種類を決めるのは [`diff`]（純関数）の 1 か所。

use std::collections::HashMap;

use artifact::{ArtifactError, ArtifactKind};
use authz::AuthContext;
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::access::{AccessChecker, AccessKind, AccessObject, AccessTarget, MAX_ACCESS_OBJECTS};
use crate::miniapp::ComponentPin;
use crate::miniapp_inputs::InputHintType;
use crate::miniapp_store::MiniAppStore;
use crate::store::GuiError;

mod diff;
pub use diff::{diff_versions, AccessChange, AccessChangeKind, VersionDiff};

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
    /// 使ってよい操作が増える（ワークフロー・GAP-11）。`scope` は操作の名前（画面は表示名で言う）。
    ScopeAdded { scope: String },
    /// 使ってよい操作がなくなる（ワークフロー・GAP-11）。
    ScopeRemoved { scope: String },
    /// 対象のテーブルが変わる（保存ビュー）。
    ViewTable,
    /// 絞り込み・並べ替え・取り出す列が変わる（保存ビュー）。
    ViewQuery,
    /// 列の並び・見出し・書式などの表示の設定が変わる（保存ビュー）。
    ViewDisplay,
    /// スキルの設定のうち 1 つが変わる（中身は返さない・GAP-86）。
    Skill { part: SkillPart },
    /// 使う人に要る権限が増える（GAP-11）。`conditional` は場合によって要る（道によって通るブロックか、失敗を
    /// 「エラー時」の出口で扱うブロックだけが要る・BUG-216・足りなくても使う人のボタンは止めない）。
    AccessAdded {
        access: PinAccess,
        conditional: bool,
    },
    /// 場合によって要っていた権限が、毎回要るようになる（足りない人のボタンは止まる・GAP-11）。
    AccessAlways { access: PinAccess },
    /// 使う人に要る権限が要らなくなる（GAP-11）。
    AccessRemoved { access: PinAccess },
}

/// 要る権限の増減の対象と操作（対象の名前は、呼んだ人に見えるときだけ）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct PinAccess {
    pub object: AccessObject,
    pub need: AccessKind,
    /// 対象（呼んだ人に見えるときだけ）。見えない・削除された・実行のときに決まる・確かめていないときは `null`。
    pub target: Option<AccessTarget>,
    /// 対象が実行のときに決まる（入力や前の結果で決まる・まだ選んでいない）。
    pub dynamic: bool,
    /// 増減する対象が多く（[`MAX_ACCESS_OBJECTS`] を超える）、この対象は名前を確かめていない。
    pub over_limit: bool,
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
    /// 最新の版に、前もって確かめられないブロック（スクリプト・シークレット・別のワークフローを実行など・GAP-82）が
    /// ある（ワークフローだけ）。使う人に要る権限の増減は確かめられた範囲だけを言うので、`changes` に増減が無くても
    /// 要る権限が変わらないとは言わない（共有の画面の `unchecked` と同じ決まり）。
    pub unchecked_blocks: bool,
}

/// 名前を引く前の、部品 1 つの結果（要る権限の増減の対象には、まだ名前が無い）。
struct Unnamed {
    update: PinUpdate,
    access: Vec<AccessChange>,
}

impl MiniAppStore {
    /// 束ねる部品（`kind` はワークフロー・保存ビュー・スキル）ごとに、呼んだ人が読める最新の版と、上げると変わるところを
    /// 返す（ピンの順・GAP-11・GAP-86）。読めない部品は `readable: false`。ほかの誤りは返す（読めないと決めつけない）。
    ///
    /// 要る権限の増減の対象の名前は、`names` で呼んだ人に見えるものだけを引く（全部の部品をまとめて 1 回・
    /// [`MAX_ACCESS_OBJECTS`] まで）。
    pub async fn pin_updates(
        &self,
        ctx: &AuthContext,
        kind: ArtifactKind,
        pins: &[ComponentPin],
        names: &AccessChecker<'_>,
        trace_id: Option<&str>,
    ) -> Result<Vec<PinUpdate>, GuiError> {
        let mut unnamed = Vec::with_capacity(pins.len());
        for pin in pins {
            unnamed.push(self.pin_update(ctx, kind, pin, trace_id).await?);
        }
        let mut objects: Vec<(AccessObject, Uuid)> = Vec::new();
        for c in unnamed.iter().flat_map(|u| &u.access) {
            let Some(id) = c.id else { continue };
            if objects.len() < MAX_ACCESS_OBJECTS && !objects.contains(&(c.object, id)) {
                objects.push((c.object, id));
            }
        }
        let targets = if objects.is_empty() {
            HashMap::new()
        } else {
            names.visible_targets(ctx, &objects).await?
        };
        Ok(unnamed
            .into_iter()
            .map(|u| with_names(u, &objects, &targets))
            .collect())
    }

    async fn pin_update(
        &self,
        ctx: &AuthContext,
        kind: ArtifactKind,
        pin: &ComponentPin,
        trace_id: Option<&str>,
    ) -> Result<Unnamed, GuiError> {
        let mut update = PinUpdate {
            artifact_id: pin.artifact_id,
            version: pin.version,
            readable: false,
            latest_version: None,
            changes: Vec::new(),
            diff_unknown: false,
            unchecked_blocks: false,
        };
        let meta = match self.artifacts.get(ctx, pin.artifact_id, trace_id).await {
            Ok(m) if m.kind == kind => m,
            Ok(_) | Err(ArtifactError::NotFound | ArtifactError::Forbidden) => {
                return Ok(Unnamed {
                    update,
                    access: Vec::new(),
                })
            }
            Err(e) => return Err(e.into()),
        };
        let latest = meta.current_version;
        update.readable = true;
        update.latest_version = Some(latest);
        if latest <= pin.version {
            return Ok(Unnamed {
                update,
                access: Vec::new(),
            });
        }
        // 本体の viewer と種類は確かめたので、2 つの版の本文だけを読む。
        let from = self
            .pinned_body(ctx, pin.artifact_id, pin.version, trace_id)
            .await?;
        let to = self
            .pinned_body(ctx, pin.artifact_id, latest, trace_id)
            .await?;
        let (Some(from), Some(to)) = (from, to) else {
            // 固定している版が無い（壊れたピン）など。差は言えない（最新の版は返し、調べられなかったと言う）。
            update.diff_unknown = true;
            return Ok(Unnamed {
                update,
                access: Vec::new(),
            });
        };
        let d = diff_versions(kind, &from, &to);
        update.changes = d.changes;
        update.diff_unknown = d.unknown;
        update.unchecked_blocks = d.unchecked;
        Ok(Unnamed {
            update,
            access: d.access,
        })
    }
}

/// 要る権限の増減に、呼んだ人に見える対象の名前を付けて差に足す（同じ言い方になるものは 1 つにする）。
fn with_names(
    u: Unnamed,
    objects: &[(AccessObject, Uuid)],
    targets: &HashMap<(AccessObject, Uuid), AccessTarget>,
) -> PinUpdate {
    let mut update = u.update;
    for c in u.access {
        let access = PinAccess {
            object: c.object,
            need: c.need,
            target: c.id.and_then(|id| targets.get(&(c.object, id)).cloned()),
            dynamic: c.id.is_none(),
            over_limit: c.id.is_some_and(|id| !objects.contains(&(c.object, id))),
        };
        let diff = match c.change {
            AccessChangeKind::Added { conditional } => PinDiff::AccessAdded {
                access,
                conditional,
            },
            AccessChangeKind::Always => PinDiff::AccessAlways { access },
            AccessChangeKind::Removed => PinDiff::AccessRemoved { access },
        };
        // 見えない対象どうしは同じ言い方になる（id を返さないので、何件あっても 1 行で言う）。
        if !update.changes.contains(&diff) {
            update.changes.push(diff);
        }
    }
    update
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

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// 名前は見える対象にだけ付け、上限の外の対象は確かめていないと言い分ける。見えない対象どうしは同じ言い方なので
    /// 1 つにする（id を返さない）。
    #[test]
    fn names_are_attached_only_to_visible_targets() {
        let added = |n: u128| AccessChange {
            change: AccessChangeKind::Added { conditional: false },
            object: AccessObject::Table,
            need: AccessKind::Add,
            id: Some(id(n)),
        };
        let u = Unnamed {
            update: PinUpdate {
                artifact_id: id(100),
                version: 1,
                readable: true,
                latest_version: Some(2),
                changes: vec![PinDiff::ViewQuery],
                diff_unknown: false,
                unchecked_blocks: false,
            },
            access: vec![
                added(1),
                added(2),
                added(3),
                added(4),
                AccessChange {
                    id: None,
                    ..added(0)
                },
            ],
        };
        let visible = AccessTarget {
            id: id(1),
            name: "経費の台帳".into(),
            app_id: None,
            content_type: None,
        };
        let targets = HashMap::from([((AccessObject::Table, id(1)), visible.clone())]);
        // 2・3 は確かめて見えなかった。4 は上限の外。
        let objects = [
            (AccessObject::Table, id(1)),
            (AccessObject::Table, id(2)),
            (AccessObject::Table, id(3)),
        ];
        let got = with_names(u, &objects, &targets);
        let access = |target, dynamic, over_limit| PinDiff::AccessAdded {
            access: PinAccess {
                object: AccessObject::Table,
                need: AccessKind::Add,
                target,
                dynamic,
                over_limit,
            },
            conditional: false,
        };
        assert_eq!(
            got.changes,
            vec![
                PinDiff::ViewQuery,
                access(Some(visible), false, false),
                access(None, false, false),
                access(None, false, true),
                access(None, true, false),
            ]
        );
    }
}

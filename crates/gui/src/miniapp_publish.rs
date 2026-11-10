//! ミニアプリの公開（D-16 の (a)・GAP-36）。
//!
//! 保存は下書きの版になり（[`MiniAppStore::update`]）、使う人（editor でない viewer）が開く・押すのは公開した版
//! だけ。公開は editor が版を選んで行う（新しい relation は足さず、保存と同じ editor）。前に公開していた版を公開し
//! 直すと公開を戻せる。公開の前に、その版の画面の書き損じ（[`screen_problems`]）があれば止める（D-16 の (c)）。
//! 止めるのは一度も公開していない版だけで、公開を戻すのは止めない（[`problems_block_publish`]）。
//! 部品のピンは、その版を保存した人が保存のときに確かめてある（版は不変）ので、公開する人の権限で確かめ直さない。
//!
//! 公開した版と下書きの差は [`diff_bodies`] が 1 か所で決める（画面は差の種類ごとに言うだけ）。

use artifact::{ArtifactKind, Publication};
use authz::{AuthContext, Relation};
use serde::Serialize;
use storage::audit::AuditAction;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::miniapp::{screen_problems, validate_miniapp_body, MiniAppBody, NamedComponentPin};
use crate::miniapp_store::MiniAppStore;
use crate::store::GuiError;
use crate::validate::GuiValidationError;

/// 公開した版から変わるところ 1 つ（`kind` で判別）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MiniAppChange {
    /// 画面の見出しが変わる。
    Description { from: String, to: String },
    /// 画面（説明の文・図・ボタン）が変わる。
    Screen,
    /// 束ねたスキルが変わる（付ける・外す・ほかのスキルにする・版を変える）。版は無ければ `null`。
    Skill {
        from_version: Option<i64>,
        to_version: Option<i64>,
        /// 別のスキルに替える（付ける・外す・同じスキルの版を変えるときは `false`）。
        replaced: bool,
    },
    /// 束ねたワークフロー（ボタンの実行先）が変わる。
    Workflow(PinChange),
    /// 束ねた保存ビュー（画面の表）が変わる。
    DataView(PinChange),
}

/// 名前で束ねた部品（ワークフロー・保存ビュー）の変わり方。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct PinChange {
    /// 新しい版での別名（外したものは前の別名）。
    pub alias: String,
    /// 前の版での別名（足したものは `null`・別名を変えたものは前の別名）。
    pub from_alias: Option<String>,
    /// 前の版で束ねていた版（足したものは `null`）。
    pub from_version: Option<i64>,
    /// 新しい版で束ねる版（外したものは `null`）。
    pub to_version: Option<i64>,
}

/// 公開の前の確かめの結果（editor だけ）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct PublishPreview {
    /// 公開しようとしている版。
    pub version: i64,
    /// いま公開している版（まだ公開していなければ `null`）。
    pub published_version: Option<i64>,
    /// 公開した版から変わるところ（まだ公開していなければ、新しい版の中身を「足す」として並べる）。
    pub changes: Vec<MiniAppChange>,
    /// 公開を止める誤り（画面の書き損じ）。空なら公開できる。
    pub problems: Vec<GuiValidationError>,
}

/// その版の公開を、画面の書き損じ（[`screen_problems`]）で止めるか（D-16 の (c)・単一定義）。
///
/// 止めるのは、一度も公開していない版の公開だけ。いま公開している版（公開し直しても何も変えない）と、前に公開していた
/// 版（公開を戻す先）は止めない。前に公開していた版は使う人が使っていた版で、新しい版に誤りがあったときの逃げ道を
/// 塞がない（migration 0145 は、書き損じのある版もそのまま公開した版にしている）。
#[must_use]
pub fn problems_block_publish(version: i64, published: Option<i64>, previous: Option<i64>) -> bool {
    published != Some(version) && previous != Some(version)
}

/// 公開した版（`from`・無ければ未公開）から `to` へ変わるところ。並びは見出し・画面・スキル・ワークフロー・保存ビュー。
pub fn diff_bodies(from: Option<&MiniAppBody>, to: &MiniAppBody) -> Vec<MiniAppChange> {
    let mut out = Vec::new();
    let from_description = from.map(|b| b.description.as_str());
    if from_description != Some(to.description.as_str()) {
        out.push(MiniAppChange::Description {
            from: from_description.unwrap_or_default().to_string(),
            to: to.description.clone(),
        });
    }
    if from.map(|b| b.screen.as_str()) != Some(to.screen.as_str()) {
        out.push(MiniAppChange::Screen);
    }
    let from_skill = from.and_then(|b| b.skill);
    if from_skill != to.skill {
        out.push(MiniAppChange::Skill {
            from_version: from_skill.map(|p| p.version),
            to_version: to.skill.map(|p| p.version),
            replaced: matches!((from_skill, to.skill), (Some(a), Some(b)) if a.artifact_id != b.artifact_id),
        });
    }
    let empty: &[NamedComponentPin] = &[];
    out.extend(
        pin_changes(from.map_or(empty, |b| &b.workflows), &to.workflows)
            .into_iter()
            .map(MiniAppChange::Workflow),
    );
    out.extend(
        pin_changes(from.map_or(empty, |b| &b.data_views), &to.data_views)
            .into_iter()
            .map(MiniAppChange::DataView),
    );
    out
}

/// 名前で束ねた部品の差（同じ部品は id で対応づける。新しい版の並び、外したものは最後）。
fn pin_changes(from: &[NamedComponentPin], to: &[NamedComponentPin]) -> Vec<PinChange> {
    let mut out = Vec::new();
    for pin in to {
        match from.iter().find(|p| p.artifact_id == pin.artifact_id) {
            Some(old) if old.alias == pin.alias && old.version == pin.version => {}
            Some(old) => out.push(PinChange {
                alias: pin.alias.clone(),
                from_alias: Some(old.alias.clone()),
                from_version: Some(old.version),
                to_version: Some(pin.version),
            }),
            None => out.push(PinChange {
                alias: pin.alias.clone(),
                from_alias: None,
                from_version: None,
                to_version: Some(pin.version),
            }),
        }
    }
    for old in from {
        if !to.iter().any(|p| p.artifact_id == old.artifact_id) {
            out.push(PinChange {
                alias: old.alias.clone(),
                from_alias: Some(old.alias.clone()),
                from_version: Some(old.version),
                to_version: None,
            });
        }
    }
    out
}

impl MiniAppStore {
    /// 公開の前の確かめ（editor）: `version`（省けば最新の版）といま公開している版の差と、公開を止める誤り。
    pub async fn publish_preview(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        version: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<PublishPreview, GuiError> {
        let meta = self.require_publisher(ctx, id, trace_id).await?;
        let version = version.unwrap_or(meta.current_version);
        let target = self.version_body(ctx, id, version, trace_id).await?;
        // いま公開している版が今の検証を通らない（古い形式）なら、未公開と同じく中身を「足す」として並べる。
        let from = match meta.published_version {
            Some(p) if p == version => Some(target.clone()),
            Some(p) => self.version_body(ctx, id, p, trace_id).await.ok(),
            None => None,
        };
        let blocks = problems_block_publish(
            version,
            meta.published_version,
            meta.previous_published_version,
        );
        Ok(PublishPreview {
            version,
            published_version: meta.published_version,
            changes: diff_bodies(from.as_ref(), &target),
            problems: if blocks {
                screen_problems(&target)
            } else {
                Vec::new()
            },
        })
    }

    /// `version` を公開する（editor）。一度も公開していない版に画面の書き損じがあれば公開しない（検証の誤り・
    /// D-16 の (c)・[`problems_block_publish`]）。
    ///
    /// 前に公開していた版を選ぶと公開を戻せる（書き損じがあっても戻せる）。いま公開している版を選んでも何も変えない。
    pub async fn publish(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        version: i64,
        trace_id: Option<&str>,
    ) -> Result<Publication, GuiError> {
        let meta = self.require_publisher(ctx, id, trace_id).await?;
        let body = self.version_body(ctx, id, version, trace_id).await?;
        if problems_block_publish(
            version,
            meta.published_version,
            meta.previous_published_version,
        ) {
            let problems = screen_problems(&body);
            if !problems.is_empty() {
                return Err(GuiError::Validation(problems));
            }
        }
        Ok(self.artifacts.publish(ctx, id, version, trace_id).await?)
    }

    /// 公開できる人（アプリの editor）かを、ミニアプリであることと合わせて確かめる。
    async fn require_publisher(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<artifact::Artifact, GuiError> {
        let meta = self.artifacts.get(ctx, id, trace_id).await?;
        if meta.kind != ArtifactKind::MiniApp {
            return Err(GuiError::Artifact(artifact::ArtifactError::NotFound));
        }
        self.artifacts
            .require_relation(
                ctx,
                id,
                Relation::Editor,
                AuditAction::ArtifactPublish,
                trace_id,
            )
            .await?;
        Ok(meta)
    }

    /// 版の本文を読み、今の検証に通す（通らない版は公開できない）。
    async fn version_body(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        version: i64,
        trace_id: Option<&str>,
    ) -> Result<MiniAppBody, GuiError> {
        let v = self
            .artifacts
            .get_version(ctx, id, version, trace_id)
            .await?;
        validate_miniapp_body(&v.body).map_err(GuiError::Validation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::miniapp::ComponentPin;

    fn pin(alias: &str, n: u128, version: i64) -> NamedComponentPin {
        NamedComponentPin {
            alias: alias.into(),
            artifact_id: Uuid::from_u128(n),
            version,
        }
    }

    fn body(description: &str, screen: &str, workflows: Vec<NamedComponentPin>) -> MiniAppBody {
        MiniAppBody {
            description: description.into(),
            screen: screen.into(),
            skill: None,
            workflows,
            data_views: Vec::new(),
        }
    }

    #[test]
    fn the_same_body_has_no_changes() {
        let b = body("経費", "# 画面", vec![pin("申請", 1, 1)]);
        assert!(diff_bodies(Some(&b), &b).is_empty());
    }

    /// 見出し・画面・スキル・ワークフローの足す・外す・版と別名の変更を、id で対応づけて言う。
    #[test]
    fn changes_are_matched_by_component() {
        let mut from = body(
            "経費",
            "# 画面",
            vec![pin("申請", 1, 1), pin("取消", 2, 3), pin("集計", 3, 1)],
        );
        from.skill = Some(ComponentPin {
            artifact_id: Uuid::from_u128(9),
            version: 1,
        });
        let to = body(
            "経費の申請",
            "# 新しい画面",
            vec![pin("申請", 1, 2), pin("取り消し", 2, 3), pin("通知", 4, 1)],
        );
        let changes = diff_bodies(Some(&from), &to);
        assert_eq!(
            changes,
            vec![
                MiniAppChange::Description {
                    from: "経費".into(),
                    to: "経費の申請".into()
                },
                MiniAppChange::Screen,
                MiniAppChange::Skill {
                    from_version: Some(1),
                    to_version: None,
                    replaced: false,
                },
                MiniAppChange::Workflow(PinChange {
                    alias: "申請".into(),
                    from_alias: Some("申請".into()),
                    from_version: Some(1),
                    to_version: Some(2),
                }),
                MiniAppChange::Workflow(PinChange {
                    alias: "取り消し".into(),
                    from_alias: Some("取消".into()),
                    from_version: Some(3),
                    to_version: Some(3),
                }),
                MiniAppChange::Workflow(PinChange {
                    alias: "通知".into(),
                    from_alias: None,
                    from_version: None,
                    to_version: Some(1),
                }),
                MiniAppChange::Workflow(PinChange {
                    alias: "集計".into(),
                    from_alias: Some("集計".into()),
                    from_version: Some(1),
                    to_version: None,
                }),
            ]
        );
        // JSON の形（画面が kind で絞り込む）。
        let json = serde_json::to_value(&changes[3]).unwrap();
        assert_eq!(json["kind"], "workflow");
        assert_eq!(json["from_version"], 1);
        assert_eq!(
            serde_json::to_value(&changes[1]).unwrap(),
            serde_json::json!({ "kind": "screen" })
        );
    }

    /// 書き損じで止めるのは、一度も公開していない版だけ（公開を戻す・いまの版の公開し直しは止めない）。
    #[test]
    fn problems_block_only_versions_never_published() {
        assert!(problems_block_publish(3, None, None), "未公開のアプリ");
        assert!(problems_block_publish(4, Some(3), Some(2)), "新しい版");
        assert!(
            problems_block_publish(1, Some(3), Some(2)),
            "公開したことの分からない版"
        );
        assert!(!problems_block_publish(2, Some(3), Some(2)), "公開を戻す");
        assert!(
            !problems_block_publish(3, Some(3), Some(2)),
            "いま公開している版"
        );
    }

    /// 別のスキルに替えたことと、同じスキルの版を変えたことを言い分ける（版の番号だけでは見分けられない）。
    #[test]
    fn a_replaced_skill_is_told_from_a_new_version() {
        let skill = |n: u128, version: i64| ComponentPin {
            artifact_id: Uuid::from_u128(n),
            version,
        };
        let mut from = body("経費", "# 画面", vec![]);
        from.skill = Some(skill(9, 1));
        let mut to = from.clone();
        to.skill = Some(skill(8, 2));
        assert_eq!(
            diff_bodies(Some(&from), &to),
            vec![MiniAppChange::Skill {
                from_version: Some(1),
                to_version: Some(2),
                replaced: true,
            }]
        );
        to.skill = Some(skill(9, 2));
        assert_eq!(
            diff_bodies(Some(&from), &to),
            vec![MiniAppChange::Skill {
                from_version: Some(1),
                to_version: Some(2),
                replaced: false,
            }]
        );
        let json = serde_json::to_value(&diff_bodies(None, &to)[2]).unwrap();
        assert_eq!(json["kind"], "skill");
        assert_eq!(json["replaced"], false, "付けるのは替えるではない");
    }

    /// まだ公開していないアプリは、新しい版の中身をすべて足すものとして並べる。
    #[test]
    fn an_unpublished_app_lists_everything_as_added() {
        let to = body("経費", "# 画面", vec![pin("申請", 1, 1)]);
        let changes = diff_bodies(None, &to);
        assert_eq!(changes.len(), 3);
        assert!(matches!(&changes[0], MiniAppChange::Description { from, .. } if from.is_empty()));
        assert!(matches!(&changes[2], MiniAppChange::Workflow(p) if p.from_version.is_none()));
    }
}

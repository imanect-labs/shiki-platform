//! 使う人の権限の確かめの入口（[`AccessChecker`]・決まりと結果の形は [親のモジュール](super)）。

use std::collections::HashMap;

use artifact::{ArtifactError, ArtifactKind, ArtifactStore};
use authz::{AuthContext, AuthzClient, CheckTuple, Consistency, FgaObject, Relation, Subject};
use data::DataStore;
use storage::audit::AuditAction;
use storage::{NodeKind, ShareTarget, StorageService};
use uuid::Uuid;

use super::{
    fga_object, objects_of, relations_per_object, rows_of, status_of, AccessNeedView, AccessObject,
    AccessReport, AccessStatus, AccessTarget, MyAccessItem, MyAppAccess, MyPartAccess, NeedRow,
    PartNeeds, SubjectAccess, UncheckedBlock, MAX_ACCESS_CHECKS, MAX_ACCESS_SUBJECTS,
};
use crate::store::GuiError;
use crate::validate::GuiValidationError;

/// 求めた人に見える対象の名前（テーブル・ファイル・フォルダー）。
#[derive(Debug, Default)]
struct Labels {
    targets: HashMap<(AccessObject, Uuid), AccessTarget>,
}

/// 使う人の権限の確かめ（単一の入口・[モジュールの説明](super)）。
pub struct AccessChecker<'a> {
    pub authz: &'a dyn AuthzClient,
    pub artifacts: &'a ArtifactStore,
    pub data: &'a DataStore,
    pub storage: &'a StorageService,
}

impl AccessChecker<'_> {
    /// 作る人の確かめを求められる人か（その種類のアーティファクトの editor）。見えなければ見つからない、editor でなければ
    /// 権限なし（監査に `artifact.access_check` の deny を残す）。
    pub async fn require_maker(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        kind: ArtifactKind,
        trace_id: Option<&str>,
    ) -> Result<(), GuiError> {
        let meta = self.artifacts.get(ctx, id, trace_id).await?;
        if meta.kind != kind {
            return Err(GuiError::Artifact(ArtifactError::NotFound));
        }
        self.artifacts
            .require_relation(
                ctx,
                id,
                Relation::Editor,
                AuditAction::ArtifactAccessCheck,
                trace_id,
            )
            .await?;
        Ok(())
    }

    /// 相手ごとに確かめる相手と、上限で切ったか（`None` は相手ごとには確かめない）。
    ///
    /// 相手ごとの結果は、共有を管理する持ち主（owner）だけが求められる。`asked` を書いたのが持ち主でなければ権限なし、
    /// 省いたのが持ち主なら共有している相手（直接の共有・[`MAX_ACCESS_SUBJECTS`] まで）、持ち主でなければ `None`。
    /// `asked` に書けるのは、今共有している相手だけ（ほかの人の権限を問う口にしない・共有していない相手は 400）。
    pub async fn subjects_for(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        asked: Option<Vec<ShareTarget>>,
        trace_id: Option<&str>,
    ) -> Result<Option<(Vec<ShareTarget>, bool)>, GuiError> {
        if let Some(asked) = asked {
            if asked.len() > MAX_ACCESS_SUBJECTS {
                return Err(GuiError::Validation(vec![GuiValidationError::new(
                    "access.too_many_subjects",
                    format!("確かめる相手は {MAX_ACCESS_SUBJECTS} 件までです"),
                )
                .at("subjects")]));
            }
            for s in &asked {
                let (ShareTarget::User { id: local } | ShareTarget::Role { id: local }) = s;
                if local != local.trim() || authz::validate_local_id(local).is_err() {
                    return Err(GuiError::Validation(vec![GuiValidationError::new(
                        "access.invalid_subject",
                        "確かめる相手の指定が正しくありません",
                    )
                    .at("subjects")]));
                }
            }
            self.artifacts
                .require_relation(
                    ctx,
                    id,
                    Relation::Owner,
                    AuditAction::ArtifactAccessCheck,
                    trace_id,
                )
                .await?;
            let shared: Vec<ShareTarget> = self
                .artifacts
                .list_shares(ctx, id, trace_id)
                .await?
                .into_iter()
                .map(|(target, _)| target)
                .collect();
            let mut unique: Vec<ShareTarget> = Vec::new();
            for s in asked {
                if !shared.contains(&s) {
                    return Err(GuiError::Validation(vec![GuiValidationError::new(
                        "access.subject_not_shared",
                        "確かめられるのは、共有している相手だけです",
                    )
                    .at("subjects")]));
                }
                if !unique.contains(&s) {
                    unique.push(s);
                }
            }
            return Ok(Some((unique, false)));
        }
        let owner = self
            .authz
            .check(
                &ctx.subject(),
                Relation::Owner,
                &ctx.ns().artifact(&id.to_string()),
                Consistency::HigherConsistency,
            )
            .await
            .map_err(|e| GuiError::Internal(format!("持ち主の確かめ: {e}")))?;
        if !owner {
            return Ok(None);
        }
        let mut unique: Vec<ShareTarget> = Vec::new();
        for (target, _) in self.artifacts.list_shares(ctx, id, trace_id).await? {
            if !unique.contains(&target) {
                unique.push(target);
            }
        }
        let more = unique.len() > MAX_ACCESS_SUBJECTS;
        unique.truncate(MAX_ACCESS_SUBJECTS);
        Ok(Some((unique, more)))
    }

    /// `objects` のうち、`ctx` に見えるものの名前（テーブルは閲覧か追加だけ、ファイル・フォルダーは閲覧）。
    async fn labels(
        &self,
        ctx: &AuthContext,
        objects: &[(AccessObject, Uuid)],
    ) -> Result<Labels, GuiError> {
        let tables: Vec<Uuid> = objects
            .iter()
            .filter(|(o, _)| *o == AccessObject::Table)
            .map(|(_, id)| *id)
            .collect();
        let nodes: Vec<(Uuid, NodeKind)> = objects
            .iter()
            .filter_map(|(o, id)| match o {
                AccessObject::File => Some((*id, NodeKind::File)),
                AccessObject::Folder => Some((*id, NodeKind::Folder)),
                AccessObject::Table => None,
            })
            .collect();
        let tables = self
            .data
            .table_labels(ctx, &tables)
            .await
            .map_err(|e| GuiError::Internal(format!("テーブルの名前: {e}")))?;
        let nodes = self
            .storage
            .node_labels(ctx, &nodes)
            .await
            .map_err(|e| GuiError::Internal(format!("ファイルの名前: {e}")))?;
        let mut out = Labels::default();
        for (id, t) in tables {
            out.targets.insert(
                (AccessObject::Table, id),
                AccessTarget {
                    id,
                    name: t.name,
                    app_id: t.app_id,
                    content_type: None,
                },
            );
        }
        for (id, n) in nodes {
            let object = match n.kind {
                NodeKind::File => AccessObject::File,
                NodeKind::Folder => AccessObject::Folder,
            };
            out.targets.insert(
                (object, id),
                AccessTarget {
                    id,
                    name: n.name,
                    app_id: None,
                    content_type: n.content_type,
                },
            );
        }
        Ok(out)
    }

    /// `subjects` それぞれが `objects` に持つ relation（`asks` は対象ごとに問う relation）。
    /// 戻りは相手ごと・対象ごとの持っている relation。
    async fn held(
        &self,
        subjects: &[Subject],
        fga: &[FgaObject],
        asks: &[Vec<Relation>],
    ) -> Result<Vec<Vec<Vec<Relation>>>, GuiError> {
        let mut tuples = Vec::new();
        for subject in subjects {
            for (object, rels) in fga.iter().zip(asks) {
                for relation in rels {
                    tuples.push(CheckTuple {
                        subject: subject.clone(),
                        relation: *relation,
                        object: object.clone(),
                    });
                }
            }
        }
        let got = if tuples.is_empty() {
            Vec::new()
        } else {
            self.authz
                .batch_check(&tuples, Consistency::HigherConsistency)
                .await
                .map_err(|e| GuiError::Internal(format!("権限の確かめ: {e}")))?
        };
        if got.len() != tuples.len() {
            return Err(GuiError::Internal(
                "権限の確かめの応答の数が問いの数と一致しません".into(),
            ));
        }
        // 問いを並べた順（相手 → 対象 → relation）に答えを戻す。
        let mut answers = got.into_iter();
        let mut out = Vec::with_capacity(subjects.len());
        for _ in subjects {
            let mut per_object = Vec::with_capacity(asks.len());
            for rels in asks {
                let mut have = Vec::new();
                for relation in rels {
                    if answers.next() == Some(true) {
                        have.push(*relation);
                    }
                }
                per_object.push(have);
            }
            out.push(per_object);
        }
        Ok(out)
    }

    /// 部署の相手に、組織全体（organization#member）へ付けたファイル・フォルダーの共有を足す。
    ///
    /// OpenFGA は部署の userset（role#member）を相手にすると、組織全体に付けた閲覧・編集を一致させない。部署の人は
    /// 組織の人なので、組織全体に共有したファイル・フォルダーは部署も使える（要らない共有を勧めない）。テーブルには
    /// 組織全体の共有が無い。
    async fn add_org_wide_to_roles(
        &self,
        ctx: &AuthContext,
        subjects: &[ShareTarget],
        objects: &[(AccessObject, Uuid)],
        asks: &[Vec<Relation>],
        held: &mut [Vec<Vec<Relation>>],
    ) -> Result<(), GuiError> {
        let roles: Vec<usize> = subjects
            .iter()
            .enumerate()
            .filter(|(_, s)| matches!(s, ShareTarget::Role { .. }))
            .map(|(i, _)| i)
            .collect();
        let nodes: Vec<usize> = objects
            .iter()
            .enumerate()
            .filter(|(_, (o, _))| *o != AccessObject::Table)
            .map(|(i, _)| i)
            .collect();
        if roles.is_empty() || nodes.is_empty() {
            return Ok(());
        }
        let fga: Vec<FgaObject> = nodes
            .iter()
            .map(|&i| fga_object(ctx, objects[i].0, objects[i].1))
            .collect();
        let node_asks: Vec<Vec<Relation>> = nodes.iter().map(|&i| asks[i].clone()).collect();
        let org = ctx.ns().organization_member(&ctx.org);
        let org_held = self
            .held(&[org], &fga, &node_asks)
            .await?
            .into_iter()
            .next()
            .unwrap_or_default();
        for i in roles {
            let Some(per_object) = held.get_mut(i) else {
                continue;
            };
            for (at, extra) in nodes.iter().zip(&org_held) {
                for rel in extra {
                    if !per_object[*at].contains(rel) {
                        per_object[*at].push(*rel);
                    }
                }
            }
        }
        Ok(())
    }

    /// 作る人の確かめ: 部品が要る権限と、`subjects`（共有の相手）ごとに足りるか。
    ///
    /// `subjects` が `None` なら相手ごとには確かめない（共有を管理できない人）。相手が [`MAX_ACCESS_SUBJECTS`] を
    /// 超えるときは呼び出し側が切って `more_subjects` を立てる。相手の権限を確かめるのは、`ctx` に見えて、`ctx` が
    /// 持ち主の対象だけ（[`AccessNeedView::manageable`]）。
    pub async fn report(
        &self,
        ctx: &AuthContext,
        version: i64,
        parts: &[PartNeeds],
        subjects: Option<&[ShareTarget]>,
    ) -> Result<AccessReport, GuiError> {
        let rows = rows_of(parts);
        let (objects, mut truncated) = objects_of(&rows);
        let labels = self.labels(ctx, &objects).await?;
        // 相手の権限を問うのは、求めた人に見えて、共有を管理している（持ち主の）対象だけ。
        let visible: Vec<(AccessObject, Uuid)> = objects
            .iter()
            .copied()
            .filter(|k| labels.targets.contains_key(k))
            .collect();
        let owner_asks = vec![vec![Relation::Owner]; visible.len()];
        let visible_fga: Vec<FgaObject> = visible
            .iter()
            .map(|(o, id)| fga_object(ctx, *o, *id))
            .collect();
        let owned = self
            .held(&[ctx.subject()], &visible_fga, &owner_asks)
            .await?
            .into_iter()
            .next()
            .unwrap_or_default();
        let manageable: Vec<(AccessObject, Uuid)> = visible
            .iter()
            .zip(&owned)
            .filter(|(_, held)| held.contains(&Relation::Owner))
            .map(|(k, _)| *k)
            .collect();
        let needs: Vec<AccessNeedView> = rows
            .iter()
            .map(|r| AccessNeedView {
                part: r.part.clone(),
                object: r.object,
                need: r.kind,
                target: r
                    .id
                    .and_then(|id| labels.targets.get(&(r.object, id)).cloned()),
                dynamic: r.id.is_none(),
                manageable: r.id.is_some_and(|id| manageable.contains(&(r.object, id))),
                conditional: r.conditional,
                over_limit: r.id.is_some_and(|id| !objects.contains(&(r.object, id))),
            })
            .collect();
        let unavailable = parts
            .iter()
            .filter(|p| p.unavailable)
            .filter_map(|p| p.part.clone())
            .collect();
        let unchecked = unchecked_blocks(parts);
        let Some(subjects) = subjects else {
            return Ok(AccessReport {
                version,
                needs,
                subjects: None,
                more_subjects: false,
                truncated,
                unavailable,
                unchecked,
            });
        };
        let asks = relations_per_object(&rows, &manageable);
        let per_subject: usize = asks.iter().map(Vec::len).sum();
        let checked = MAX_ACCESS_CHECKS
            .checked_div(per_subject)
            .map_or(subjects.len(), |n| n.min(subjects.len()));
        if checked < subjects.len() {
            truncated = true;
        }
        let fga: Vec<FgaObject> = manageable
            .iter()
            .map(|(o, id)| fga_object(ctx, *o, *id))
            .collect();
        let ns = ctx.ns();
        let fga_subjects: Vec<Subject> =
            subjects[..checked].iter().map(|s| s.subject(&ns)).collect();
        let mut held = self.held(&fga_subjects, &fga, &asks).await?;
        self.add_org_wide_to_roles(ctx, &subjects[..checked], &manageable, &asks, &mut held)
            .await?;
        let results = subjects
            .iter()
            .enumerate()
            .map(|(i, subject)| SubjectAccess {
                subject: subject.clone(),
                display_name: None,
                statuses: rows
                    .iter()
                    .map(|r| {
                        let at = r
                            .id
                            .and_then(|id| manageable.iter().position(|k| *k == (r.object, id)));
                        match (held.get(i), at) {
                            (Some(h), Some(at)) => status_of(r.object, r.kind, &h[at]),
                            _ => AccessStatus::Unchecked,
                        }
                    })
                    .collect(),
            })
            .collect();
        Ok(AccessReport {
            version,
            needs,
            subjects: Some(results),
            more_subjects: false,
            truncated,
            unavailable,
            unchecked,
        })
    }

    /// 使う人の確かめ: 自分に足りない権限を部品ごとに返す（見えない対象は名前を出さない）。
    ///
    /// 問うのは必ず要る権限だけ（場合によって要る権限ではボタンを止めない・BUG-216）。
    pub async fn mine(
        &self,
        ctx: &AuthContext,
        version: i64,
        parts: &[PartNeeds],
    ) -> Result<MyAppAccess, GuiError> {
        let mut rows = rows_of(parts);
        rows.retain(|r| !r.conditional);
        let (objects, truncated) = objects_of(&rows);
        let asks = relations_per_object(&rows, &objects);
        let fga: Vec<FgaObject> = objects
            .iter()
            .map(|(o, id)| fga_object(ctx, *o, *id))
            .collect();
        let held = self.held(&[ctx.subject()], &fga, &asks).await?;
        let held = held.into_iter().next().unwrap_or_default();
        let status = |r: &NeedRow| {
            r.id.and_then(|id| objects.iter().position(|k| *k == (r.object, id)))
                .and_then(|at| held.get(at))
                .map(|h| status_of(r.object, r.kind, h))
        };
        // 名前は足りない・範囲が狭い対象のうち、自分に見えるものだけ（見えるかは名前を引く口が決める）。
        let named: Vec<(AccessObject, Uuid)> = objects
            .iter()
            .copied()
            .filter(|(o, id)| {
                rows.iter().any(|r| {
                    r.object == *o
                        && r.id == Some(*id)
                        && matches!(
                            status(r),
                            Some(AccessStatus::Missing | AccessStatus::Limited)
                        )
                })
            })
            .collect();
        let labels = self.labels(ctx, &named).await?;
        let mut out: Vec<MyPartAccess> = Vec::new();
        for p in parts {
            let Some(part) = p.part.clone() else { continue };
            if p.unavailable {
                out.push(MyPartAccess {
                    part,
                    status: AccessStatus::Missing,
                    unavailable: true,
                    items: Vec::new(),
                });
                continue;
            }
            let mut items: Vec<MyAccessItem> = Vec::new();
            for r in rows.iter().filter(|r| r.part.as_ref() == Some(&part)) {
                let Some(s @ (AccessStatus::Missing | AccessStatus::Limited)) = status(r) else {
                    continue;
                };
                let item = MyAccessItem {
                    object: r.object,
                    need: r.kind,
                    status: s,
                    name: r
                        .id
                        .and_then(|id| labels.targets.get(&(r.object, id)))
                        .map(|t| t.name.clone()),
                };
                if !items.contains(&item) {
                    items.push(item);
                }
            }
            if let Some(worst) = items.iter().map(|i| i.status).max() {
                out.push(MyPartAccess {
                    part,
                    status: worst,
                    unavailable: false,
                    items,
                });
            }
        }
        Ok(MyAppAccess {
            version,
            parts: out,
            truncated,
        })
    }
}

/// 部品をまたいで、前もって確かめられないブロック（部品の順・同じものは 1 つ・GAP-82）。
fn unchecked_blocks(parts: &[PartNeeds]) -> Vec<UncheckedBlock> {
    let mut out: Vec<UncheckedBlock> = Vec::new();
    for p in parts {
        for (block, kind) in &p.unchecked {
            let item = UncheckedBlock {
                part: p.part.clone(),
                block: block.clone(),
                kind: *kind,
            };
            if !out.contains(&item) {
                out.push(item);
            }
        }
    }
    out
}

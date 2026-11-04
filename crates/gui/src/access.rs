//! アプリとワークフローを使う人の権限の確かめ（GAP-37）。
//!
//! アプリのボタン・画面の表と、共有されたワークフローの実行は、使う人本人の ReBAC で動く（miniapp-platform §7）。
//! アプリを共有しても、書き先のテーブルなどの権限が足りなければ、開いた後か実行の失敗で初めて分かる。そこで、
//!
//! - **作る人**（アプリ・ワークフローの editor）には、部品が要る権限の一覧と、共有の相手（人・部署）ごとに足りるか
//!   （[`AccessChecker::report`]）を返す。相手ごとの結果は、共有を管理する持ち主だけが求められる（呼び出し側が確かめる）。
//! - **使う人**には、自分に足りない権限を部品（ボタンの実行先・画面の表・スキル）ごとに返す（[`AccessChecker::mine`]）。
//!   画面は押す前にボタンを止め、理由を出す。
//!
//! # 守ること
//!
//! - **判定は OpenFGA の BatchCheck だけ**（[`AuthzClient::batch_check`]・単一のチョークポイント）。ここは「足りるか」を
//!   前もって言うだけで、実際の読み書きは今までどおり本人の権限で二段の認可を通る（この確かめが誤っても権限は広がらない）。
//! - **見えない部品の名前を出さない。** 対象の名前は、求めた人に見える（テーブルは閲覧か追加だけ、ファイル・フォルダーは
//!   閲覧）ものだけを返す（[`DataStore::table_labels`]・[`StorageService::node_labels`]）。作る人にも、自分に見えない対象は
//!   名前を出さず、相手の権限も確かめない（見えない対象について他人の権限を問う口にしない）。使う人には自分に見えるもの
//!   だけを名前で言い、id は返さない。
//! - **問う数に上限を置く。** 相手は [`MAX_ACCESS_SUBJECTS`]、対象は [`MAX_ACCESS_OBJECTS`]、相手ごとの問いの合計は
//!   [`MAX_ACCESS_CHECKS`] まで。超えた分は確かめずに `unchecked` と `truncated` で返す（黙って「足りる」と言わない）。
//!
//! # 足りるかの決まり（単一の定義）
//!
//! 要る操作（[`AccessKind`]）ごとに、足りる relation と、範囲が狭くなる relation を [`relations_for`] が決める。
//! テーブルの「追加だけ」（submitter）は編集（editor）が含意する（authorization-model.fga）ので、追加は submitter を
//! 問えば足りる。読む・書き換えるで追加だけしか無いときは、自分が追加したレコードだけになる（`limited`・D-113・D-127）。

use std::collections::HashMap;

use artifact::{ArtifactError, ArtifactKind, ArtifactStore};
use authz::{AuthContext, AuthzClient, CheckTuple, Consistency, FgaObject, Relation, Subject};
use data::DataStore;
use serde::Serialize;
use storage::audit::AuditAction;
use storage::{NodeKind, ShareTarget, StorageService};
use utoipa::ToSchema;
use uuid::Uuid;
pub use workflow_engine::{AccessKind, AccessObject};

use crate::store::GuiError;
use crate::validate::GuiValidationError;

/// 一度に確かめる相手（人・部署）の数の上限。
pub const MAX_ACCESS_SUBJECTS: usize = 20;
/// 一度に確かめる対象（テーブル・ファイル・フォルダー）の数の上限。
pub const MAX_ACCESS_OBJECTS: usize = 100;
/// 相手ごとの問い（相手 × 対象 × relation）の合計の上限（OpenFGA の BatchCheck は 50 問ずつ 4 本並べて送る）。
pub const MAX_ACCESS_CHECKS: usize = 1_000;

/// 権限を確かめた部品。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AccessPart {
    /// 束ねたワークフロー（画面のボタンの実行先）。
    Workflow { alias: String },
    /// 束ねた保存ビュー（画面の表）。
    DataView { alias: String },
    /// 束ねたスキル（このアプリについて AI に聞く）。
    Skill,
}

/// 部品 1 つが要る権限（読めなかった部品は `unavailable`）。ワークフローそのものを確かめるときは `part` が `None`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartNeeds {
    pub part: Option<AccessPart>,
    /// 要る権限（対象の種類・対象・操作）。対象が `None` は実行のときに決まる（確かめない）。
    pub needs: Vec<(AccessObject, Option<Uuid>, AccessKind)>,
    /// 部品を読めなかった（消えた・本文が壊れている）。
    pub unavailable: bool,
}

/// 確かめた対象（求めた人に見えるときだけ返す）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct AccessTarget {
    pub id: Uuid,
    pub name: String,
    /// インストールしたアプリが持つテーブル（テーブルの共有ではなく、アプリの「使える部署・グループ」で渡す・GAP-68）。
    pub app_id: Option<Uuid>,
    /// ファイルの内容の種類（開く画面を決める）。
    pub content_type: Option<String>,
}

/// 部品が要る権限 1 つ（作る人向け）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct AccessNeedView {
    /// 部品（ワークフローそのものを確かめたときは `null`）。
    pub part: Option<AccessPart>,
    pub object: AccessObject,
    pub need: AccessKind,
    /// 対象（求めた人に見えるときだけ）。見えない・実行のときに決まるときは `null`。
    pub target: Option<AccessTarget>,
    /// 対象が実行のときに決まる（入力や前の結果で決まる・まだ選んでいない）。確かめない。
    pub dynamic: bool,
}

/// 足りるか。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccessStatus {
    /// 足りている。
    Ok,
    /// 足りているが、追加だけの共有なので、自分が追加したレコードだけ読める・直せる。
    Limited,
    /// 確かめていない（実行のときに決まる・求めた人に見えない・問う数の上限）。
    Unchecked,
    /// 足りない。
    Missing,
}

/// 相手 1 件の結果（作る人向け）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct SubjectAccess {
    pub subject: ShareTarget,
    /// 表示名（引けたときだけ）。
    pub display_name: Option<String>,
    /// `needs` と同じ並びの結果。
    pub statuses: Vec<AccessStatus>,
}

/// 作る人向けの確かめの結果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct AccessReport {
    /// 確かめた版。
    pub version: i64,
    /// 部品が要る権限（部品の順・同じものは 1 つ）。
    pub needs: Vec<AccessNeedView>,
    /// 相手ごとの結果（共有を管理する持ち主だけ。ほかの人には `null`）。
    pub subjects: Option<Vec<SubjectAccess>>,
    /// 共有している相手が上限より多く、確かめていない相手がいる。
    pub more_subjects: bool,
    /// 対象か問いが上限より多く、確かめていないものがある。
    pub truncated: bool,
    /// 読めなかった部品（消えた・本文が壊れている）。
    pub unavailable: Vec<AccessPart>,
}

/// 使う人に足りないもの 1 つ。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct MyAccessItem {
    pub object: AccessObject,
    pub need: AccessKind,
    /// `missing`（足りない）か `limited`（自分が追加したレコードだけ）。
    pub status: AccessStatus,
    /// 対象の名前（あなたに見えるときだけ）。
    pub name: Option<String>,
}

/// 使う人の部品 1 つの結果（足りない・範囲が狭い・使えない部品だけを返す）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct MyPartAccess {
    pub part: AccessPart,
    /// 部品の中でいちばん悪い結果（`missing` か `limited`）。
    pub status: AccessStatus,
    /// 部品を読めなかった（消えた・本文が壊れている）。
    pub unavailable: bool,
    pub items: Vec<MyAccessItem>,
}

/// 使う人向けの確かめの結果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct MyAppAccess {
    /// 確かめた版。
    pub version: i64,
    /// 足りない・範囲が狭い・使えない部品（すべて足りていれば空）。
    pub parts: Vec<MyPartAccess>,
    /// 対象が上限より多く、確かめていないものがある。
    pub truncated: bool,
}

/// 要る操作ごとの、足りる relation と、範囲が狭くなる relation（[モジュールの説明](self)）。
#[must_use]
pub fn relations_for(
    object: AccessObject,
    kind: AccessKind,
) -> (&'static [Relation], &'static [Relation]) {
    match (object, kind) {
        (AccessObject::Table, AccessKind::Read) => (&[Relation::Viewer], &[Relation::Submitter]),
        // 編集は追加だけを含意する（submitter = [user, role#member] or editor）。
        (AccessObject::Table, AccessKind::Add) => (&[Relation::Submitter], &[]),
        (AccessObject::Table, AccessKind::Change) => (&[Relation::Editor], &[Relation::Submitter]),
        (AccessObject::File | AccessObject::Folder, AccessKind::Read) => (&[Relation::Viewer], &[]),
        // テーブルの削除・追加か更新・まとめて登録、ファイル・フォルダーへの書き込みは編集だけ。
        (AccessObject::Table, AccessKind::Edit)
        | (AccessObject::File | AccessObject::Folder, _) => (&[Relation::Editor], &[]),
    }
}

/// 持っている relation から結果を決める（`held` に入っていない relation は持っていない）。
#[must_use]
pub fn status_of(object: AccessObject, kind: AccessKind, held: &[Relation]) -> AccessStatus {
    let (full, limited) = relations_for(object, kind);
    if full.iter().any(|r| held.contains(r)) {
        AccessStatus::Ok
    } else if limited.iter().any(|r| held.contains(r)) {
        AccessStatus::Limited
    } else {
        AccessStatus::Missing
    }
}

/// 部品をまたいで同じものを 1 つにした、要る権限の行。
#[derive(Debug, Clone, PartialEq, Eq)]
struct NeedRow {
    part: Option<AccessPart>,
    object: AccessObject,
    id: Option<Uuid>,
    kind: AccessKind,
}

fn rows_of(parts: &[PartNeeds]) -> Vec<NeedRow> {
    let mut rows: Vec<NeedRow> = Vec::new();
    for p in parts {
        for (object, id, kind) in &p.needs {
            let row = NeedRow {
                part: p.part.clone(),
                object: *object,
                id: *id,
                kind: *kind,
            };
            if !rows.contains(&row) {
                rows.push(row);
            }
        }
    }
    rows
}

/// 確かめる対象（種類と id・並びは最初に出た順・上限まで）と、上限で切ったか。
fn objects_of(rows: &[NeedRow]) -> (Vec<(AccessObject, Uuid)>, bool) {
    let mut out: Vec<(AccessObject, Uuid)> = Vec::new();
    let mut cut = false;
    for r in rows {
        let Some(id) = r.id else { continue };
        if out.contains(&(r.object, id)) {
            continue;
        }
        if out.len() >= MAX_ACCESS_OBJECTS {
            cut = true;
            continue;
        }
        out.push((r.object, id));
    }
    (out, cut)
}

/// 対象ごとに問う relation（その対象の行の、足りる relation と範囲が狭くなる relation の和・重複なし）。
fn relations_per_object(rows: &[NeedRow], objects: &[(AccessObject, Uuid)]) -> Vec<Vec<Relation>> {
    objects
        .iter()
        .map(|(object, id)| {
            let mut rels: Vec<Relation> = Vec::new();
            for r in rows
                .iter()
                .filter(|r| r.object == *object && r.id == Some(*id))
            {
                let (full, limited) = relations_for(r.object, r.kind);
                for rel in full.iter().chain(limited) {
                    if !rels.contains(rel) {
                        rels.push(*rel);
                    }
                }
            }
            rels
        })
        .collect()
}

fn fga_object(ctx: &AuthContext, object: AccessObject, id: Uuid) -> FgaObject {
    let ns = ctx.ns();
    let id = id.to_string();
    match object {
        AccessObject::Table => ns.data_table(&id),
        AccessObject::File => ns.file(&id),
        AccessObject::Folder => ns.folder(&id),
    }
}

/// 求めた人に見える対象の名前（テーブル・ファイル・フォルダー）。
#[derive(Debug, Default)]
struct Labels {
    targets: HashMap<(AccessObject, Uuid), AccessTarget>,
}

/// 使う人の権限の確かめ（単一の入口・[モジュールの説明](self)）。
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
            let mut unique: Vec<ShareTarget> = Vec::new();
            for s in asked {
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

    /// 作る人の確かめ: 部品が要る権限と、`subjects`（共有の相手）ごとに足りるか。
    ///
    /// `subjects` が `None` なら相手ごとには確かめない（共有を管理できない人）。相手が [`MAX_ACCESS_SUBJECTS`] を
    /// 超えるときは呼び出し側が切って `more_subjects` を立てる。相手の権限を確かめるのは、`ctx` に見える対象だけ。
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
            })
            .collect();
        let unavailable = parts
            .iter()
            .filter(|p| p.unavailable)
            .filter_map(|p| p.part.clone())
            .collect();
        let Some(subjects) = subjects else {
            return Ok(AccessReport {
                version,
                needs,
                subjects: None,
                more_subjects: false,
                truncated,
                unavailable,
            });
        };
        // 相手の権限を問うのは、求めた人に見える対象だけ。
        let visible: Vec<(AccessObject, Uuid)> = objects
            .iter()
            .copied()
            .filter(|k| labels.targets.contains_key(k))
            .collect();
        let asks = relations_per_object(&rows, &visible);
        let per_subject: usize = asks.iter().map(Vec::len).sum();
        let checked = MAX_ACCESS_CHECKS
            .checked_div(per_subject)
            .map_or(subjects.len(), |n| n.min(subjects.len()));
        if checked < subjects.len() {
            truncated = true;
        }
        let fga: Vec<FgaObject> = visible
            .iter()
            .map(|(o, id)| fga_object(ctx, *o, *id))
            .collect();
        let ns = ctx.ns();
        let fga_subjects: Vec<Subject> =
            subjects[..checked].iter().map(|s| s.subject(&ns)).collect();
        let held = self.held(&fga_subjects, &fga, &asks).await?;
        let results = subjects
            .iter()
            .enumerate()
            .map(|(i, subject)| SubjectAccess {
                subject: subject.clone(),
                display_name: None,
                statuses: rows
                    .iter()
                    .map(|r| {
                        let at =
                            r.id.and_then(|id| visible.iter().position(|k| *k == (r.object, id)));
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
        })
    }

    /// 使う人の確かめ: 自分に足りない権限を部品ごとに返す（見えない対象は名前を出さない）。
    pub async fn mine(
        &self,
        ctx: &AuthContext,
        version: i64,
        parts: &[PartNeeds],
    ) -> Result<MyAppAccess, GuiError> {
        let rows = rows_of(parts);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// 追加は追加だけでも足り、読む・書き換えるは追加だけだと範囲が狭い。削除などは編集が要る。ファイル・フォルダーは
    /// 閲覧と編集だけ（D-113・D-127）。
    #[test]
    fn status_follows_the_table_sharing_roles() {
        use AccessKind::*;
        use AccessObject::*;
        use AccessStatus as S;
        let sub = [Relation::Submitter];
        let view = [Relation::Viewer];
        let edit_and_sub = [Relation::Editor, Relation::Submitter];
        assert_eq!(status_of(Table, Add, &sub), S::Ok);
        assert_eq!(status_of(Table, Read, &sub), S::Limited);
        assert_eq!(status_of(Table, Change, &sub), S::Limited);
        assert_eq!(status_of(Table, Edit, &sub), S::Missing);
        assert_eq!(status_of(Table, Read, &view), S::Ok);
        assert_eq!(status_of(Table, Add, &view), S::Missing);
        assert_eq!(status_of(Table, Edit, &edit_and_sub), S::Ok);
        assert_eq!(status_of(File, Read, &view), S::Ok);
        assert_eq!(status_of(Folder, Edit, &view), S::Missing);
        assert_eq!(status_of(Folder, Edit, &[Relation::Editor]), S::Ok);
        assert_eq!(status_of(File, Read, &[]), S::Missing);
    }

    /// 同じ部品の同じものは 1 つにし、対象は上限で切る（切ったことを返す）。問う relation は対象ごとの和。
    #[test]
    fn rows_objects_and_asks_are_deduped_and_capped() {
        let wf = Some(AccessPart::Workflow { alias: "a".into() });
        let parts = vec![
            PartNeeds {
                part: wf.clone(),
                needs: vec![
                    (AccessObject::Table, Some(id(1)), AccessKind::Add),
                    (AccessObject::Table, Some(id(1)), AccessKind::Add),
                    (AccessObject::Table, Some(id(1)), AccessKind::Change),
                    (AccessObject::Table, None, AccessKind::Read),
                ],
                unavailable: false,
            },
            PartNeeds {
                part: Some(AccessPart::DataView { alias: "v".into() }),
                needs: vec![(AccessObject::Table, Some(id(1)), AccessKind::Read)],
                unavailable: false,
            },
        ];
        let rows = rows_of(&parts);
        assert_eq!(rows.len(), 4);
        let (objects, cut) = objects_of(&rows);
        assert_eq!(objects, vec![(AccessObject::Table, id(1))]);
        assert!(!cut);
        assert_eq!(
            relations_per_object(&rows, &objects),
            vec![vec![
                Relation::Submitter,
                Relation::Editor,
                Relation::Viewer
            ]]
        );
        let many = PartNeeds {
            part: wf,
            needs: (0..=MAX_ACCESS_OBJECTS as u128)
                .map(|n| (AccessObject::File, Some(id(n + 10)), AccessKind::Read))
                .collect(),
            unavailable: false,
        };
        let (objects, cut) = objects_of(&rows_of(&[many]));
        assert_eq!(objects.len(), MAX_ACCESS_OBJECTS);
        assert!(cut, "上限を超えた対象は確かめずに切ったと言う");
    }
}

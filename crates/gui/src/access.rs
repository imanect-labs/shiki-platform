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
//!   名前を出さない。使う人には自分に見えるものだけを名前で言い、id は返さない。
//! - **他人の権限は、求めた人が共有を管理している対象についてだけ問う。** 相手ごとの結果を返すのは、求めた人がその
//!   対象の持ち主（テーブル・ファイル・フォルダーの owner。共有の一覧を読めるのも持ち主だけ・`list_table_shares`・
//!   `authorize_share_admin`）のときだけで、ほかの対象は `unchecked`（[`AccessNeedView::manageable`] が偽）。見えるだけ・
//!   追加だけの人が、この口で他人の共有を問えないようにする。相手も、アーティファクトを今共有している相手だけ
//!   （[`AccessChecker::subjects_for`]）。
//! - **問う数に上限を置く。** 相手は [`MAX_ACCESS_SUBJECTS`]、対象は [`MAX_ACCESS_OBJECTS`]、相手ごとの問いの合計は
//!   [`MAX_ACCESS_CHECKS`] まで。超えた分は確かめずに `unchecked` と `truncated` で返す（黙って「足りる」と言わない）。
//!
//! # 足りるかの決まり（単一の定義）
//!
//! 要る操作（[`AccessKind`]）ごとに、足りる relation と、範囲が狭くなる relation を [`relations_for`] が決める。
//! テーブルの「追加だけ」（submitter）は編集（editor）が含意する（authorization-model.fga）ので、追加は submitter を
//! 問えば足りる。読む・書き換えるで追加だけしか無いときは、自分が追加したレコードだけになる（`limited`・D-113・D-127）。
//!
//! # 必ず要る権限と、場合によって要る権限（BUG-216）
//!
//! 束ねたワークフローの要る権限には、道によって通るブロックだけが要るか（[`PartNeed::conditional`]・決まりは
//! `workflow_engine::ir::paths` の 1 か所）を添える。使う人の確かめ（[`AccessChecker::mine`]）は必ず要る権限だけを
//! 問い、場合によって要る権限ではボタンを止めない（通らない入力でも押せなくしない）。作る人の確かめは両方を問い、
//! 「場合によって要る」と言い分ける（[`AccessNeedView::conditional`]）。

use authz::{AuthContext, FgaObject, Relation};
use serde::Serialize;
use storage::ShareTarget;
use utoipa::ToSchema;
use uuid::Uuid;
pub use workflow_engine::{AccessKind, AccessObject};

mod checker;
pub use checker::AccessChecker;

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

/// 部品が要る権限 1 つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartNeed {
    pub object: AccessObject,
    /// 対象。`None` は実行のときに決まる（確かめない）。
    pub id: Option<Uuid>,
    pub kind: AccessKind,
    /// 道によって通るブロックだけが要る（分かれた流れ・「エラー時」の出口の先・繰り返しの中・BUG-216）。使う人の
    /// ボタンは、これだけでは止めない。
    pub conditional: bool,
}

impl PartNeed {
    /// 部品を使うたびに要る権限（画面の表・スキルの参照）。
    #[must_use]
    pub const fn always(object: AccessObject, id: Option<Uuid>, kind: AccessKind) -> Self {
        PartNeed {
            object,
            id,
            kind,
            conditional: false,
        }
    }
}

/// 部品 1 つが要る権限（読めなかった部品は `unavailable`）。ワークフローそのものを確かめるときは `part` が `None`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartNeeds {
    pub part: Option<AccessPart>,
    /// 要る権限（並びはブロックの順）。
    pub needs: Vec<PartNeed>,
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
    /// 求めた人がこの対象の共有を管理している（持ち主）。相手ごとの権限は、これが真の対象だけ確かめる
    /// （ほかは `unchecked`。対象の持ち主に確かめてもらう）。
    pub manageable: bool,
    /// 場合によって要る（道によって通るブロックだけが要る・BUG-216）。足りなくても使う人のボタンは止めない。
    pub conditional: bool,
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

/// 部品ごとに同じものを 1 つにした、要る権限の行。
#[derive(Debug, Clone, PartialEq, Eq)]
struct NeedRow {
    part: Option<AccessPart>,
    object: AccessObject,
    id: Option<Uuid>,
    kind: AccessKind,
    /// 部品の中のどのブロックでも、道によって通るときだけ要る（1 つでも必ず通るブロックが要れば偽）。
    conditional: bool,
}

impl NeedRow {
    fn same(&self, part: Option<&AccessPart>, n: &PartNeed) -> bool {
        self.part.as_ref() == part
            && self.object == n.object
            && self.id == n.id
            && self.kind == n.kind
    }
}

fn rows_of(parts: &[PartNeeds]) -> Vec<NeedRow> {
    let mut rows: Vec<NeedRow> = Vec::new();
    for p in parts {
        for n in &p.needs {
            if let Some(row) = rows.iter_mut().find(|r| r.same(p.part.as_ref(), n)) {
                row.conditional &= n.conditional;
                continue;
            }
            rows.push(NeedRow {
                part: p.part.clone(),
                object: n.object,
                id: n.id,
                kind: n.kind,
                conditional: n.conditional,
            });
        }
    }
    rows
}

/// 確かめる対象（種類と id・上限まで）と、上限で切ったか。並びは、必ず要る行の対象を先に、最初に出た順
/// （上限で切るときに、ボタンを止める材料になる対象を残す）。
fn objects_of(rows: &[NeedRow]) -> (Vec<(AccessObject, Uuid)>, bool) {
    let mut out: Vec<(AccessObject, Uuid)> = Vec::new();
    let mut cut = false;
    let always = rows.iter().filter(|r| !r.conditional);
    let conditional = rows.iter().filter(|r| r.conditional);
    for r in always.chain(conditional) {
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
        let need = PartNeed::always;
        let parts = vec![
            PartNeeds {
                part: wf.clone(),
                needs: vec![
                    need(AccessObject::Table, Some(id(1)), AccessKind::Add),
                    need(AccessObject::Table, Some(id(1)), AccessKind::Add),
                    need(AccessObject::Table, Some(id(1)), AccessKind::Change),
                    need(AccessObject::Table, None, AccessKind::Read),
                ],
                unavailable: false,
            },
            PartNeeds {
                part: Some(AccessPart::DataView { alias: "v".into() }),
                needs: vec![need(AccessObject::Table, Some(id(1)), AccessKind::Read)],
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
                .map(|n| need(AccessObject::File, Some(id(n + 10)), AccessKind::Read))
                .collect(),
            unavailable: false,
        };
        let (objects, cut) = objects_of(&rows_of(&[many]));
        assert_eq!(objects.len(), MAX_ACCESS_OBJECTS);
        assert!(cut, "上限を超えた対象は確かめずに切ったと言う");
    }

    /// 同じ部品の同じ要る権限は、1 つでも必ず通るブロックが要れば必ず要る（BUG-216）。上限で切るときは、必ず要る
    /// 行の対象を先に残す。
    #[test]
    fn conditional_rows_merge_and_yield_to_sure_ones() {
        let wf = Some(AccessPart::Workflow { alias: "a".into() });
        let maybe = |object, n, kind| PartNeed {
            conditional: true,
            ..PartNeed::always(object, Some(id(n)), kind)
        };
        let rows = rows_of(&[PartNeeds {
            part: wf.clone(),
            needs: vec![
                maybe(AccessObject::Table, 1, AccessKind::Add),
                PartNeed::always(AccessObject::Table, Some(id(1)), AccessKind::Add),
                maybe(AccessObject::Table, 2, AccessKind::Edit),
            ],
            unavailable: false,
        }]);
        let flags: Vec<bool> = rows.iter().map(|r| r.conditional).collect();
        assert_eq!(flags, vec![false, true]);
        let mut needs: Vec<PartNeed> = (0..MAX_ACCESS_OBJECTS as u128)
            .map(|n| maybe(AccessObject::File, n + 10, AccessKind::Read))
            .collect();
        needs.push(PartNeed::always(
            AccessObject::Table,
            Some(id(5)),
            AccessKind::Add,
        ));
        let (objects, cut) = objects_of(&rows_of(&[PartNeeds {
            part: wf,
            needs,
            unavailable: false,
        }]));
        assert!(cut);
        assert_eq!(
            objects[0],
            (AccessObject::Table, id(5)),
            "必ず要る対象を先に確かめる"
        );
    }
}

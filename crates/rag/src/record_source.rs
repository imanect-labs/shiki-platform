//! 構造化データのレコードを検索ソースに加える（Task 15.8 スパイク・docs/data/rag-integration.md）。
//!
//! 既存の検索（[`crate::SearchService`]）は「候補 = file のチャンク」を前提にしている。本モジュールは
//! その前提を崩すための**型と post-filter の一般化**で、既存のファイル検索の挙動は変えない
//! （file の判定は [`crate::authz_filter::post_filter_by_file`] と同じ関数を共有する）。
//!
//! # 二段 authz（レコード）
//!
//! - **pre-filter**: チャンクの構造タグ `data_table:<t>|<table_id>`（[`record_authz_tag`]）∩
//!   `ListObjects(viewer, data_table)`（[`readable_tables`]）。folder/file タグと同型で、
//!   テーブル共有の変更で索引を書き換えない（PIT-3 と同じ性質）
//! - **post-filter**: テーブルごとに**クエリ 1 本**（[`RecordVisibility`]・実装は `crates/data` の
//!   `DataStore::rag_visible_records`）。テーブル viewer・行述語（v1 の意味論）・個別共有を
//!   REST の読取と同じ合成で評価する。**OpenFGA のタプルは 1 本も増やさない**（行を FGA オブジェクトに
//!   しない・PIT-7 と同じ判断）
//! - **世代**: チャンクの世代がテーブルの現在の世代と一致しない候補は落とす（`field_policy` を後から
//!   付けた直後、再構築が終わるまでの古いチャンクを出さない・fail-closed）
//!
//! pre-filter が壊れても（タグの汚染・tenant-only 縮退）、post-filter がテーブル viewer から評価し直す
//! ので見えない行は出ない（二段のどちらかが壊れても権限を守る）。

use std::collections::{BTreeMap, HashMap, HashSet};

use async_trait::async_trait;
use authz::{AuthContext, AuthzClient, ObjectType, Relation};
use uuid::Uuid;

use crate::authz_filter::{file_decisions, ReadableSet};
use crate::error::RagError;
use crate::search_types::RecordSearchResult;

/// 1 回の可視性問い合わせに載せる候補数の上限（data 側の上限と同じ）。
pub const MAX_RECORD_IDS_PER_QUERY: usize = 256;

/// 検索候補の出所（多型化した候補キー）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceRef {
    /// ドライブのファイル（認可の最小オブジェクトは file・PIT-7）。
    File { node_id: Uuid },
    /// 構造化データのレコード（認可は「テーブル ReBAC ＋ 行述語」・タプルを持たない）。
    Record { table_id: Uuid, record_id: Uuid },
}

/// 取得段（dense / keyword / 融合）が返す候補 1 件。
#[derive(Debug, Clone, PartialEq)]
pub struct SourceCandidate {
    pub chunk_id: Uuid,
    pub source: SourceRef,
    pub score: f32,
    /// レコードのチャンクの索引世代（ファイルは `None`）。
    pub generation: Option<String>,
}

/// post-filter が可視と判定したレコードの、**現在の値から作り直した**投影。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordProjection {
    pub rev: i64,
    pub title: String,
    pub body: String,
}

/// テーブル 1 つ分の可視性の答え。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VisibleRecords {
    /// テーブルの現在の索引世代（見えない・無い・オプトアウトなら `None` ＝全件を落とす）。
    pub generation: Option<String>,
    /// 可視なレコード（不可視と不存在は区別しない）。
    pub records: HashMap<Uuid, RecordProjection>,
}

/// レコードの post-filter（候補 `record_id` を可視集合に絞る）。
///
/// 実装は**テーブルごとにクエリ 1 本**で答え、認可材料をリクエストを跨いで保持しないこと（PIT-18）。
/// 本番の実装は `crates/data` の `DataStore::rag_visible_records` へ委ねる薄いアダプタ（rag は data に
/// 依存しないので、両方に依存する配線層に置く）。
#[async_trait]
pub trait RecordVisibility: Send + Sync {
    async fn visible_records(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        record_ids: &[Uuid],
    ) -> Result<VisibleRecords, RagError>;
}

/// レコードのチャンクに付ける pre-filter の構造タグ（`data_table:<tenant>|<table_id>`）。
#[must_use]
pub fn record_authz_tag(ctx: &AuthContext, table_id: Uuid) -> String {
    ctx.ns()
        .data_table(&table_id.to_string())
        .as_str()
        .to_string()
}

/// 閲覧者が viewer のテーブル集合（pre-filter のタグ・クエリごとに算出・キャッシュしない）。
///
/// 上限を超えたら tenant-only へ縮退する（[`crate::authz_filter::readable_set`] と同じ規則・PIT-1）。
pub async fn readable_tables(
    ctx: &AuthContext,
    authz: &dyn AuthzClient,
    max_tags: usize,
) -> Result<ReadableSet, RagError> {
    let tables = authz
        .list_objects(&ctx.subject(), Relation::Viewer, ObjectType::DataTable)
        .await?;
    if tables.len() > max_tags {
        return Ok(ReadableSet {
            tags: Vec::new(),
            overflowed: true,
        });
    }
    Ok(ReadableSet {
        tags: tables,
        overflowed: false,
    })
}

/// 認可済みの候補 1 件。
#[derive(Debug, Clone, PartialEq)]
pub struct AllowedCandidate {
    pub candidate: SourceCandidate,
    /// レコードの候補なら、可視性の問い合わせが返した現在の投影（引用本文に使う）。
    pub record: Option<RecordProjection>,
}

/// 多型の post-filter の結果。
#[derive(Debug, Default)]
pub struct SourcePostFilterOutcome {
    /// 認可された候補（入力順を保つ）。
    pub allowed: Vec<AllowedCandidate>,
    pub denied_chunks: usize,
    pub denied_files: usize,
    /// 不可視（または不存在）で落としたレコード数。
    pub denied_records: usize,
    /// 世代不一致（古いチャンク・オプトアウト済み）で落とした候補数。
    pub stale_chunks: usize,
    /// file id → allow（引用監査用・[`crate::authz_filter::PostFilterOutcome`] と同じ）。
    pub file_decisions: HashMap<Uuid, bool>,
    /// (table_id, record_id) → allow（引用監査用）。
    pub record_decisions: HashMap<(Uuid, Uuid), bool>,
}

/// file とレコードが混在する候補の post-filter（reranker の前に呼ぶ・PIT-2）。
///
/// file は従来どおり BatchCheck 1 往復、レコードはテーブルごとに可視性の問い合わせ 1 本
/// （[`MAX_RECORD_IDS_PER_QUERY`] 件ごと）。どちらかの問い合わせが失敗したら全体をエラーにする
/// （部分的な結果で「見えるはずのものが無い」を黙って返さない・fail-closed）。
pub async fn post_filter_sources(
    ctx: &AuthContext,
    authz: &dyn AuthzClient,
    records: &dyn RecordVisibility,
    candidates: Vec<SourceCandidate>,
) -> Result<SourcePostFilterOutcome, RagError> {
    let mut files = Vec::new();
    let mut seen_files = HashSet::new();
    // table_id → 重複なしの record_id（BTreeMap で問い合わせ順を固定する）。
    let mut by_table: BTreeMap<Uuid, Vec<Uuid>> = BTreeMap::new();
    let mut seen_records = HashSet::new();
    for c in &candidates {
        match c.source {
            SourceRef::File { node_id } => {
                if seen_files.insert(node_id) {
                    files.push(node_id);
                }
            }
            SourceRef::Record {
                table_id,
                record_id,
            } => {
                if seen_records.insert((table_id, record_id)) {
                    by_table.entry(table_id).or_default().push(record_id);
                }
            }
        }
    }
    let file_decisions = if files.is_empty() {
        HashMap::new()
    } else {
        file_decisions(ctx, authz, files).await?
    };
    let mut tables: HashMap<Uuid, VisibleRecords> = HashMap::new();
    for (table_id, ids) in &by_table {
        let mut merged = VisibleRecords::default();
        for chunk in ids.chunks(MAX_RECORD_IDS_PER_QUERY) {
            let v = records.visible_records(ctx, *table_id, chunk).await?;
            // 問い合わせの間に世代が変わったら（並行の再構築）、全件を落とす側に倒す。
            if merged.generation.is_some() && merged.generation != v.generation {
                merged = VisibleRecords::default();
                break;
            }
            merged.generation = v.generation;
            merged.records.extend(v.records);
        }
        tables.insert(*table_id, merged);
    }
    let mut out = SourcePostFilterOutcome {
        denied_files: file_decisions.values().filter(|a| !**a).count(),
        file_decisions,
        ..SourcePostFilterOutcome::default()
    };
    for (table_id, ids) in &by_table {
        let t = &tables[table_id];
        for id in ids {
            let ok = t.generation.is_some() && t.records.contains_key(id);
            out.record_decisions.insert((*table_id, *id), ok);
        }
    }
    out.denied_records = out.record_decisions.values().filter(|a| !**a).count();
    for c in candidates {
        let record = match c.source {
            SourceRef::File { node_id } => {
                if out.file_decisions.get(&node_id).copied().unwrap_or(false) {
                    None
                } else {
                    out.denied_chunks += 1;
                    continue;
                }
            }
            SourceRef::Record {
                table_id,
                record_id,
            } => {
                let t = &tables[&table_id];
                let Some(p) = t.records.get(&record_id) else {
                    out.denied_chunks += 1;
                    continue;
                };
                if t.generation.is_none() || c.generation != t.generation {
                    out.stale_chunks += 1;
                    continue;
                }
                Some(p.clone())
            }
        };
        out.allowed.push(AllowedCandidate {
            candidate: c,
            record,
        });
    }
    Ok(out)
}

/// 認可済みのレコード候補を検索結果にする（file の候補は従来の hydrate が担うので含めない）。
#[must_use]
pub fn record_results(allowed: &[AllowedCandidate]) -> Vec<RecordSearchResult> {
    allowed
        .iter()
        .filter_map(|a| match (a.candidate.source, &a.record) {
            (
                SourceRef::Record {
                    table_id,
                    record_id,
                },
                Some(p),
            ) => Some(RecordSearchResult {
                chunk_id: a.candidate.chunk_id,
                table_id,
                record_id,
                rev: p.rev,
                title: p.title.clone(),
                content: p.body.clone(),
                score: a.candidate.score,
            }),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    struct Fixed(VisibleRecords);

    #[async_trait]
    impl RecordVisibility for Fixed {
        async fn visible_records(
            &self,
            _ctx: &AuthContext,
            _table_id: Uuid,
            record_ids: &[Uuid],
        ) -> Result<VisibleRecords, RagError> {
            Ok(VisibleRecords {
                generation: self.0.generation.clone(),
                records: self
                    .0
                    .records
                    .iter()
                    .filter(|(k, _)| record_ids.contains(k))
                    .map(|(k, v)| (*k, v.clone()))
                    .collect(),
            })
        }
    }

    fn ctx() -> AuthContext {
        AuthContext::new(
            authz::Principal {
                kind: authz::PrincipalKind::User,
                id: "u".into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some("t".into()),
            },
            "acme".into(),
            "t".into(),
        )
    }

    fn cand(table: Uuid, record: Uuid, gen: Option<&str>) -> SourceCandidate {
        SourceCandidate {
            chunk_id: Uuid::new_v4(),
            source: SourceRef::Record {
                table_id: table,
                record_id: record,
            },
            score: 1.0,
            generation: gen.map(str::to_string),
        }
    }

    fn proj() -> RecordProjection {
        RecordProjection {
            rev: 1,
            title: "t".into(),
            body: "b".into(),
        }
    }

    /// file の候補が無ければ OpenFGA を叩かない（authz を呼ぶと panic する偽物で確かめる）。
    struct NoFga;
    #[async_trait]
    impl AuthzClient for NoFga {
        async fn check(
            &self,
            _: &authz::Subject,
            _: Relation,
            _: &authz::FgaObject,
            _: authz::Consistency,
        ) -> Result<bool, authz::AuthzError> {
            unreachable!("record だけの post-filter は OpenFGA を叩かない")
        }
        async fn write_tuple(
            &self,
            _: &authz::Subject,
            _: Relation,
            _: &authz::FgaObject,
        ) -> Result<bool, authz::AuthzError> {
            unreachable!()
        }
        async fn delete_tuple(
            &self,
            _: &authz::Subject,
            _: Relation,
            _: &authz::FgaObject,
        ) -> Result<bool, authz::AuthzError> {
            unreachable!()
        }
        async fn read_tuples(
            &self,
            _: &authz::FgaObject,
            _: Option<Relation>,
        ) -> Result<Vec<authz::ReadTupleKey>, authz::AuthzError> {
            unreachable!()
        }
        async fn list_objects(
            &self,
            _: &authz::Subject,
            _: Relation,
            _: ObjectType,
        ) -> Result<Vec<String>, authz::AuthzError> {
            unreachable!()
        }
        async fn delete_object_tuples(
            &self,
            _: &authz::FgaObject,
        ) -> Result<u32, authz::AuthzError> {
            unreachable!()
        }
        async fn read_subject_objects(
            &self,
            _: &authz::Subject,
            _: ObjectType,
        ) -> Result<Vec<String>, authz::AuthzError> {
            unreachable!()
        }
    }

    #[tokio::test]
    async fn invisible_and_stale_record_candidates_are_dropped() {
        let table = Uuid::new_v4();
        let (visible, hidden, stale) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let v = Fixed(VisibleRecords {
            generation: Some("g2".into()),
            records: [(visible, proj()), (stale, proj())].into_iter().collect(),
        });
        let out = post_filter_sources(
            &ctx(),
            &NoFga,
            &v,
            vec![
                cand(table, visible, Some("g2")),
                cand(table, hidden, Some("g2")),
                cand(table, stale, Some("g1")),
            ],
        )
        .await
        .unwrap();
        assert_eq!(out.allowed.len(), 1);
        assert_eq!(
            out.allowed[0].candidate.source,
            SourceRef::Record {
                table_id: table,
                record_id: visible
            }
        );
        assert_eq!(out.denied_chunks, 1);
        assert_eq!(out.stale_chunks, 1);
        assert_eq!(out.denied_records, 1);
        assert_eq!(record_results(&out.allowed).len(), 1);
    }

    #[tokio::test]
    async fn table_without_generation_drops_everything() {
        let table = Uuid::new_v4();
        let r = Uuid::new_v4();
        let v = Fixed(VisibleRecords {
            generation: None,
            records: [(r, proj())].into_iter().collect(),
        });
        let out = post_filter_sources(&ctx(), &NoFga, &v, vec![cand(table, r, None)])
            .await
            .unwrap();
        assert!(out.allowed.is_empty());
        assert!(!out.record_decisions[&(table, r)]);
    }
}

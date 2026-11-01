//! 二段 authz フィルタ（Task 2.7・PIT-1/2/3）。
//!
//! - **pre-filter**: ユーザーの可読オブジェクト集合（OpenFGA `ListObjects`）を
//!   クエリごとに算出し、`authz_tags ∩ 可読集合` で dense/keyword 両系統を絞る。
//!   タグは共有変更で再書込しない構造タグ（file 自身＋祖先フォルダ）なので、
//!   **grant は次のクエリで即反映**される（PIT-3 解消）。
//!   可読集合が上限を超えたら pre-filter を放棄して tenant-only へ縮退し、
//!   post-filter 全依存＋over-fetch で正しさを維持する（PIT-1 のフォールバック）。
//!   OpenFGA の ListObjects が応答の上限で切ったかもしれない集合（[`authz::visible::list_may_be_cut`]）も、
//!   設定の上限によらず同じく縮退する（切れた集合を正にすると、読めるはずの文書が黙って出ない・BUG-190）。
//! - **post-filter**: 取得後に OpenFGA で file 粒度の最終 check（HigherConsistency）。
//!   pre-filter のタグが陳腐化していても（move 直後など）権限変更に追従する二重防御。
//!   chunk を FGA オブジェクトにしない（PIT-7）。

use std::collections::{HashMap, HashSet};

use authz::{AuthContext, AuthzClient, CheckTuple, Consistency, ObjectType, Relation};
use uuid::Uuid;

use crate::error::RagError;
use crate::vector_store::ScoredChunk;

/// pre-filter に使う可読タグ集合。
#[derive(Debug, Clone)]
pub struct ReadableSet {
    /// `folder:<t>|<id>` / `file:<t>|<id>` の名前空間化文字列（ListObjects の応答形式のまま）。
    pub tags: Vec<String>,
    /// 上限超過で tenant-only へ縮退したか（over-fetch 係数の引き上げ判断に使う）。
    pub overflowed: bool,
}

/// ユーザーの可読 folder/file 集合を算出する（クエリごと・キャッシュしない）。
///
/// 次のどちらかなら tenant-only へ縮退する（`overflowed`）。
/// - folder か file の応答が ListObjects の上限に届いた（[`authz::visible::list_may_be_cut`]）。上限で切り詰められた
///   「不完全な集合」を正として使うと、可読文書が黙って欠ける（under-recall）。集合ごとに見るので、`max_tags` を
///   ListObjects の上限より大きく設定しても切れた集合は使わない（BUG-190）。
/// - タグの数の合計が `max_tags` を超えた（pre-filter の式が大きくなりすぎる）。
pub async fn readable_set(
    ctx: &AuthContext,
    authz: &dyn AuthzClient,
    max_tags: usize,
) -> Result<ReadableSet, RagError> {
    let subject = ctx.subject();
    let (folders, files) = futures::try_join!(
        authz.list_objects(&subject, Relation::Viewer, ObjectType::Folder),
        authz.list_objects(&subject, Relation::Viewer, ObjectType::File),
    )?;
    if may_be_cut(&[folders.len(), files.len()]) || folders.len() + files.len() > max_tags {
        return Ok(ReadableSet {
            tags: Vec::new(),
            overflowed: true,
        });
    }
    let mut tags = folders;
    tags.extend(files);
    Ok(ReadableSet {
        tags,
        overflowed: false,
    })
}

/// ListObjects の応答のどれかが上限で切れたかもしれないか（`lens` は応答ごとの件数・BUG-190）。
///
/// 可読集合（[`readable_set`]）と可読テーブル（[`crate::record_source::readable_tables`]）が同じ規則で縮退する。
pub(crate) fn may_be_cut(lens: &[usize]) -> bool {
    lens.iter().any(|n| authz::visible::list_may_be_cut(*n))
}

/// post-filter の結果。
#[derive(Debug)]
pub struct PostFilterOutcome {
    /// 認可された候補（入力順を保つ）。
    pub allowed: Vec<ScoredChunk>,
    /// 落とされた chunk 数。
    pub denied_chunks: usize,
    /// 落とされた file（node）数。
    pub denied_files: usize,
    /// 認可判定の内訳（file id → allow）。引用監査に記録する。
    pub file_decisions: HashMap<Uuid, bool>,
}

/// file 粒度の OpenFGA 最終検証（HigherConsistency・PIT-11）。
///
/// 候補を file（node）単位にまとめて BatchCheck（[`AuthzClient::batch_check`]）で判定し、
/// deny の chunk を全て落とす。
/// reranker の**前**に呼ぶこと（読めない chunk に rerank 計算を浪費しない・PIT-2）。
pub async fn post_filter_by_file(
    ctx: &AuthContext,
    authz: &dyn AuthzClient,
    candidates: Vec<ScoredChunk>,
) -> Result<PostFilterOutcome, RagError> {
    let distinct_files: Vec<Uuid> = {
        let mut seen = HashSet::new();
        candidates
            .iter()
            .filter(|c| seen.insert(c.node_id))
            .map(|c| c.node_id)
            .collect()
    };

    let file_decisions = file_decisions(ctx, authz, distinct_files).await?;

    let denied_files = file_decisions.values().filter(|allowed| !**allowed).count();
    let before = candidates.len();
    let allowed: Vec<ScoredChunk> = candidates
        .into_iter()
        .filter(|c| file_decisions.get(&c.node_id).copied().unwrap_or(false))
        .collect();
    Ok(PostFilterOutcome {
        denied_chunks: before - allowed.len(),
        denied_files,
        allowed,
        file_decisions,
    })
}

/// file 粒度の最終 check（BatchCheck 1 往復・50 問ごと・HigherConsistency）の判定表。
///
/// [`post_filter_by_file`] と、レコードを含む多型の post-filter
/// （[`crate::record_source::post_filter_sources`]）が共有する（file の判定規則を 2 つに分けない）。
pub(crate) async fn file_decisions(
    ctx: &AuthContext,
    authz: &dyn AuthzClient,
    distinct_files: Vec<Uuid>,
) -> Result<HashMap<Uuid, bool>, RagError> {
    // file ごとの check を BatchCheck 1 往復（50 問ごと）にまとめる（Task 15.9）。
    let subject = ctx.subject();
    let items: Vec<CheckTuple> = distinct_files
        .iter()
        .map(|file_id| CheckTuple {
            subject: subject.clone(),
            relation: Relation::Viewer,
            object: ctx.ns().file(&file_id.to_string()),
        })
        .collect();
    // 剥奪の即時反映が要る正しさクリティカル経路（PIT-11）。
    let allowed = authz
        .batch_check(&items, Consistency::HigherConsistency)
        .await?;
    if allowed.len() != distinct_files.len() {
        return Err(RagError::Authz(authz::AuthzError::Unexpected {
            status: 0,
            body: "batch_check の応答数が問い数と一致しません".into(),
        }));
    }
    Ok(distinct_files.into_iter().zip(allowed).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record_source::readable_tables;
    use async_trait::async_trait;
    use authz::visible::LIST_OBJECTS_MAX_RESULTS;
    use authz::{AuthzError, FgaObject, Principal, ReadTupleKey, Subject};

    /// ListObjects が種類ごとに決まった件数を返す偽物（ほかの口は呼ばれない）。
    struct Lists {
        folders: usize,
        files: usize,
        tables: usize,
    }

    #[async_trait]
    impl AuthzClient for Lists {
        async fn check(
            &self,
            _: &Subject,
            _: Relation,
            _: &FgaObject,
            _: Consistency,
        ) -> Result<bool, AuthzError> {
            unreachable!()
        }
        async fn write_tuple(
            &self,
            _: &Subject,
            _: Relation,
            _: &FgaObject,
        ) -> Result<bool, AuthzError> {
            unreachable!()
        }
        async fn delete_tuple(
            &self,
            _: &Subject,
            _: Relation,
            _: &FgaObject,
        ) -> Result<bool, AuthzError> {
            unreachable!()
        }
        async fn read_tuples(
            &self,
            _: &FgaObject,
            _: Option<Relation>,
        ) -> Result<Vec<ReadTupleKey>, AuthzError> {
            unreachable!()
        }
        async fn list_objects(
            &self,
            _: &Subject,
            _: Relation,
            ty: ObjectType,
        ) -> Result<Vec<String>, AuthzError> {
            let n = match ty {
                ObjectType::Folder => self.folders,
                ObjectType::File => self.files,
                ObjectType::DataTable => self.tables,
                other => unreachable!("{other:?}"),
            };
            Ok((0..n).map(|i| format!("{}:t|{i}", ty.as_str())).collect())
        }
        async fn delete_object_tuples(&self, _: &FgaObject) -> Result<u32, AuthzError> {
            unreachable!()
        }
        async fn read_subject_objects(
            &self,
            _: &Subject,
            _: ObjectType,
        ) -> Result<Vec<String>, AuthzError> {
            unreachable!()
        }
    }

    fn ctx() -> AuthContext {
        AuthContext::new(
            Principal {
                kind: authz::PrincipalKind::User,
                id: "u".into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: None,
            },
            "org".into(),
            "t".into(),
        )
    }

    /// 上限を ListObjects の上限より大きく設定しても、上限に届いた集合は使わずに縮退する（BUG-190）。
    #[tokio::test]
    async fn a_set_cut_by_list_objects_falls_back_whatever_the_setting() {
        let cap = LIST_OBJECTS_MAX_RESULTS;
        let generous = cap * 10;
        for (folders, files) in [(cap, 0), (0, cap), (cap + 5, 3)] {
            let set = readable_set(
                &ctx(),
                &Lists {
                    folders,
                    files,
                    tables: 0,
                },
                generous,
            )
            .await
            .unwrap();
            assert!(set.overflowed, "folders={folders} files={files}");
            assert!(set.tags.is_empty());
        }
        // 片方ずつは上限の内なら、合計が上限を超えていても設定の内なら使う（集合ごとに判定する）。
        let set = readable_set(
            &ctx(),
            &Lists {
                folders: cap - 1,
                files: cap - 1,
                tables: 0,
            },
            generous,
        )
        .await
        .unwrap();
        assert!(!set.overflowed);
        assert_eq!(set.tags.len(), 2 * (cap - 1));
        // 設定の上限は前と同じく効く（合計で見る）。
        let set = readable_set(
            &ctx(),
            &Lists {
                folders: 300,
                files: 201,
                tables: 0,
            },
            500,
        )
        .await
        .unwrap();
        assert!(set.overflowed);
        let set = readable_set(
            &ctx(),
            &Lists {
                folders: 300,
                files: 200,
                tables: 0,
            },
            500,
        )
        .await
        .unwrap();
        assert!(!set.overflowed);
        assert_eq!(set.tags.len(), 500);
    }

    /// 可読テーブル（レコードの pre-filter）も同じ規則で縮退する（BUG-190）。
    #[tokio::test]
    async fn readable_tables_falls_back_when_cut() {
        let cap = LIST_OBJECTS_MAX_RESULTS;
        let lists = |tables| Lists {
            folders: 0,
            files: 0,
            tables,
        };
        let cut = readable_tables(&ctx(), &lists(cap), cap * 10)
            .await
            .unwrap();
        assert!(cut.overflowed && cut.tags.is_empty());
        let whole = readable_tables(&ctx(), &lists(cap - 1), cap * 10)
            .await
            .unwrap();
        assert!(!whole.overflowed);
        assert_eq!(whole.tags.len(), cap - 1);
        let over = readable_tables(&ctx(), &lists(501), 500).await.unwrap();
        assert!(over.overflowed);
    }
}

//! 二段 authz フィルタ（Task 2.7・PIT-1/2/3）。
//!
//! - **pre-filter**: ユーザーの可読オブジェクト集合（OpenFGA `ListObjects`）を
//!   クエリごとに算出し、`authz_tags ∩ 可読集合` で dense/keyword 両系統を絞る。
//!   タグは共有変更で再書込しない構造タグ（file 自身＋祖先フォルダ）なので、
//!   **grant は次のクエリで即反映**される（PIT-3 解消）。
//!   可読集合が上限を超えたら pre-filter を放棄して tenant-only へ縮退し、
//!   post-filter 全依存＋over-fetch で正しさを維持する（PIT-1 のフォールバック）。
//! - **post-filter**: 取得後に OpenFGA で file 粒度の最終 check（HigherConsistency）。
//!   pre-filter のタグが陳腐化していても（move 直後など）権限変更に追従する二重防御。
//!   chunk を FGA オブジェクトにしない（PIT-7）。

use std::collections::{HashMap, HashSet};

use authz::{AuthContext, AuthzClient, Consistency, ObjectType, Relation};
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
/// `max_tags` は OpenFGA ListObjects の応答上限（既定 1000）より小さく設定すること。
/// 応答が上限で切り詰められた「不完全な集合」を正として使うと可読文書が silent に
/// 欠落する（under-recall）ため、上限手前で縮退する方が安全。
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
    if folders.len() + files.len() > max_tags {
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
/// 候補を file（node）単位にまとめて並列 check し、deny の chunk を全て落とす。
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

    let subject = ctx.subject();
    let checks = distinct_files.iter().map(|file_id| {
        let object = ctx.ns().file(&file_id.to_string());
        let subject = subject.clone();
        async move {
            let allowed = authz
                .check(
                    &subject,
                    Relation::Viewer,
                    &object,
                    // 剥奪の即時反映が要る正しさクリティカル経路（PIT-11）。
                    Consistency::HigherConsistency,
                )
                .await?;
            Ok::<(Uuid, bool), RagError>((*file_id, allowed))
        }
    });
    let file_decisions: HashMap<Uuid, bool> = futures::future::try_join_all(checks)
        .await?
        .into_iter()
        .collect();

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

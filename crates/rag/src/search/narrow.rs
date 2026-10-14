//! 呼び出し側（モデルのツール引数）が指定した絞り込みと、知識スコープの**交差**（#423）。
//!
//! `SearchScope` は「フォルダ配下全体 ∪ 個別ファイル」の和集合なので、2 つのスコープの交差は
//! 1 つの `SearchScope` で表せる。木の部分木どうしは「入れ子」か「交わらない」のどちらかで、
//! 交差は各組の内側の方になるため:
//!
//! 交差 = { 絞り込み側の要素のうち、知識スコープの内側にあるもの }
//!      ∪ { 知識スコープ側の要素のうち、絞り込みの内側にあるもの }
//!
//! 「内側にあるか」は**同じ種別の一致**か、フォルダからの真の子孫（`node_closure` の depth > 0・
//! tenant 述語付き）で判定する。種別をまたいだ一致は認めない: skill がフォルダの ID を誤って
//! `files` に保存していても（その場合 `file:` タグは何にも一致しない）、モデルが同じ ID を
//! `folder_ids` に渡して配下全体へ広げることはできない。
//!
//! **権限は一切広がらない**: 交差は知識スコープの部分集合で、検索時にはさらに可読集合
//! （pre-filter）と OpenFGA の file check（post-filter）が掛かる。union にすると skill の知識
//! スコープをモデルの指定で広げられてしまうため、必ず交差を取る。

use std::collections::HashSet;

use authz::AuthContext;
use uuid::Uuid;

use crate::error::RagError;
use crate::search::{SearchOutput, SearchService};
use crate::search_types::{SearchDebug, SearchMode, SearchScope};
use crate::vector_store::ScoredChunk;

impl SearchService {
    /// 知識スコープ `base` をモデルの絞り込み `narrow` で絞って検索する（#423）。
    ///
    /// - `narrow` が空なら `base` のまま [`Self::search`] と同じ。
    /// - 交差が空なら検索せずに 0 件を返す（空のスコープは「絞らない」になるため）。この場合も
    ///   `rag.search` の監査は残す（全検索操作の監査・NFR-6）。
    /// - 交差で検索するときは、候補を `base` で**検索時点の木に対して**再検証する。交差を
    ///   取った後に絞り込み先のフォルダが `base` の外へ移動されても、上限を失わない。
    #[allow(clippy::too_many_arguments)] // search の引数＋絞り込み（呼び出しは doc_search の 1 箇所）。
    pub async fn search_narrowed(
        &self,
        ctx: &AuthContext,
        query: &str,
        top_k: Option<u32>,
        mode: SearchMode,
        base: Option<&SearchScope>,
        narrow: &SearchScope,
        trace_id: Option<&str>,
    ) -> Result<SearchOutput, RagError> {
        let base = base.filter(|b| !b.is_empty());
        if narrow.is_empty() {
            return self.search(ctx, query, top_k, mode, base, trace_id).await;
        }
        let scope = self.narrow_scope(ctx, base, narrow).await?;
        if scope.is_empty() {
            self.audit_citations(
                ctx,
                query,
                &[],
                &std::collections::HashMap::new(),
                Some(&scope),
                trace_id,
            )
            .await?;
            return Ok(SearchOutput {
                results: Vec::new(),
                debug: SearchDebug::default(),
            });
        }
        self.search_bounded(ctx, query, top_k, mode, Some(&scope), base, trace_id)
            .await
    }

    /// 候補のうち、ファイルが `bound` の内側にあるものの node_id 集合（検索時点の木で判定）。
    pub(super) async fn files_within(
        &self,
        ctx: &AuthContext,
        bound: &SearchScope,
        chunks: &[ScoredChunk],
    ) -> Result<HashSet<Uuid>, RagError> {
        let files: Vec<Uuid> = chunks
            .iter()
            .map(|c| c.node_id)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        if files.is_empty() {
            return Ok(HashSet::new());
        }
        let mut inside: HashSet<Uuid> = files
            .iter()
            .filter(|f| bound.files.contains(f))
            .copied()
            .collect();
        if !bound.folders.is_empty() {
            let under: Vec<Uuid> = sqlx::query_scalar(
                "select descendant from node_closure \
                 where tenant_id = $1 and depth > 0 and ancestor = any($2) and descendant = any($3)",
            )
            .bind(&ctx.tenant_id)
            .bind(&bound.folders)
            .bind(&files)
            .fetch_all(&self.pool)
            .await?;
            inside.extend(under);
        }
        Ok(inside)
    }

    /// 知識スコープ `base`（None = 全可読範囲）を `narrow` で絞った実効スコープを返す。
    ///
    /// 返り値が空なら交差が空（＝検索しても何も返らない）。空の `SearchScope` は検索側で
    /// 「絞らない」と解釈されるので、呼び出し側は**空なら検索せずに 0 件を返す**こと（fail-closed）。
    /// `narrow` が空のときは `base` をそのまま返す（絞り込みの指定なし）。
    pub async fn narrow_scope(
        &self,
        ctx: &AuthContext,
        base: Option<&SearchScope>,
        narrow: &SearchScope,
    ) -> Result<SearchScope, RagError> {
        let base = base.filter(|b| !b.is_empty());
        if narrow.is_empty() {
            return Ok(base.cloned().unwrap_or_default());
        }
        let Some(base) = base else {
            // 知識スコープ無し: 絞り込みがそのまま実効スコープ（存在しない id は何にも一致しない）。
            return Ok(dedup(narrow.clone()));
        };
        let ids: Vec<Uuid> = base
            .folders
            .iter()
            .chain(&base.files)
            .chain(&narrow.folders)
            .chain(&narrow.files)
            .copied()
            .collect();
        let edges: HashSet<(Uuid, Uuid)> = sqlx::query_as::<_, (Uuid, Uuid)>(
            "select ancestor, descendant from node_closure \
             where tenant_id = $1 and depth > 0 and ancestor = any($2) and descendant = any($2)",
        )
        .bind(&ctx.tenant_id)
        .bind(&ids)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .collect();
        // `xs` のうち `scope` の内側にあるもの。`same_kind` は `scope` 側の同じ種別の列
        // （フォルダならフォルダ、ファイルならファイル）で、一致はその中でだけ認める。
        let pick = |xs: &[Uuid], same_kind: &[Uuid], scope: &SearchScope| -> Vec<Uuid> {
            xs.iter()
                .filter(|x| {
                    same_kind.contains(x)
                        || scope.folders.iter().any(|f| edges.contains(&(*f, **x)))
                })
                .copied()
                .collect()
        };
        let mut folders = pick(&narrow.folders, &base.folders, base);
        folders.extend(pick(&base.folders, &narrow.folders, narrow));
        let mut files = pick(&narrow.files, &base.files, base);
        files.extend(pick(&base.files, &narrow.files, narrow));
        Ok(dedup(SearchScope { folders, files }))
    }
}

/// 順序を保って重複を除く（監査ログとタグ数を無駄に膨らませない）。
fn dedup(scope: SearchScope) -> SearchScope {
    let uniq = |xs: Vec<Uuid>| {
        let mut seen = HashSet::new();
        xs.into_iter().filter(|x| seen.insert(*x)).collect()
    };
    SearchScope {
        folders: uniq(scope.folders),
        files: uniq(scope.files),
    }
}

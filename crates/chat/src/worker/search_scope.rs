//! 会話の社内文書検索の実効スコープ（skill の知識スコープ ∩ 会話の検索フォルダ・#965）。
//!
//! どちらも**絞り込みだけ**で、権限は広げない（検索結果はこれまでどおり可読集合の pre-filter と
//! OpenFGA の post-filter を通る）。両方あるときは交差を取る（union にするとどちらかの範囲を
//! もう一方で広げられる）。交差は rag の [`rag::SearchService::narrow_scope`] が木に対して求める。

use authz::AuthContext;
use rag::SearchScope;
use uuid::Uuid;

use super::ChatWorker;
use crate::store::ClaimedRun;
use crate::ChatError;

/// 実効スコープの決め方（木を引かずに決まるか、交差を取るか）。
#[derive(Debug, PartialEq, Eq)]
enum ScopePlan {
    /// そのまま使う（`None` は全可読範囲）。
    Use(Option<SearchScope>),
    /// skill の知識スコープと検索フォルダの交差を取る。
    Intersect {
        base: SearchScope,
        folder: SearchScope,
    },
}

/// skill の知識スコープと会話の検索フォルダから、実効スコープの決め方を選ぶ。
fn plan(skill_scope: Option<SearchScope>, folder: Option<Uuid>) -> ScopePlan {
    let skill_scope = skill_scope.filter(|s| !s.is_empty());
    let Some(folder) = folder else {
        return ScopePlan::Use(skill_scope);
    };
    let folder = SearchScope {
        folders: vec![folder],
        files: Vec::new(),
    };
    match skill_scope {
        // skill の範囲が無いなら、フォルダがそのまま実効スコープ。
        None => ScopePlan::Use(Some(folder)),
        Some(base) => ScopePlan::Intersect { base, folder },
    }
}

/// 交差が空なら「何にも当たらない」スコープにする。空の [`SearchScope`] は「絞らない」と
/// 解釈される（全可読範囲になる）ので、そのまま返すと絞り込みが外れてしまう（fail-closed）。
fn non_empty_or_nothing(scope: SearchScope) -> SearchScope {
    if scope.is_empty() {
        SearchScope {
            folders: Vec::new(),
            // どのチャンクのタグにも一致しない ID（nil）。
            files: vec![Uuid::nil()],
        }
    } else {
        scope
    }
}

impl ChatWorker {
    /// この run の doc_search / doc_read に課す知識スコープ。`None` は全可読範囲。
    pub(super) async fn effective_search_scope(
        &self,
        ctx: &AuthContext,
        run: &ClaimedRun,
        skills: &[crate::skill::AppliedSkill],
    ) -> Result<Option<SearchScope>, ChatError> {
        let folder = self
            .store
            .thread_search_folder(&run.tenant_id, run.thread_id)
            .await?;
        match plan(crate::skill::combined_scope(skills), folder) {
            ScopePlan::Use(scope) => Ok(scope),
            ScopePlan::Intersect { base, folder } => {
                let Some(search) = &self.search else {
                    // 検索が未配線なら doc_search 自体を出さないので、範囲は使われない。
                    return Ok(Some(base));
                };
                let both = search
                    .narrow_scope(ctx, Some(&base), &folder)
                    .await
                    .map_err(|e| ChatError::Internal(format!("search scope: {e}")))?;
                Ok(Some(non_empty_or_nothing(both)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{non_empty_or_nothing, plan, ScopePlan};
    use rag::SearchScope;
    use uuid::Uuid;

    fn folders(ids: &[Uuid]) -> SearchScope {
        SearchScope {
            folders: ids.to_vec(),
            files: Vec::new(),
        }
    }

    #[test]
    fn plan_uses_skill_scope_when_no_folder_is_chosen() {
        assert_eq!(plan(None, None), ScopePlan::Use(None));
        let s = folders(&[Uuid::new_v4()]);
        assert_eq!(plan(Some(s.clone()), None), ScopePlan::Use(Some(s)));
        // 空の知識スコープは「絞らない」と同じ。
        assert_eq!(
            plan(Some(SearchScope::default()), None),
            ScopePlan::Use(None)
        );
    }

    #[test]
    fn plan_uses_the_folder_alone_or_intersects_with_the_skill_scope() {
        let f = Uuid::new_v4();
        assert_eq!(plan(None, Some(f)), ScopePlan::Use(Some(folders(&[f]))));
        assert_eq!(
            plan(Some(SearchScope::default()), Some(f)),
            ScopePlan::Use(Some(folders(&[f])))
        );
        let base = folders(&[Uuid::new_v4()]);
        assert_eq!(
            plan(Some(base.clone()), Some(f)),
            ScopePlan::Intersect {
                base,
                folder: folders(&[f])
            }
        );
    }

    #[test]
    fn empty_intersection_matches_nothing_instead_of_everything() {
        let s = non_empty_or_nothing(SearchScope::default());
        assert!(!s.is_empty(), "空のままだと全可読範囲になる");
        assert_eq!(s.files, vec![Uuid::nil()]);
        let f = Uuid::new_v4();
        assert_eq!(non_empty_or_nothing(folders(&[f])), folders(&[f]));
    }
}

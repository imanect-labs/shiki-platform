//! 会話の社内文書検索に課す範囲（skill の知識スコープと会話の検索フォルダ・#965）。
//!
//! どちらも**絞り込みだけ**で、権限は広げない（検索結果はこれまでどおり可読集合の pre-filter と
//! OpenFGA の post-filter を通る）。両方あるときは**交差前のまま**別々に課す。先に交差を 1 つへ
//! まとめると、その後に検索フォルダが skill の範囲の外へ移動されたとき skill の制約が失われる。
//! doc_search（[`rag::SearchService::search_narrowed_within`]）と doc_read は、検索・読み出しの
//! たびに全部の範囲の内側かを検索時点の木で確かめる。

use rag::SearchScope;
use uuid::Uuid;

use super::ChatWorker;
use crate::store::ClaimedRun;
use crate::ChatError;

/// 課す範囲（空は全可読範囲）。skill の知識スコープ・検索フォルダの順で、空の範囲は入れない。
fn bounds(skill_scope: Option<SearchScope>, folder: Option<Uuid>) -> SearchBounds {
    let skill = skill_scope.filter(|s| !s.is_empty());
    let folder = folder.map(|f| SearchScope {
        folders: vec![f],
        files: Vec::new(),
    });
    SearchBounds { skill, folder }
}

/// doc_search / doc_read に課す範囲。
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct SearchBounds {
    /// skill の知識スコープ（無ければ `None`）。
    pub(super) skill: Option<SearchScope>,
    /// 会話の検索フォルダ（無ければ `None`）。
    pub(super) folder: Option<SearchScope>,
}

impl SearchBounds {
    /// 課す範囲の列（古典の事前検索に渡す）。
    pub(super) fn as_slice(&self) -> Vec<&SearchScope> {
        self.skill.iter().chain(self.folder.iter()).collect()
    }
}

impl ChatWorker {
    /// この run の doc_search / doc_read に課す範囲。
    pub(super) async fn search_bounds(
        &self,
        run: &ClaimedRun,
        skills: &[crate::skill::AppliedSkill],
    ) -> Result<SearchBounds, ChatError> {
        let folder = self
            .store
            .thread_search_folder(&run.tenant_id, run.thread_id)
            .await?;
        Ok(bounds(crate::skill::combined_scope(skills), folder))
    }
}

#[cfg(test)]
mod tests {
    use super::{bounds, SearchBounds};
    use rag::SearchScope;
    use uuid::Uuid;

    fn folders(ids: &[Uuid]) -> SearchScope {
        SearchScope {
            folders: ids.to_vec(),
            files: Vec::new(),
        }
    }

    #[test]
    fn no_skill_scope_and_no_folder_leaves_the_whole_readable_range() {
        assert_eq!(bounds(None, None), SearchBounds::default());
        // 空の知識スコープは「絞らない」と同じ。
        assert_eq!(
            bounds(Some(SearchScope::default()), None),
            SearchBounds::default()
        );
        assert!(SearchBounds::default().as_slice().is_empty());
    }

    #[test]
    fn skill_scope_and_folder_are_kept_apart_instead_of_being_intersected() {
        let f = Uuid::new_v4();
        let only_folder = bounds(None, Some(f));
        assert_eq!(only_folder.folder, Some(folders(&[f])));
        assert_eq!(only_folder.as_slice(), vec![&folders(&[f])]);

        let base = folders(&[Uuid::new_v4()]);
        let both = bounds(Some(base.clone()), Some(f));
        assert_eq!(both.skill, Some(base.clone()));
        assert_eq!(both.folder, Some(folders(&[f])));
        // 交差前のまま両方を課す（検索・読み出しのたびに両方の内側かを確かめる）。
        assert_eq!(both.as_slice(), vec![&base, &folders(&[f])]);
    }
}

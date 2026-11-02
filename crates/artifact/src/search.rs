//! 名前で探す一覧（AI のツール `list_workflows`・GAP-31）。`store.rs` から分離。
//!
//! 名前・現行版の表示名・説明の部分一致まで SQL で絞った候補（テナント・組織・種類・自分が持ち主のものを先に・
//! 新しい順）を、「自分が owner」か OpenFGA の viewer の判定（BatchCheck）で絞る（全件取得→フィルタをしない・
//! 二段の認可）。候補は ListObjects を引かずに DB から出す（ListObjects の上限で、共有された分が黙って欠けない・
//! BUG-149・[`crate::visible`]）。

use authz::{AuthContext, Consistency, Relation};

use crate::model::{Artifact, ArtifactKind};
use crate::store::{artifact_columns, ArtifactRow, ArtifactStore};
use crate::visible::{in_order, Candidate, MAX_CHECKS};
use crate::{map_db, ArtifactError};

/// 名前で絞る候補の上限（これより多く当たったら、自分のもの以外を新しい順に打ち切る）。
const MAX_CANDIDATES: i64 = 1_000;
/// 探す語の上限（文字数）。
pub const MAX_SEARCH_QUERY_CHARS: usize = 100;

/// [`ArtifactStore::search_visible`] の結果。
#[derive(Debug, Clone)]
pub struct SearchPage {
    /// 見えるもの（更新の新しい順・`limit` 件まで）。
    pub items: Vec<Artifact>,
    /// ほかにも当たるものがある（かもしれない）。`limit` を超えて見つかったか、候補か OpenFGA に問う数の上限で
    /// 打ち切った。
    pub more: bool,
}

/// [`ArtifactStore::search_visible`] の頼み。
#[derive(Debug, Clone, Copy)]
pub struct SearchRequest<'a> {
    pub kind: ArtifactKind,
    /// 名前・現行版の表示名（`display_name`）・説明（`description`）に含む語（大文字小文字を区別しない・
    /// 空白だけ・省略は絞らない）。
    pub query: Option<&'a str>,
    /// 共有されたもの（viewer）も含めるか（偽なら自分が持ち主のものだけ）。
    pub include_shared: bool,
    /// 件数（1〜100）。
    pub limit: i64,
}

impl ArtifactStore {
    /// 名前で探す（自分が持ち主のもの・`include_shared` なら共有されたものも・新しい順）。
    ///
    /// 語は SQL の部分一致（`ILIKE`・`%` `_` `\` は文字として扱う）で絞り、見えるかは一覧と同じ判定（持ち主の列か
    /// OpenFGA の viewer）で決める。行ごとの監査は書かない（一覧と同じ・中身を読むときに `artifact.read` が残る）。
    pub async fn search_visible(
        &self,
        ctx: &AuthContext,
        req: SearchRequest<'_>,
    ) -> Result<SearchPage, ArtifactError> {
        let query = req.query.map(str::trim).filter(|q| !q.is_empty());
        if query.is_some_and(|q| q.chars().count() > MAX_SEARCH_QUERY_CHARS) {
            return Err(ArtifactError::Invalid(format!(
                "探す語は {MAX_SEARCH_QUERY_CHARS} 字までです"
            )));
        }
        let needle = query.map(|q| format!("%{}%", escape_like(q)));
        let limit = usize::try_from(req.limit.clamp(1, 100)).unwrap_or(1);
        let mut candidates: Vec<Candidate> = sqlx::query_as(
            "SELECT a.id, a.owner, a.updated_at \
             FROM artifact a \
             JOIN artifact_version v \
               ON v.tenant_id = a.tenant_id AND v.artifact_id = a.id AND v.version = a.current_version \
             WHERE a.tenant_id = $1 AND a.org = $2 AND a.kind = $3 AND a.deleted_at IS NULL \
               AND ($4 OR a.owner = $5) \
               AND ($6::text IS NULL OR a.name ILIKE $6 ESCAPE '\\' \
                    OR v.body->>'display_name' ILIKE $6 ESCAPE '\\' \
                    OR v.body->>'description' ILIKE $6 ESCAPE '\\') \
             ORDER BY (a.owner = $5) DESC, a.updated_at DESC, a.id DESC LIMIT $7",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(req.kind.as_str())
        .bind(req.include_shared)
        .bind(&ctx.principal.id)
        .bind(needle)
        .bind(MAX_CANDIDATES)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        let cut_candidates = i64::try_from(candidates.len()).unwrap_or(i64::MAX) >= MAX_CANDIDATES;
        // 打ち切りは自分のものを後回しにしない（SQL の並び）。選ぶ順は持ち主を問わず新しい順。
        candidates.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(b.id.cmp(&a.id)));
        let picked = self.pick_visible(ctx, &candidates, limit + 1).await?;
        let more = picked.ids.len() > limit || cut_candidates || picked.checks >= MAX_CHECKS;
        let mut ids = picked.ids;
        ids.truncate(limit);
        if ids.is_empty() {
            return Ok(SearchPage {
                items: Vec::new(),
                more,
            });
        }
        let rows: Vec<ArtifactRow> = sqlx::query_as(concat!(
            "SELECT ",
            artifact_columns!(),
            " FROM artifact \
             WHERE tenant_id = $1 AND org = $2 AND id = ANY($3) AND deleted_at IS NULL"
        ))
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(&ids)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        let items = rows
            .into_iter()
            .map(ArtifactRow::into_artifact)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SearchPage {
            items: in_order(&ids, items, |a| a.id),
            more,
        })
    }

    /// それぞれを本人が編集できるか（入力順）。持ち主なら編集できる。ほかは editor を OpenFGA にまとめて問う
    /// （BatchCheck・見える一覧の注記に使う。保存の可否は保存の口が改めて確かめる）。
    pub async fn editable_flags(
        &self,
        ctx: &AuthContext,
        items: &[Artifact],
    ) -> Result<Vec<bool>, ArtifactError> {
        let ns = ctx.ns();
        let ask: Vec<authz::FgaObject> = items
            .iter()
            .filter(|a| a.owner != ctx.principal.id)
            .map(|a| ns.artifact(&a.id.to_string()))
            .collect();
        let mut held = authz::visible::holds_any(
            self.authz.as_ref(),
            &ctx.subject(),
            &[Relation::Editor],
            &ask,
            Consistency::MinimizeLatency,
        )
        .await
        .map_err(|e| ArtifactError::Internal(format!("authz: {e}")))?
        .into_iter();
        Ok(items
            .iter()
            .map(|a| a.owner == ctx.principal.id || held.next().unwrap_or(false))
            .collect())
    }
}

/// `LIKE` の特殊文字（`%`・`_`・`\`）を文字として扱うように逃がす（`ESCAPE '\'`）。
fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_wildcards_are_matched_literally() {
        assert_eq!(escape_like("経費_100%"), "経費\\_100\\%");
        assert_eq!(escape_like("a\\b"), "a\\\\b");
        assert_eq!(escape_like("経費フロー"), "経費フロー");
    }
}

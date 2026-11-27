//! 使う人に下書きを見せない（下書きと公開を分ける種類・ミニアプリ・D-16・BUG-213）。`versions.rs` から分離。
//!
//! 汎用の一覧と取得（`GET /artifacts`・`GET /artifacts/{id}`）は、メタデータに下書きの番号（`current_version`）・
//! 前に公開していた版・下書きの保存で進んだ更新の時刻を持つ。下書きと公開を分ける種類で editor でない人には、
//! [`crate::Artifact::as_seen_by_user`] の形にして返す（決まりは [`crate::ArtifactKind::shows_drafts`] の 1 か所・
//! `GET /mini-apps/{id}/resolved` と同じ）。

use std::collections::HashSet;

use authz::{AuthContext, Consistency, Relation};
use uuid::Uuid;

use crate::model::Artifact;
use crate::store::ArtifactStore;
use crate::ArtifactError;

impl ArtifactStore {
    /// メタデータを、呼ぶ人に見せる形で取得する（viewer 権限・汎用の `GET /artifacts/{id}`）。
    pub async fn get_as_seen(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Artifact, ArtifactError> {
        let meta = self.get(ctx, id, trace_id).await?;
        self.as_seen_by(ctx, vec![meta])
            .await?
            .pop()
            .ok_or(ArtifactError::NotFound)
    }

    /// 呼ぶ人に見せる形にそろえる（並びはそのまま）。
    ///
    /// 下書きと公開を分ける種類のうち、持ち主の列が呼ぶ人でないものだけ、editor かを OpenFGA にまとめて問う
    /// （BatchCheck・持ち主は editor を含む）。editor でなければ [`Artifact::as_seen_by_user`] の形にする。読めない
    /// ときは誤りにする（下書きの番号を見せる向きに倒さない）。
    pub async fn as_seen_by(
        &self,
        ctx: &AuthContext,
        items: Vec<Artifact>,
    ) -> Result<Vec<Artifact>, ArtifactError> {
        let ask: Vec<Uuid> = items
            .iter()
            .filter(|a| a.kind.has_drafts() && a.owner != ctx.principal().id)
            .map(|a| a.id)
            .collect();
        if ask.is_empty() {
            return Ok(items);
        }
        let ns = ctx.ns();
        let objects: Vec<authz::FgaObject> =
            ask.iter().map(|id| ns.artifact(&id.to_string())).collect();
        let held = authz::visible::holds_any(
            self.authz.as_ref(),
            &ctx.subject(),
            &[Relation::Editor],
            &objects,
            Consistency::HigherConsistency,
        )
        .await
        .map_err(|e| ArtifactError::Internal(format!("authz: {e}")))?;
        let editors: HashSet<Uuid> = ask
            .into_iter()
            .zip(held)
            .filter_map(|(id, editor)| editor.then_some(id))
            .collect();
        Ok(items
            .into_iter()
            .map(|a| {
                let can_edit = a.owner == ctx.principal().id || editors.contains(&a.id);
                if a.kind.shows_drafts(can_edit) {
                    a
                } else {
                    a.as_seen_by_user()
                }
            })
            .collect())
    }
}

//! 自分のものと共有されたものを合わせた一覧（`GET /artifacts`・`GET /workflows` の同じ入口・BUG-162・BUG-185・
//! BUG-186）。
//!
//! 先頭のページは自分のものと共有されたものを合わせて返し、続きの位置はそれぞれ別に返す（`owned_next`・
//! `shared_next_cursor`）。合わせたページの最後の 1 件を自分のものの続きの位置に使うと、最後が共有されたものだった
//! ときに、それより新しい自分のものを読み飛ばす（BUG-185）。

use artifact::{Artifact, ArtifactKind, ArtifactPosition, ArtifactStore};
use authz::AuthContext;
use chrono::{DateTime, Utc};
use data::DataStore;
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::ApiError;

/// 自分のものの続きの位置（返した自分のものの最後の 1 件・後ろにまだあるときだけ）。
///
/// 自分のものを指すだけなので封をしない（`GET /artifacts`・`GET /workflows` の `before_*` にそのまま渡す）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
pub struct OwnedNext {
    pub before_updated_at: DateTime<Utc>,
    pub before_id: Uuid,
}

/// 自分のものと共有されたものを合わせた一覧の頼み（`GET /artifacts`・`GET /workflows` の同じ契約）。
pub(crate) struct ListPageRequest<'a> {
    pub kind: Option<ArtifactKind>,
    /// 自分のものの続き（前の応答の `owned_next`）。
    pub before: Option<ArtifactPosition>,
    pub limit: i64,
    /// 共有された分の続き（前の応答の `shared_next_cursor`）。
    pub shared_cursor: Option<&'a str>,
}

/// 自分のものと共有されたものを合わせた一覧の 1 ページ（[`list_page`]）。
pub(crate) struct ListPage {
    /// 更新の新しい順。
    pub items: Vec<Artifact>,
    pub owned_next: Option<OwnedNext>,
    pub shared_next_cursor: Option<String>,
}

/// 自分のものと共有されたものを合わせた一覧を読む（アーティファクトとワークフローの一覧の単一の入口）。
///
/// - 続きの位置が無い頼みは、自分のものの先頭のページと共有された分の先頭のページを合わせて返す。
/// - `before`（自分のものの続き）は自分のものの続きだけを、`shared_cursor`（共有された分の続き）は共有された分の
///   続きだけを返す（両方は同時に使えない）。どちらの続きの位置も、後ろにまだあるときだけ返す。
/// - 共有された分の続きの位置は見えないものを指しうるので、テナントに束縛して暗号化する（BUG-162）。
/// - 下書きと公開を分ける種類で editor でないものは、公開した版だけを見せる形にする（[`ArtifactStore::as_seen_by`]・
///   BUG-213）。並べ替えと続きの位置は、その前の保存した時刻で決める。
pub(crate) async fn list_page(
    artifacts: &ArtifactStore,
    data: &DataStore,
    ctx: &AuthContext,
    req: ListPageRequest<'_>,
) -> Result<ListPage, ApiError> {
    let ListPageRequest {
        kind,
        before,
        limit,
        shared_cursor,
    } = req;
    if let Some(c) = shared_cursor.filter(|c| !c.is_empty()) {
        if before.is_some() {
            return Err(ApiError::BadRequest(
                "shared_cursor と before_updated_at・before_id は同時に使えません".into(),
            ));
        }
        let p = data.open_list_position(shared_position_kind(kind), ctx.tenant_id(), c)?;
        let page = artifacts
            .list_shared_page(ctx, kind, Some((p.updated_at, p.id)), limit)
            .await?;
        return Ok(ListPage {
            items: artifacts.as_seen_by(ctx, page.items).await?,
            owned_next: None,
            shared_next_cursor: seal_shared(data, ctx, kind, page.next)?,
        });
    }
    let owned = artifacts.list_mine_page(ctx, kind, before, limit).await?;
    let owned_next = owned.next.map(|(before_updated_at, before_id)| OwnedNext {
        before_updated_at,
        before_id,
    });
    let mut items = owned.items;
    // 自分のものの続き（`before` あり）は、自分のものの続きだけを返す（共有された分は先頭のページで合わせ済み）。
    let mut shared_next_cursor = None;
    if before.is_none() {
        let shared = artifacts.list_shared_page(ctx, kind, None, limit).await?;
        items.extend(shared.items);
        items.sort_by_key(|a| std::cmp::Reverse((a.updated_at, a.id)));
        items.dedup_by_key(|a| a.id);
        shared_next_cursor = seal_shared(data, ctx, kind, shared.next)?;
    }
    Ok(ListPage {
        items: artifacts.as_seen_by(ctx, items).await?,
        owned_next,
        shared_next_cursor,
    })
}

/// 自分のものの続きの位置（`before_updated_at`・`before_id` は対で使う。片方だけなら先頭から）。
pub(crate) fn before_of(at: Option<DateTime<Utc>>, id: Option<Uuid>) -> Option<ArtifactPosition> {
    match (at, id) {
        (Some(at), Some(id)) => Some((at, id)),
        _ => None,
    }
}

/// 共有された分の続きの位置の束縛（絞った種類も入れる。スキルの一覧の位置をワークフローの一覧に渡すと、その位置より
/// 新しい共有されたワークフローを黙って読み飛ばすので断る）。
fn shared_position_kind(kind: Option<ArtifactKind>) -> data::ListPositionKind {
    data::ListPositionKind::ArtifactsShared {
        kind: kind.map(ArtifactKind::as_str),
    }
}

/// 共有された分の続きの位置を、テナントと一覧の種類に束縛して暗号化する（見えないものを指しうる・BUG-162）。
fn seal_shared(
    data: &DataStore,
    ctx: &AuthContext,
    kind: Option<ArtifactKind>,
    next: Option<ArtifactPosition>,
) -> Result<Option<String>, ApiError> {
    next.map(|(updated_at, id)| {
        data.seal_list_position(
            shared_position_kind(kind),
            ctx.tenant_id(),
            data::ListPosition { updated_at, id },
        )
    })
    .transpose()
    .map_err(Into::into)
}

#[cfg(test)]
#[path = "artifact_list_it.rs"]
mod artifact_list_it;

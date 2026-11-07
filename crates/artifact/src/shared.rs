//! 自分に**共有された**アーティファクトの一覧（owner 除外・kind 絞り込み・更新日降順・keyset）。
//!
//! FGA `list_objects`（viewer の実効集合＝ロール継承込み）→ DB 突き合わせの二段。tenant 名前空間の突き合わせ
//! （strip_object_id）と DB 側 tenant/org フィルタの二重防御。
//!
//! 実効集合は持ち主の分も数えるので、たくさん作った人は ListObjects の上限に届く。届いたら実効集合を正にせず、
//! DB の候補（自分が持ち主でないもの・新しい順）を OpenFGA で絞る（BUG-149・[`crate::visible`]）。1 回の頼みで問うのは
//! [`crate::visible::MAX_CHECKS`] 問までなので、届いたら見つけた分と続きの位置を返す（BUG-162・ページが `limit` より
//! 少なくても続きがある）。続きの位置は見えないアーティファクトを指しうるので、外へ出すときは API がテナントに束縛して
//! 暗号化する。

use std::collections::HashSet;

use authz::{AuthContext, ObjectType, Relation};
use uuid::Uuid;

use crate::model::ArtifactKind;
use crate::store::{artifact_columns, ArtifactPage, ArtifactPosition, ArtifactRow, ArtifactStore};
use crate::visible::{Candidate, CHUNK, MAX_CHECKS};
use crate::{map_db, ArtifactError};

impl ArtifactStore {
    /// 自分に共有されたアーティファクトの 1 ページ（`after` は前のページの [`ArtifactPage::next`]）。
    pub async fn list_shared_page(
        &self,
        ctx: &AuthContext,
        kind: Option<ArtifactKind>,
        after: Option<ArtifactPosition>,
        limit: i64,
    ) -> Result<ArtifactPage, ArtifactError> {
        let limit = usize::try_from(limit.clamp(1, 100)).unwrap_or(1);
        let objs = self
            .authz
            .list_objects(&ctx.subject(), Relation::Viewer, ObjectType::Artifact)
            .await
            .map_err(|e| ArtifactError::Internal(e.to_string()))?;
        let cut = authz::visible::list_may_be_cut(objs.len());
        let ids = crate::visible::local_ids(ctx, &objs);
        if cut {
            return self.scan_shared_with_me(ctx, kind, after, limit, ids).await;
        }
        if ids.is_empty() {
            return Ok(ArtifactPage {
                items: Vec::new(),
                next: None,
            });
        }
        // 1 件多く引いて、続きがあるかを決める（続きの位置は返した最後の 1 件）。
        let rows: Vec<ArtifactRow> = sqlx::query_as(concat!(
            "SELECT ",
            artifact_columns!(),
            " FROM artifact \
             WHERE tenant_id = $1 AND org = $2 AND id = ANY($3) AND owner <> $4 \
               AND deleted_at IS NULL AND ($5::text IS NULL OR kind = $5) \
               AND ($6::timestamptz IS NULL OR (updated_at, id) < ($6, $7::uuid)) \
             ORDER BY updated_at DESC, id DESC LIMIT $8"
        ))
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(&ids)
        .bind(&ctx.principal().id)
        .bind(kind.map(ArtifactKind::as_str))
        .bind(after.map(|(at, _)| at))
        .bind(after.map(|(_, id)| id))
        .bind(i64::try_from(limit + 1).unwrap_or(101))
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        let mut items = rows
            .into_iter()
            .map(ArtifactRow::into_artifact)
            .collect::<Result<Vec<_>, _>>()?;
        let more = items.len() > limit;
        items.truncate(limit);
        let next = if more {
            items.last().map(|a| (a.updated_at, a.id))
        } else {
            None
        };
        Ok(ArtifactPage { items, next })
    }

    /// [`Self::list_shared_page`] の、ListObjects が上限に届いたときの経路。自分が持ち主でない候補を新しい順に
    /// 少しずつ引き、ListObjects が返した分は問い直さずに、それ以外を OpenFGA に問う（問うのは [`MAX_CHECKS`] 問まで）。
    async fn scan_shared_with_me(
        &self,
        ctx: &AuthContext,
        kind: Option<ArtifactKind>,
        after: Option<ArtifactPosition>,
        limit: usize,
        known: Vec<Uuid>,
    ) -> Result<ArtifactPage, ArtifactError> {
        let known: HashSet<Uuid> = known.into_iter().collect();
        let mut picked: Vec<Uuid> = Vec::new();
        let mut at = after;
        let mut checks = 0usize;
        let next = loop {
            let chunk: Vec<Candidate> = sqlx::query_as(
                "SELECT id, owner, updated_at FROM artifact \
                 WHERE tenant_id = $1 AND org = $2 AND owner <> $3 AND deleted_at IS NULL \
                   AND ($4::text IS NULL OR kind = $4) \
                   AND ($5::timestamptz IS NULL OR (updated_at, id) < ($5, $6::uuid)) \
                 ORDER BY updated_at DESC, id DESC LIMIT $7",
            )
            .bind(ctx.tenant_id())
            .bind(ctx.org())
            .bind(&ctx.principal().id)
            .bind(kind.map(ArtifactKind::as_str))
            .bind(at.map(|(t, _)| t))
            .bind(at.map(|(_, id)| id))
            .bind(i64::try_from(CHUNK).unwrap_or(i64::MAX))
            .fetch_all(&self.db)
            .await
            .map_err(map_db)?;
            checks += chunk.iter().filter(|c| !known.contains(&c.id)).count();
            let flags = self.visible_flags(ctx, &chunk, &known).await?;
            let mut filled = None;
            for (c, visible) in chunk.iter().zip(flags) {
                if visible {
                    picked.push(c.id);
                    if picked.len() >= limit {
                        // 1 ページ分埋まった: 最後に選んだものの後ろから続ける。
                        filled = Some((c.updated_at, c.id));
                        break;
                    }
                }
            }
            if filled.is_some() {
                break filled;
            }
            at = chunk.last().map(|c| (c.updated_at, c.id)).or(at);
            if chunk.len() < CHUNK {
                break None;
            }
            if checks >= MAX_CHECKS {
                tracing::warn!(
                    checks,
                    found = picked.len(),
                    "共有された一覧: OpenFGA に問う数の上限に届いたので、見つけた分と続きの位置を返す"
                );
                break at;
            }
        };
        if picked.is_empty() {
            return Ok(ArtifactPage {
                items: Vec::new(),
                next,
            });
        }
        let rows: Vec<ArtifactRow> = sqlx::query_as(concat!(
            "SELECT ",
            artifact_columns!(),
            " FROM artifact \
             WHERE tenant_id = $1 AND org = $2 AND id = ANY($3) AND deleted_at IS NULL"
        ))
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(&picked)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        let items = rows
            .into_iter()
            .map(ArtifactRow::into_artifact)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ArtifactPage {
            items: crate::visible::in_order(&picked, items, |a| a.id),
            next,
        })
    }
}

//! 見えるアーティファクトの選び方（BUG-149）。
//!
//! 一覧を OpenFGA の ListObjects の実効集合だけで絞ると、応答の上限（[`authz::visible::LIST_OBJECTS_MAX_RESULTS`]）
//! で黙って切れる（持ち主の分も数えるので、たくさん作った人ほど共有された分が欠ける）。ここでは DB の候補
//! （テナント・組織・種類などで絞ったもの・並びの順）を、持ち主の列か OpenFGA の判定（BatchCheck）で絞る。
//!
//! - 本文の参照で絞る一覧（[`crate::store::ArtifactStore::list_visible_with_body_ref`] など）は候補が少ないので、
//!   はじめから DB の候補を絞る（ListObjects を引かない）。
//! - 共有された一覧（[`crate::store::ArtifactStore::list_shared_page`]）は候補が多いので、ListObjects が上限より
//!   少なく返したときはそれを正にし、上限に届いたときだけ DB の候補を絞る。
//!
//! どちらも OpenFGA に問うのは 1 回の頼みで [`MAX_CHECKS`] 問までにし、届いたら見つけた分を返す（一覧は先頭の
//! 新しいものから埋まるので、欠けるのは古い方）。

use std::collections::HashSet;

use authz::{AuthContext, Consistency, Relation};
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::store::ArtifactStore;
use crate::ArtifactError;

/// 1 回に絞る候補の件数。
pub(crate) const CHUNK: usize = 200;
/// 1 回の頼みで OpenFGA に問う数の上限（BatchCheck は 50 問ずつ 4 本並べて送る）。
pub(crate) const MAX_CHECKS: usize = 1_000;

/// 一覧の候補（id・持ち主・並びのキーだけ。本文やメタは選んだ後に読む）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct Candidate {
    pub(crate) id: Uuid,
    pub(crate) owner: String,
    pub(crate) updated_at: DateTime<Utc>,
}

/// 候補を絞った結果。
#[derive(Debug, Default)]
pub(crate) struct Picked {
    /// 見える候補の id（候補の順）。
    pub(crate) ids: Vec<Uuid>,
    /// OpenFGA に問うた数。
    pub(crate) checks: usize,
}

impl ArtifactStore {
    /// 候補のそれぞれが見えるか（入力順）。持ち主の列が自分なら見える（これまでの一覧と同じ）。`known` は
    /// ListObjects が返した viewer の分（問い直さない）。それ以外は viewer を OpenFGA に問う。
    pub(crate) async fn visible_flags(
        &self,
        ctx: &AuthContext,
        chunk: &[Candidate],
        known: &HashSet<Uuid>,
    ) -> Result<Vec<bool>, ArtifactError> {
        let ns = ctx.ns();
        let trusted = |c: &Candidate| c.owner == ctx.principal.id || known.contains(&c.id);
        let ask: Vec<authz::FgaObject> = chunk
            .iter()
            .filter(|c| !trusted(c))
            .map(|c| ns.artifact(&c.id.to_string()))
            .collect();
        let mut held = authz::visible::holds_any(
            self.authz.as_ref(),
            &ctx.subject(),
            &[Relation::Viewer],
            &ask,
            Consistency::HigherConsistency,
        )
        .await
        .map_err(|e| ArtifactError::Internal(format!("authz: {e}")))?
        .into_iter();
        Ok(chunk
            .iter()
            .map(|c| trusted(c) || held.next().unwrap_or(false))
            .collect())
    }

    /// 並びの順の候補から、見えるものを先頭から `limit` 件まで選ぶ（OpenFGA に問うのは [`MAX_CHECKS`] 問まで）。
    pub(crate) async fn pick_visible(
        &self,
        ctx: &AuthContext,
        candidates: &[Candidate],
        limit: usize,
    ) -> Result<Picked, ArtifactError> {
        let mut picked = Picked::default();
        let none = HashSet::new();
        for chunk in candidates.chunks(CHUNK) {
            if picked.checks >= MAX_CHECKS {
                tracing::warn!(
                    checks = picked.checks,
                    found = picked.ids.len(),
                    "アーティファクトの一覧: OpenFGA に問う数の上限に届いたので見つけた分を返す"
                );
                break;
            }
            picked.checks += chunk.iter().filter(|c| c.owner != ctx.principal.id).count();
            let flags = self.visible_flags(ctx, chunk, &none).await?;
            for (c, visible) in chunk.iter().zip(flags) {
                if visible {
                    picked.ids.push(c.id);
                    if picked.ids.len() >= limit {
                        return Ok(picked);
                    }
                }
            }
        }
        Ok(picked)
    }
}

/// ListObjects の応答（`artifact:<tenant>|<uuid>`）から、このテナントの id を取り出す（テナントの違う識別子・
/// 読めない id は落とす）。
pub(crate) fn local_ids(ctx: &AuthContext, objs: &[String]) -> Vec<Uuid> {
    let ns = ctx.ns();
    objs.iter()
        .filter_map(|o| {
            let (_, id_part) = o.split_once(':')?;
            Uuid::parse_str(ns.strip_object_id(id_part)?).ok()
        })
        .collect()
}

/// `ids` の順に並べ直す（その間に消えたものは落とす）。
pub(crate) fn in_order<T>(ids: &[Uuid], items: Vec<T>, id_of: impl Fn(&T) -> Uuid) -> Vec<T> {
    let mut by_id: std::collections::HashMap<Uuid, T> =
        items.into_iter().map(|t| (id_of(&t), t)).collect();
    ids.iter().filter_map(|id| by_id.remove(id)).collect()
}

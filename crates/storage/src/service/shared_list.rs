//! StorageService: 自分に共有されたノードの一覧（ドライブの「共有されたもの」）。
//!
//! OpenFGA の ListObjects（viewer の実効集合・継承込み）で id を引き、DB で生存ノードのメタへ keyset
//! `(updated_at, id)` 降順で 1 ページ解決する。作成者本人のノードは除く。
//!
//! # ListObjects の上限（BUG-162）
//!
//! ListObjects の応答には上限がある（[`authz::visible::LIST_OBJECTS_MAX_RESULTS`]）。実効集合は自分が持ち主の
//! ノードと共有されたフォルダの中身も数えるので、ファイルの多い人ほど共有された分が黙って欠ける。file か folder の
//! 応答が上限に届いたら実効集合を正にせず、DB の候補（組織・テナントの生きている、自分が作っていないノード・並びの
//! 順）を [`SHARED_SCAN_CHUNK`] 件ずつ引いて [`authz::visible::holds_any`]（BatchCheck）で絞る（二段の認可は DB の
//! 候補が pre-filter、OpenFGA の判定が post-filter）。ListObjects が返した分は問い直さない。
//! 見えないノードばかりが続く組織で OpenFGA を叩きすぎないよう、1 回の頼みで問うのは [`SHARED_SCAN_MAX_CHECKS`]
//! 問までにし、届いたら見つけた分と続きの位置を返す（ページが `limit` より少なくても、空でも続きがある）。
//! 続きの位置は見えないノードを指しうるので、API がテナントに束縛して暗号化する（[`SharedPosition`]）。

// 分割した impl ブロック。親 `service.rs` の struct/フィールド/自由関数/型 import を総取りする。
#[allow(clippy::wildcard_imports)]
use super::*;
use crate::audit::AuditAction;
use crate::model::{SharedPage, SharedPosition};

/// DB の候補を OpenFGA で絞る経路で、1 回に引く候補の件数。
pub const SHARED_SCAN_CHUNK: i64 = 200;
/// DB の候補を OpenFGA で絞る経路で、1 回の頼みに OpenFGA へ問う数の上限（ListObjects が返した分は問わない）。
pub const SHARED_SCAN_MAX_CHECKS: usize = 1_000;

/// DB の候補を並びの順に引く文（[`StorageService::list_shared_with_me`] の上限に届いた経路）。
///
/// 引数は `$1` = org・`$2` = tenant_id・`$3` = 自分・`$4`・`$5` = 続きの位置（`updated_at`・`id`。無ければ NULL）・
/// `$6` = 件数。並びの索引（migration 0142 の `node_shared_scan_idx`）を逆にたどって打ち切る前提で、計画の回帰
/// テストが EXPLAIN する（引くたびに組織の生きているノードをすべて読んで並べ直す形に戻さない）。
#[must_use]
pub fn shared_scan_sql() -> String {
    format!(
        "SELECT {NODE_COLS} FROM node \
         WHERE org = $1 AND tenant_id = $2 AND deleted_at IS NULL AND created_by <> $3 \
           AND ($4::timestamptz IS NULL OR (updated_at, id) < ($4::timestamptz, $5)) \
         ORDER BY updated_at DESC, id DESC LIMIT $6"
    )
}

impl StorageService {
    /// 自分に共有されたノード一覧（自分が作成したものを除く・org+tenant スコープ・新しい順）。
    ///
    /// `after` は前のページの [`SharedPage::next`]。全件取得はせず、`next` で無限スクロールする。
    pub async fn list_shared_with_me(
        &self,
        ctx: &AuthContext,
        after: Option<SharedPosition>,
        limit: usize,
        trace_id: Option<&str>,
    ) -> Result<SharedPage, StorageError> {
        let limit = limit.clamp(1, 100);
        let subject = ctx.subject();
        let mut known: Vec<Uuid> = Vec::new();
        let mut cut = false;
        for object_type in [ObjectType::File, ObjectType::Folder] {
            let objs = self
                .authz
                .list_objects(&subject, Relation::Viewer, object_type)
                .await?;
            cut |= authz::visible::list_may_be_cut(objs.len());
            for o in objs {
                // "file:<tenant>|<uuid>" / "folder:<tenant>|<uuid>" から自テナントの id 部を取り出す。
                // strip_object_id が tenant 不一致を弾くため、共用ストアでも越境オブジェクトは混入しない
                // （FGA 側の名前空間化に加え、DB 側 org+tenant フィルタと二重防御）。
                let Some((_, id_part)) = o.split_once(':') else {
                    continue;
                };
                if let Some(local) = ctx.ns().strip_object_id(id_part) {
                    if let Ok(uuid) = Uuid::parse_str(local) {
                        known.push(uuid);
                    }
                }
            }
        }
        let (rows, next) = if cut {
            self.scan_shared_with_me(ctx, after, limit, known).await?
        } else {
            self.shared_in_set(ctx, after, limit, &known).await?
        };
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: AuditAction::NodeSharedWithMeList,
                    object_type: "organization",
                    object_id: ctx.org(),
                    decision: Decision::Allow,
                    trace_id,
                    metadata: json!({ "count": rows.len() }),
                },
            )
            .await?;
        let items = rows
            .into_iter()
            .map(row_to_node)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SharedPage { items, next })
    }

    /// ListObjects が上限より少なく返したときの経路: viewer の集合（id）を DB メタへ keyset で解決する。
    async fn shared_in_set(
        &self,
        ctx: &AuthContext,
        after: Option<SharedPosition>,
        limit: usize,
        ids: &[Uuid],
    ) -> Result<(Vec<NodeRow>, Option<SharedPosition>), StorageError> {
        if ids.is_empty() {
            return Ok((Vec::new(), None));
        }
        let sql = format!(
            "SELECT {NODE_COLS} FROM node \
             WHERE id = ANY($1) AND org = $2 AND tenant_id = $3 \
               AND deleted_at IS NULL AND created_by <> $4 \
               AND ($5::timestamptz IS NULL OR (updated_at, id) < ($5::timestamptz, $6)) \
             ORDER BY updated_at DESC, id DESC LIMIT $7"
        );
        let rows: Vec<NodeRow> = sqlx::query_as(&sql)
            .bind(ids)
            .bind(ctx.org())
            .bind(ctx.tenant_id())
            .bind(&ctx.principal().id)
            .bind(after.map(|p| p.updated_at))
            .bind(after.map(|p| p.id))
            .bind(i64::try_from(limit).unwrap_or(100))
            .fetch_all(&self.db)
            .await?;
        let next = if rows.len() == limit {
            rows.last().map(position_of)
        } else {
            None
        };
        Ok((rows, next))
    }

    /// ListObjects が上限に届いたときの経路（[モジュール文書](self)）。`known` は ListObjects が返した viewer の分
    /// （問い直さない）。それ以外の候補は viewer を OpenFGA に問う（[`SHARED_SCAN_MAX_CHECKS`] 問まで）。
    async fn scan_shared_with_me(
        &self,
        ctx: &AuthContext,
        after: Option<SharedPosition>,
        limit: usize,
        known: Vec<Uuid>,
    ) -> Result<(Vec<NodeRow>, Option<SharedPosition>), StorageError> {
        let known: std::collections::HashSet<Uuid> = known.into_iter().collect();
        let ns = ctx.ns();
        let subject = ctx.subject();
        let sql = shared_scan_sql();
        let mut picked: Vec<NodeRow> = Vec::new();
        let mut at = after;
        let mut checks = 0usize;
        loop {
            let chunk: Vec<NodeRow> = sqlx::query_as(&sql)
                .bind(ctx.org())
                .bind(ctx.tenant_id())
                .bind(&ctx.principal().id)
                .bind(at.map(|p| p.updated_at))
                .bind(at.map(|p| p.id))
                .bind(SHARED_SCAN_CHUNK)
                .fetch_all(&self.db)
                .await?;
            let exhausted = chunk.len() < usize::try_from(SHARED_SCAN_CHUNK).unwrap_or(200);
            let mut ask: Vec<FgaObject> = Vec::new();
            for r in chunk.iter().filter(|r| !known.contains(&r.id)) {
                let kind = NodeKind::parse(&r.kind).ok_or_else(|| {
                    StorageError::Integrity(format!("未知のノード種別: {}", r.kind))
                })?;
                ask.push(node_fga_object(&ns, kind, r.id));
            }
            checks += ask.len();
            let mut held = authz::visible::holds_any(
                self.authz.as_ref(),
                &subject,
                &[Relation::Viewer],
                &ask,
                Consistency::HigherConsistency,
            )
            .await?
            .into_iter();
            at = chunk.last().map(position_of).or(at);
            for r in chunk {
                let visible = known.contains(&r.id) || held.next().unwrap_or(false);
                if !visible {
                    continue;
                }
                picked.push(r);
                if picked.len() >= limit {
                    // 1 ページ分埋まった: 最後に選んだものの後ろから続ける（その後ろの候補は次のページで問い直す）。
                    let next = picked.last().map(position_of);
                    return Ok((picked, next));
                }
            }
            if exhausted {
                return Ok((picked, None));
            }
            if checks >= SHARED_SCAN_MAX_CHECKS {
                tracing::warn!(
                    checks,
                    found = picked.len(),
                    "共有されたもの: OpenFGA に問う数の上限に届いたので、見つけた分と続きの位置を返す"
                );
                return Ok((picked, at));
            }
        }
    }
}

fn position_of(r: &NodeRow) -> SharedPosition {
    SharedPosition {
        updated_at: r.updated_at,
        id: r.id,
    }
}

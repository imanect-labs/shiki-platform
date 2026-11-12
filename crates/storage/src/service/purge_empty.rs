//! StorageService: 「ゴミ箱を空にする」と、ゴミ箱の一覧の完全削除の添え物（GAP-108b・D-201）。
//!
//! 「空にする」で消えるのは、**呼んだ人のゴミ箱の一覧に出ているもの**（[`StorageService::list_trash`] と同じ条件:
//! システム領域でない削除の根で、呼んだ人が editor）のうち、呼んだ人が消せるもの（根の owner か組織の管理者）
//! だけ。一覧に出ない他人の根は、管理者が呼んでも消さない（管理者を閲覧に連鎖させない・PIT-65）。一覧に出るが
//! 消せないもの（editor だが owner でも管理者でもない）は数えて残す。
//!
//! 古い順（`deleted_at`, `id` の昇順）に keyset で進め、1 回の呼び出しで消す根・消すノードの合計・確かめる根の
//! 候補に上限を置く。上限に届いたら、最後に確かめた候補の位置を続きとして返す（`next_cursor` の間は繰り返す）。
//!
//! 消す範囲は、**最初の呼び出し（続きの位置なし）の時点でゴミ箱にあったもの**に限る。最初の呼び出しで上限の時刻
//! （DB の `now()`）を決め、`deleted_at` がそれより後の根は、同じ呼び出しの後のバッチでも続きの呼び出しでも
//! 消さない（大きなゴミ箱を空にする数分の間に、確認の後でゴミ箱に入れたものを、元に戻す機会なしに消さない）。
//! 上限は続きの位置に入れて渡す（位置は不透明な文字列のまま）。利用者が位置を書き換えて上限を後ろにずらしても、
//! 位置なしで呼べば消せる範囲より広がらないので、認可の問題にはならない。

#[allow(clippy::wildcard_imports)]
use super::*;

use authz::CheckTuple;

use super::purge::{PurgeOutcome, INLINE_CLEANUP_MAX};
use crate::audit::AuditAction;

/// 「空にする」1 回で完全に削除する根の上限。
pub const EMPTY_TRASH_MAX_ROOTS: usize = 100;

/// 「空にする」1 回で完全に削除するノード（配下を含む）の合計の上限。届いたら次の根へ進まずに続きを返す
/// （1 つの根はその大きさによらず最後まで消す）。
///
/// 手元の計測（2 コア・ほかのビルドと同居・node と node_version が数万行の DB）で、1 つのフォルダの完全削除は
/// 配下 500 件で約 1.8 秒・2,000 件で約 3.6 秒・5,000 件で約 9 秒（片付けをその場で 300 行まで含む）。表が大きいほど
/// 外部キーの確かめ（node.parent_id・node_version.blob_sha256 に全件の索引が無い）が伸びるので、1 回の呼び出しを
/// 数秒に収める 2,000 件にする。
pub const EMPTY_TRASH_MAX_NODES: usize = 2_000;

/// 「空にする」1 回で確かめる根の候補の上限（OpenFGA に問う数を抑える）。
const EMPTY_TRASH_MAX_SCAN: usize = 200;

/// 1 回の問い合わせで引く根の候補の数。
const EMPTY_TRASH_BATCH: i64 = 50;

/// [`StorageService::empty_trash`] の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmptyTrashOutcome {
    /// 完全に削除した根の数。
    pub purged: usize,
    /// 一覧に出ているが消さずに残した根の数（持ち主でも管理者でもない・中に生きているノードがある）。
    pub skipped: usize,
    /// 続きがあれば次の呼び出しに渡す位置（無ければ `None`）。
    pub next_cursor: Option<String>,
}

/// ゴミ箱の一覧の 1 項目に添える、完全削除の情報。
#[derive(Debug, Clone)]
pub struct TrashEntry {
    pub node: Node,
    /// 呼んだ人が完全に削除できるか（根の owner か組織の管理者）。表示のための値で、削除の口が毎回確かめ直す。
    pub can_purge: bool,
    /// フォルダなら、配下のファイルの数（ファイルなら `None`）。
    pub file_count: Option<i64>,
    /// フォルダなら、配下のファイルのうち呼んだ人が作ったのでないものの数（ファイルなら `None`）。
    pub others_file_count: Option<i64>,
}

/// [`StorageService::list_trash_entries`] の 1 ページ。
#[derive(Debug, Clone)]
pub struct TrashPage {
    pub items: Vec<TrashEntry>,
    pub next_cursor: Option<String>,
}

/// 「空にする」の根の候補。
#[derive(sqlx::FromRow)]
struct Candidate {
    id: Uuid,
    kind: String,
    deleted_at: DateTime<Utc>,
}

impl StorageService {
    /// ゴミ箱の一覧（[`Self::list_trash`]）に、完全に削除できるかと、フォルダの配下のファイルの数を添える。
    ///
    /// `can_purge` は、管理者なら 1 回の check、そうでなければ owner の BatchCheck でまとめて引く。配下の数は 1 ページ
    /// に 1 回の集計（表示のためで、認可には使わない）。
    pub async fn list_trash_entries(
        &self,
        ctx: &AuthContext,
        cursor: Option<&str>,
        limit: usize,
        trace_id: Option<&str>,
    ) -> Result<TrashPage, StorageError> {
        let page = self.list_trash(ctx, cursor, limit, trace_id).await?;
        if page.items.is_empty() {
            return Ok(TrashPage {
                items: Vec::new(),
                next_cursor: page.next_cursor,
            });
        }
        let can = self
            .purgeable(
                ctx,
                &page
                    .items
                    .iter()
                    .map(|n| (n.kind, n.id))
                    .collect::<Vec<_>>(),
                Consistency::MinimizeLatency,
            )
            .await?;
        let folders: Vec<Uuid> = page
            .items
            .iter()
            .filter(|n| n.kind == NodeKind::Folder)
            .map(|n| n.id)
            .collect();
        let counts: Vec<(Uuid, i64, i64)> = if folders.is_empty() {
            Vec::new()
        } else {
            sqlx::query_as(
                "SELECT c.ancestor, count(*), count(*) FILTER (WHERE n.created_by <> $3) \
                 FROM node_closure c JOIN node n ON n.tenant_id = c.tenant_id AND n.id = c.descendant \
                 WHERE c.tenant_id = $1 AND n.org = $2 AND c.ancestor = ANY($4) AND n.kind = 'file' \
                 GROUP BY c.ancestor",
            )
            .bind(ctx.tenant_id())
            .bind(ctx.org())
            .bind(&ctx.principal().id)
            .bind(&folders)
            .fetch_all(&self.db)
            .await?
        };
        let items = page
            .items
            .into_iter()
            .zip(can)
            .map(|(node, can_purge)| {
                let (file_count, others_file_count) = match node.kind {
                    NodeKind::File => (None, None),
                    NodeKind::Folder => counts
                        .iter()
                        .find(|(id, _, _)| *id == node.id)
                        .map_or((Some(0), Some(0)), |(_, f, o)| (Some(*f), Some(*o))),
                };
                TrashEntry {
                    node,
                    can_purge,
                    file_count,
                    others_file_count,
                }
            })
            .collect();
        Ok(TrashPage {
            items,
            next_cursor: page.next_cursor,
        })
    }

    /// `nodes` のそれぞれを呼んだ人が完全に削除できるか（同じ順）。管理者なら 1 回の check で全部、そうでなければ
    /// owner の BatchCheck。
    async fn purgeable(
        &self,
        ctx: &AuthContext,
        nodes: &[(NodeKind, Uuid)],
        consistency: Consistency,
    ) -> Result<Vec<bool>, StorageError> {
        if nodes.is_empty() {
            return Ok(Vec::new());
        }
        if self.is_org_admin(ctx, consistency).await? {
            return Ok(vec![true; nodes.len()]);
        }
        let subject = ctx.subject();
        let ns = ctx.ns();
        let items: Vec<CheckTuple> = nodes
            .iter()
            .map(|(kind, id)| CheckTuple {
                subject: subject.clone(),
                relation: Relation::Owner,
                object: node_fga_object(&ns, *kind, *id),
            })
            .collect();
        Ok(self.authz.batch_check(&items, consistency).await?)
    }

    /// 呼んだ人のゴミ箱の一覧に出ている根のうち、呼んだ人が消せるものを古い順に完全に削除する（D-201）。
    ///
    /// 上限（[`EMPTY_TRASH_MAX_ROOTS`]・[`EMPTY_TRASH_MAX_NODES`]・確かめる候補の数）に届いたら、続きの位置を
    /// [`EmptyTrashOutcome::next_cursor`] に返す。呼び出しごとに `trash.empty` を 1 行（チェーンしない・数だけ）、
    /// 消した根ごとに `file.purge` / `folder.purge`（チェーンする）を監査に残す。
    pub async fn empty_trash(
        &self,
        ctx: &AuthContext,
        cursor: Option<&str>,
        trace_id: Option<&str>,
    ) -> Result<EmptyTrashOutcome, StorageError> {
        // ゴミ箱を見られるのは org のメンバー（一覧と同じ）。
        self.require(
            ctx,
            Relation::Member,
            &ctx.ns().organization(ctx.org()),
            AuditAction::TrashEmpty,
            "organization",
            ctx.org(),
            trace_id,
        )
        .await?;
        // 続きなら位置と上限の時刻を受け継ぐ。最初の呼び出しなら、上限をいまの時刻（DB の時計）にする。
        let (mut after, upper): (Option<TrashPos>, DateTime<Utc>) = match cursor {
            Some(c) => {
                let (pos, upper) = decode_empty_cursor(c)?;
                (Some(pos), upper)
            }
            None => (
                None,
                sqlx::query_scalar("SELECT now()")
                    .fetch_one(&self.db)
                    .await?,
            ),
        };
        // 元に戻せない操作なので、剥奪をすぐに効かせる（PIT-11）。
        let is_admin = self
            .is_org_admin(ctx, Consistency::HigherConsistency)
            .await?;
        let mut out = EmptyTrashOutcome {
            purged: 0,
            skipped: 0,
            next_cursor: None,
        };
        let mut nodes = 0usize;
        let mut scanned = 0usize;
        let mut budget = INLINE_CLEANUP_MAX;
        let mut failure: Option<StorageError> = None;
        let mut exhausted = false;
        'scan: loop {
            let batch = self.trash_candidates(ctx, after, upper).await?;
            let short = (batch.len() as i64) < EMPTY_TRASH_BATCH;
            let (listed, can) = self.empty_checks(ctx, &batch, is_admin).await?;
            for (i, cand) in batch.iter().enumerate() {
                after = Some((cand.deleted_at, cand.id));
                scanned += 1;
                if listed[i] {
                    let kind = NodeKind::parse(&cand.kind).unwrap_or(NodeKind::File);
                    if can[i] {
                        match self
                            .purge_locked(ctx, cand.id, kind, trace_id, &mut budget)
                            .await
                        {
                            Ok(PurgeOutcome::Purged(r)) => {
                                out.purged += 1;
                                nodes += r.nodes;
                            }
                            Ok(_) => out.skipped += 1,
                            // 並行して消えた（別の「空にする」・完全削除）。数えない。
                            Err(StorageError::NotFound) => {}
                            Err(e) => {
                                failure = Some(e);
                                break 'scan;
                            }
                        }
                    } else {
                        out.skipped += 1;
                    }
                }
                let limit_hit = out.purged >= EMPTY_TRASH_MAX_ROOTS
                    || nodes >= EMPTY_TRASH_MAX_NODES
                    || scanned >= EMPTY_TRASH_MAX_SCAN;
                if limit_hit {
                    exhausted = short && i + 1 == batch.len();
                    break 'scan;
                }
            }
            if short {
                exhausted = true;
                break;
            }
        }
        if !exhausted && failure.is_none() {
            out.next_cursor = after.map(|(ts, id)| encode_empty_cursor(ts, id, upper));
        }
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: AuditAction::TrashEmpty,
                    object_type: "organization",
                    object_id: ctx.org(),
                    decision: Decision::Allow,
                    trace_id,
                    metadata: json!({
                        "purged": out.purged,
                        "skipped": out.skipped,
                        "subtree_count": nodes,
                        "scanned": scanned,
                        "complete": out.next_cursor.is_none() && failure.is_none(),
                    }),
                },
            )
            .await?;
        match failure {
            Some(e) => Err(e),
            None => Ok(out),
        }
    }

    /// ゴミ箱の一覧と同じ条件（システム領域でない削除の根）の候補のうち、`upper` までに削除したものを、`after` の
    /// 次から古い順に引く。
    async fn trash_candidates(
        &self,
        ctx: &AuthContext,
        after: Option<(DateTime<Utc>, Uuid)>,
        upper: DateTime<Utc>,
    ) -> Result<Vec<Candidate>, StorageError> {
        Ok(sqlx::query_as(
            "SELECT n.id, n.kind, n.deleted_at FROM node n \
             WHERE n.org = $1 AND n.tenant_id = $2 AND n.deleted_at IS NOT NULL AND n.system = false \
               AND n.deleted_at <= $6 \
               AND NOT EXISTS ( \
                 SELECT 1 FROM node p \
                 WHERE p.tenant_id = n.tenant_id AND p.id = n.parent_id AND p.deleted_at IS NOT NULL) \
               AND ($3::timestamptz IS NULL OR (n.deleted_at, n.id) > ($3::timestamptz, $4)) \
             ORDER BY n.deleted_at, n.id LIMIT $5",
        )
        .bind(ctx.org())
        .bind(ctx.tenant_id())
        .bind(after.map(|(ts, _)| ts))
        .bind(after.map(|(_, id)| id))
        .bind(EMPTY_TRASH_BATCH)
        .bind(upper)
        .fetch_all(&self.db)
        .await?)
    }

    /// 候補ごとに、呼んだ人の一覧に出るか（editor）と、消せるか（owner か管理者）を BatchCheck でまとめて引く。
    async fn empty_checks(
        &self,
        ctx: &AuthContext,
        batch: &[Candidate],
        is_admin: bool,
    ) -> Result<(Vec<bool>, Vec<bool>), StorageError> {
        if batch.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let subject = ctx.subject();
        let ns = ctx.ns();
        let objects: Vec<FgaObject> = batch
            .iter()
            .map(|c| {
                node_fga_object(
                    &ns,
                    NodeKind::parse(&c.kind).unwrap_or(NodeKind::File),
                    c.id,
                )
            })
            .collect();
        let tuples = |relation: Relation| -> Vec<CheckTuple> {
            objects
                .iter()
                .map(|object| CheckTuple {
                    subject: subject.clone(),
                    relation,
                    object: object.clone(),
                })
                .collect()
        };
        // 一覧と同じく、即時の剥奪を反映する（強整合）。
        let listed = self
            .authz
            .batch_check(&tuples(Relation::Editor), Consistency::HigherConsistency)
            .await?;
        let can = if is_admin {
            vec![true; batch.len()]
        } else {
            self.authz
                .batch_check(&tuples(Relation::Owner), Consistency::HigherConsistency)
                .await?
        };
        Ok((listed, can))
    }
}

/// 「空にする」の位置（最後に確かめた候補の `deleted_at` と `id`）。
type TrashPos = (DateTime<Utc>, Uuid);

/// 「空にする」の続きの位置と上限の分け目（RFC 3339 の時刻には現れない）。
const UPPER_SEP: char = '|';

/// 「空にする」の続きの位置（最後に確かめた候補と、最初の呼び出しで決めた上限の時刻）を不透明な文字列にする。
fn encode_empty_cursor(deleted_at: DateTime<Utc>, id: Uuid, upper: DateTime<Utc>) -> String {
    encode_ts_cursor(
        &format!(
            "{}{UPPER_SEP}{}",
            deleted_at.to_rfc3339(),
            upper.to_rfc3339()
        ),
        id,
    )
}

/// [`encode_empty_cursor`] の逆。上限の無い位置（一覧の位置など）と壊れた位置は `Invalid`。
fn decode_empty_cursor(cursor: &str) -> Result<(TrashPos, DateTime<Utc>), StorageError> {
    let invalid = || StorageError::Invalid("カーソルが不正です".into());
    let (ts, id) = decode_ts_cursor(cursor)?;
    let (deleted_at, upper) = ts.split_once(UPPER_SEP).ok_or_else(invalid)?;
    let parse = |t: &str| {
        DateTime::parse_from_rfc3339(t)
            .map(|t| t.with_timezone(&Utc))
            .map_err(|_| invalid())
    };
    Ok(((parse(deleted_at)?, id), parse(upper)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_cursor_round_trips_with_upper() {
        let id = Uuid::new_v4();
        let deleted_at = DateTime::parse_from_rfc3339("2026-11-01T01:02:03.456789Z")
            .unwrap()
            .with_timezone(&Utc);
        let upper = DateTime::parse_from_rfc3339("2026-11-12T10:00:00.5Z")
            .unwrap()
            .with_timezone(&Utc);
        let c = encode_empty_cursor(deleted_at, id, upper);
        assert_eq!(decode_empty_cursor(&c).unwrap(), ((deleted_at, id), upper));
    }

    #[test]
    fn empty_cursor_rejects_cursor_without_upper() {
        // 一覧の位置（上限を持たない）や壊れた位置は受けない。
        let list = encode_ts_cursor("2026-11-01T01:02:03+00:00", Uuid::new_v4());
        assert!(matches!(
            decode_empty_cursor(&list),
            Err(StorageError::Invalid(_))
        ));
        let bad = encode_ts_cursor("2026-11-01T01:02:03+00:00|nope", Uuid::new_v4());
        assert!(matches!(
            decode_empty_cursor(&bad),
            Err(StorageError::Invalid(_))
        ));
        assert!(matches!(
            decode_empty_cursor("zz"),
            Err(StorageError::Invalid(_))
        ));
    }
}

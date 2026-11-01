//! StorageService: フォルダーの中身の一覧と名前の検索で、DB の候補を OpenFGA で絞る部分（BUG-187）。
//!
//! 候補（DB の行・並びの順・キーセット）を引き、BatchCheck（[`authz::visible::holds_any`]）でまとめて viewer を
//! 確かめる（二段の認可は DB の候補が pre-filter、OpenFGA の判定が post-filter）。1 回の頼みで OpenFGA に問う数は
//! [`NODE_SCAN_MAX_CHECKS`] 問までにし、届いたら見つけた分と、最後に確かめた候補の位置を続きの位置として返す
//! （ページが `limit` より少なくても、空でも続きがある・黙って欠かない）。続きの位置は見えないノードの名前を含み
//! うるので、外へ出す API がテナントと一覧の種類に束縛して暗号化する（`data::DataStore::seal_list_cursor`）。
//!
//! # 名前の検索
//!
//! ListObjects（file と folder の viewer）が上限より少なく返したら、その集合の中だけを SQL で名前に当てる（OpenFGA
//! に問わない・[`StorageService::search_in_visible`]）。どちらかが上限に届いたら（[`authz::visible::list_may_be_cut`]）、
//! 候補を語で選ばずに組織の生きているノードを並びの順に引き、名前に当たるかは見えると決まった候補にだけ使う（SQL は
//! 当たるかを列で返すだけ・[`ScanWhere::Named`]）。語で候補を選ぶと、当たる見えないノードがあるときだけ OpenFGA に
//! 問うことになり、応答の時間と「0 件で続きあり」から、見えないノードの名前に語が含まれるかが分かってしまう。
//! 問う数は語によらない（BUG-149 のテーブルの名前の検索と同じ考え方）。ListObjects が返した分は問い直さない。

use std::collections::HashSet;

// 分割した impl ブロック。親 `service.rs` の struct/フィールド/自由関数/型 import を総取りする。
#[allow(clippy::wildcard_imports)]
use super::*;

/// 1 回の頼みで OpenFGA に問う数の上限（フォルダーの中身の一覧・名前の検索・BUG-187）。
///
/// BatchCheck は 50 問ずつ 4 本並べて送る（[`authz::visible::holds_any`]）ので、上限まで問うと 5 往復ほど。
pub const NODE_SCAN_MAX_CHECKS: usize = 1_000;
/// 2 回目からの候補の件数（BatchCheck の 1 波＝50 問 × 4 本）。
const NODE_SCAN_CHUNK: usize = 200;
/// 1 回目の候補の件数の下限（小さいページでも、見えない候補が少し混ざるだけで 2 往復にしない）。
const NODE_SCAN_FIRST_MIN: usize = 16;

/// 候補の選び方。
pub(super) enum ScanWhere<'a> {
    /// フォルダーの子（`parent_id` が `None` なら組織の最上位）。`include_system` はシステム領域を含めるか（#392）。
    Children {
        parent_id: Option<Uuid>,
        include_system: bool,
    },
    /// 組織の生きているノード（システム領域を除く）。`terms`（ILIKE の模様）は候補を選ばず、当たるかの列にだけ使う。
    Named { terms: &'a [String] },
}

/// 候補の行と、名前の検索の語に当たるか（フォルダーの子は常に当たる）。
#[derive(sqlx::FromRow)]
struct ScanRow {
    #[sqlx(flatten)]
    node: NodeRow,
    hit: bool,
}

/// 並びの列式・型・向き・キーセットの比較（[`child_sort_value`] と同じ列式）。
struct Order {
    col: &'static str,
    cast: &'static str,
    dir: &'static str,
    cmp: &'static str,
}

fn order_of(sort: ChildSort) -> Order {
    let (col, cast) = match sort.key {
        ChildSortKey::Name => ("name", "text"),
        ChildSortKey::Updated => ("updated_at", "timestamptz"),
        // フォルダは size_bytes が NULL のため 0 とみなす（NULL を keyset から排除）。
        ChildSortKey::Size => ("coalesce(size_bytes, 0)", "bigint"),
    };
    let (dir, cmp) = if sort.desc {
        ("DESC", "<")
    } else {
        ("ASC", ">")
    };
    Order {
        col,
        cast,
        dir,
        cmp,
    }
}

/// 続きの位置（並びの値と id）を内部のカーソルにする（外へは API が封をして出す）。
fn encode_at(sort: ChildSort, at: Option<&(String, Uuid)>) -> Option<String> {
    at.map(|(v, id)| encode_child_cursor(sort, v, *id))
}

impl StorageService {
    /// 候補を並びの順に引き、BatchCheck で viewer を確かめて 1 ページを返す（[モジュール文書](self)）。
    ///
    /// `known` は ListObjects が viewer と返したノード（問い直さない）。ページの後ろに見えるものがあるかを確かめるため、
    /// `limit` + 1 件目まで探す（BUG-180）。返す続きの位置は、`limit` + 1 件目が見つかったら `limit` 件目、問う数の
    /// 上限に届いたら最後に確かめた候補（見えないことがある・後ろに見えるものが無いこともある）、候補が尽きたら
    /// `None`（ちょうど `limit` 件で尽きたときも `None`）。
    pub(super) async fn scan_nodes(
        &self,
        ctx: &AuthContext,
        filter: &ScanWhere<'_>,
        sort: ChildSort,
        cursor: Option<&str>,
        limit: usize,
        known: &HashSet<Uuid>,
    ) -> Result<(Vec<Node>, Option<String>), StorageError> {
        let limit = limit.clamp(1, 100);
        let ns = ctx.ns();
        let subject = ctx.subject();
        let mut at = cursor.map(|c| decode_child_cursor(sort, c)).transpose()?;
        let mut items: Vec<Node> = Vec::with_capacity(limit + 1);
        // 選んだ件の位置（`limit` + 1 件目が見つかったら、`limit` 件目の位置を続きにする）。
        let mut picked: Vec<(String, Uuid)> = Vec::with_capacity(limit + 1);
        let mut checks = 0usize;
        let mut first = true;
        loop {
            let want = limit + 1 - items.len();
            let n = if first {
                want.clamp(NODE_SCAN_FIRST_MIN, NODE_SCAN_CHUNK)
            } else {
                NODE_SCAN_CHUNK
            }
            .min(NODE_SCAN_MAX_CHECKS - checks);
            let rows = self
                .fetch_candidates(ctx, filter, sort, at.as_ref(), n)
                .await?;
            let exhausted = rows.len() < n;
            let ask: Vec<FgaObject> = rows
                .iter()
                .filter(|r| !known.contains(&r.node.id))
                .map(|r| {
                    // 種類の読めない行はファイルとして問う（フォルダーなら見えないと判定される・fail-closed）。
                    let kind = NodeKind::parse(&r.node.kind).unwrap_or(NodeKind::File);
                    node_fga_object(&ns, kind, r.node.id)
                })
                .collect();
            checks += ask.len();
            // 即時剥奪の反映のため強整合（これまでの単発の check と同じ）。
            let mut held = authz::visible::holds_any(
                self.authz.as_ref(),
                &subject,
                &[Relation::Viewer],
                &ask,
                Consistency::HigherConsistency,
            )
            .await?
            .into_iter();
            for r in rows {
                at = Some((child_sort_value(sort.key, &r.node), r.node.id));
                // `known` の行は問いに入れていない（問いの答えは known でない行の順に並ぶ）。
                let visible = known.contains(&r.node.id) || held.next().unwrap_or(false);
                if !visible || !r.hit {
                    continue;
                }
                picked.push((child_sort_value(sort.key, &r.node), r.node.id));
                items.push(row_to_node(r.node)?);
                if items.len() > limit {
                    // 後ろに見えるものがある: その 1 件は次のページで問い直す。
                    items.truncate(limit);
                    return Ok((items, encode_at(sort, picked.get(limit - 1))));
                }
            }
            if exhausted {
                return Ok((items, None));
            }
            if checks >= NODE_SCAN_MAX_CHECKS {
                tracing::warn!(
                    checks,
                    found = items.len(),
                    "ドライブの一覧: OpenFGA に問う数の上限に届いたので、見つけた分と続きの位置を返す"
                );
                return Ok((items, encode_at(sort, at.as_ref())));
            }
            first = false;
        }
    }

    /// 候補を `n` 件、並びの順に引く（`at` の後ろから）。
    async fn fetch_candidates(
        &self,
        ctx: &AuthContext,
        filter: &ScanWhere<'_>,
        sort: ChildSort,
        at: Option<&(String, Uuid)>,
        n: usize,
    ) -> Result<Vec<ScanRow>, StorageError> {
        let o = order_of(sort);
        let (col, cast, dir, cmp) = (o.col, o.cast, o.dir, o.cmp);
        let limit = i64::try_from(n).unwrap_or(i64::MAX);
        let after_val = at.map(|(v, _)| v.as_str());
        let after_id = at.map(|(_, id)| *id);
        let rows = match filter {
            ScanWhere::Children {
                parent_id,
                include_system,
            } => {
                // parent_id は IS NOT DISTINCT FROM で NULL（最上位）も同値比較する。after_val は text で受けて
                // 列型へキャストして比べる。
                let sql = format!(
                    "SELECT {NODE_COLS}, true AS hit FROM node \
                     WHERE org = $1 AND tenant_id = $2 AND deleted_at IS NULL \
                       AND ($7 OR system = false) \
                       AND parent_id IS NOT DISTINCT FROM $3 \
                       AND ($4::text IS NULL OR ({col}, id) {cmp} ($4::{cast}, $5)) \
                     ORDER BY {col} {dir}, id {dir} LIMIT $6"
                );
                sqlx::query_as(&sql)
                    .bind(&ctx.org)
                    .bind(&ctx.tenant_id)
                    .bind(parent_id)
                    .bind(after_val)
                    .bind(after_id)
                    .bind(limit)
                    .bind(include_system)
                    .fetch_all(&self.db)
                    .await?
            }
            ScanWhere::Named { terms } => {
                let sql = named_scan_sql(sort);
                sqlx::query_as(&sql)
                    .bind(&ctx.org)
                    .bind(&ctx.tenant_id)
                    .bind(terms)
                    .bind(after_val)
                    .bind(after_id)
                    .bind(limit)
                    .fetch_all(&self.db)
                    .await?
            }
        };
        Ok(rows)
    }

    /// 名前の検索で、ListObjects が上限より少なく返したときの経路: viewer の集合の中だけを名前に当てる（問わない）。
    #[allow(clippy::too_many_arguments)] // 認可 ctx ＋ 語 ＋ ソート ＋ ページング ＋ 見える集合。
    pub(super) async fn search_in_visible(
        &self,
        ctx: &AuthContext,
        terms: &[String],
        sort: ChildSort,
        cursor: Option<&str>,
        limit: usize,
        ids: &[Uuid],
    ) -> Result<(Vec<Node>, Option<String>), StorageError> {
        let limit = limit.clamp(1, 100);
        if ids.is_empty() {
            return Ok((Vec::new(), None));
        }
        let at = cursor.map(|c| decode_child_cursor(sort, c)).transpose()?;
        let o = order_of(sort);
        let (col, cast, dir, cmp) = (o.col, o.cast, o.dir, o.cmp);
        let sql = format!(
            "SELECT {NODE_COLS} FROM node \
             WHERE id = ANY($1) AND org = $2 AND tenant_id = $3 AND deleted_at IS NULL AND system = false \
               AND name ILIKE ALL($4) \
               AND ($5::text IS NULL OR ({col}, id) {cmp} ($5::{cast}, $6)) \
             ORDER BY {col} {dir}, id {dir} LIMIT $7"
        );
        let rows: Vec<NodeRow> = sqlx::query_as(&sql)
            .bind(ids)
            .bind(&ctx.org)
            .bind(&ctx.tenant_id)
            .bind(terms)
            .bind(at.as_ref().map(|(v, _)| v.as_str()))
            .bind(at.as_ref().map(|(_, id)| *id))
            // 後ろに見えるものがあるかを確かめるため 1 件多く引く（BUG-180）。
            .bind(i64::try_from(limit + 1).unwrap_or(101))
            .fetch_all(&self.db)
            .await?;
        let mut rows = rows;
        let next = if rows.len() > limit {
            rows.truncate(limit);
            rows.last()
                .map(|r| encode_child_cursor(sort, &child_sort_value(sort.key, r), r.id))
        } else {
            None
        };
        let items = rows
            .into_iter()
            .map(row_to_node)
            .collect::<Result<Vec<_>, _>>()?;
        Ok((items, next))
    }

    /// 自分が viewer のファイルとフォルダー（ListObjects）と、どちらかが上限に届いたか（届いたら集合を正にしない）。
    pub(super) async fn visible_nodes(
        &self,
        ctx: &AuthContext,
    ) -> Result<(HashSet<Uuid>, bool), StorageError> {
        let subject = ctx.subject();
        let (files, folders) = tokio::try_join!(
            self.authz
                .list_objects(&subject, Relation::Viewer, ObjectType::File),
            self.authz
                .list_objects(&subject, Relation::Viewer, ObjectType::Folder),
        )?;
        let cut = authz::visible::list_may_be_cut(files.len())
            || authz::visible::list_may_be_cut(folders.len());
        let ns = ctx.ns();
        let mut known = HashSet::with_capacity(files.len() + folders.len());
        for o in files.iter().chain(folders.iter()) {
            // "file:<tenant>|<uuid>" / "folder:<tenant>|<uuid>" から自テナントの id 部を取り出す（越境は混ぜない）。
            let Some((_, id_part)) = o.split_once(':') else {
                continue;
            };
            if let Some(local) = ns.strip_object_id(id_part) {
                if let Ok(id) = Uuid::parse_str(local) {
                    known.insert(id);
                }
            }
        }
        Ok((known, cut))
    }
}

/// 名前の検索で ListObjects が上限に届いたときに候補を引く文（[`ScanWhere::Named`]）。
///
/// 引数は `$1` = org・`$2` = tenant_id・`$3` = 語（ILIKE の模様）・`$4`・`$5` = 続きの位置（並びの値・id）・`$6` = 件数。
/// 語は `hit` の列にだけ使い、候補は選ばない（[モジュール文書](self)）。名前の順の索引（migration 0128 の
/// `node_name_scan_idx`）をたどって打ち切る前提で、計画の回帰テストが EXPLAIN する。
#[must_use]
pub fn named_scan_sql(sort: ChildSort) -> String {
    let o = order_of(sort);
    let (col, cast, dir, cmp) = (o.col, o.cast, o.dir, o.cmp);
    format!(
        "SELECT {NODE_COLS}, (name ILIKE ALL($3)) AS hit FROM node \
         WHERE org = $1 AND tenant_id = $2 AND deleted_at IS NULL AND system = false \
           AND ($4::text IS NULL OR ({col}, id) {cmp} ($4::{cast}, $5)) \
         ORDER BY {col} {dir}, id {dir} LIMIT $6"
    )
}

//! テーブル一覧（Task 9.2/9.8）。`store.rs` から分離（1 ファイル行数規約）。
//!
//! FGA viewer 実効集合 → DB 突合の二段（画面の一覧は追加だけ＝`submitter` の実効集合も足す・D-113）。
//! app-gateway の所有束縛（`list_app_tables`）は
//! `app_id` の絞り込みを LIMIT より前に SQL で行う（上限超過時の欠落を防ぐ）。
//!
//! 画面の一覧（`GET /data/tables`）は名前の検索と続きの位置で引く（[`DataStore::list_tables_page`]・GAP-42）。
//! 検索も続きも見えるテーブルの中だけを返すので、見えないテーブルの名前に当たることはない。
//!
//! # ListObjects の上限（BUG-149）
//!
//! ListObjects の応答には上限がある（[`authz::visible::LIST_OBJECTS_MAX_RESULTS`]）。応答が上限に届いたら
//! 実効集合を正にせず、DB の候補（テナント・組織・名前・続きの位置で絞ったもの・並びの順）を
//! [`TABLE_SCAN_CHUNK`] 件ずつ引いて [`authz::visible::holds_any`]（BatchCheck）で絞る。ListObjects が返した分は
//! 判定済みとして問い直さない。上限より少ない応答は全部とみなし、これまでどおり SQL で絞る（BatchCheck は
//! 1 往復が重く、上限の内では ListObjects 1 往復の方が速い）。
//! 見えないテーブルばかりが続く組織で OpenFGA を叩きすぎないよう、1 回の頼みで OpenFGA に問うのは
//! [`TABLE_SCAN_MAX_CHECKS`] 問までにし、届いたら見つけた分と続きの位置を返す（ページが `limit` より少なくても
//! 続きがある）。DB から引く候補は「選んだもの（`limit` + 1 件まで）か問うたもの」なので、これで DB の側も抑えられる。
//! 続きの位置は見えないテーブルを指しうるので暗号化する（[`crate::cursor::CursorCodec::seal_table_list`]）。
//!
//! この経路では、名前の検索の語で候補を選ばない。候補はテナント・組織・アプリ・続きの位置だけで選び、名前に当たるかは
//! 見えると決まった候補にだけ使う（SQL は当たるかを列で返すだけ）。語で候補を選ぶと、当たる見えないテーブルがあるときだけ
//! OpenFGA に問うことになり、応答の時間と「0 件で続きあり」から、見えないテーブルの名前が分かってしまう。問う数は語に
//! よらない。
//!
//! ページを持たない一覧（チャットの AI・ワークフローの `data.list_tables`・アプリの一覧と購読）は、問う数の上限で切れたら
//! 続きがあることを呼び出し側へ返す（[`TableListing::more`]・黙って短い一覧を全部と見せない）。アプリの一覧と購読は
//! 候補がそのアプリのテーブルだけなので、続きを [`APP_TABLE_ROUNDS`] 回まで読み足す。

use std::collections::{HashMap, HashSet};

use authz::{AuthContext, Consistency, ObjectType, Relation};
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::cursor::CursorCodec;
use crate::model::DataTable;
use crate::store::{DataStore, TableRow, TABLE_COLS};
use crate::{map_db, DataError, InvalidCode};

/// 一覧の 1 ページの件数の上限。
pub const MAX_TABLE_PAGE: i64 = 200;
/// DB の候補を OpenFGA で絞る経路で、1 回に引く候補の件数（[モジュール文書](self)）。
pub const TABLE_SCAN_CHUNK: i64 = 200;
/// DB の候補を OpenFGA で絞る経路で、1 回の頼みに OpenFGA へ問う数の上限（候補 × relation・ListObjects が
/// 返した分は問わない）。BatchCheck は 50 問ずつ 4 本並べて送る（[`authz::visible::holds_any`]）ので、上限まで
/// 問うと 5 往復ほど（手元の計測で 1 秒弱）。
pub const TABLE_SCAN_MAX_CHECKS: usize = 1_000;
/// 名前の検索の文字数の上限（テーブル名の上限と同じ・これより長い語はどの名前にも当たらない）。
const MAX_TABLE_QUERY_CHARS: usize = 128;
/// アプリのテーブルの一覧（ページを持たない）で、問う数の上限で切れたときに続きを読み足す回数の上限（候補はアプリの
/// テーブルだけ）。
pub const APP_TABLE_ROUNDS: usize = 5;

/// ページを持たない一覧の結果（[モジュール文書](self)）。
#[derive(Debug, Clone, Default)]
pub struct TableListing {
    /// 見えるテーブル（更新の新しい順・`limit` まで）。
    pub items: Vec<DataTable>,
    /// 一覧に載らなかった見えるテーブルがあるかもしれない（`limit` を超えたか、OpenFGA に問う数の上限で切れた）。
    pub more: bool,
}

/// テーブル一覧の 1 ページの頼み（`GET /data/tables` の `q`・`cursor`・`limit`・GAP-42）。
#[derive(Debug, Clone, Default)]
pub struct TableListQuery {
    /// 名前に含む文字（大文字小文字を区別しない・前後の空白は除く・空なら絞らない）。
    pub q: Option<String>,
    /// 続きの位置（前のページの [`TablePage::next_cursor`]・不透明な文字列）。
    pub cursor: Option<String>,
    /// 1 ページの件数（1〜[`MAX_TABLE_PAGE`] に丸める）。
    pub limit: i64,
}

/// テーブル一覧の 1 ページ（更新の新しい順）。
#[derive(Debug, Clone)]
pub struct TablePage {
    pub items: Vec<DataTable>,
    /// 続きがあるときの位置（無ければ `None`）。
    pub next_cursor: Option<String>,
}

/// 一覧の続きの位置（並びのキー `(updated_at, id)` の最後の値・キーセット）。
///
/// 中身はそのページで最後に確かめたテーブルの `updated_at` と `id`。DB の候補を OpenFGA で絞る経路では
/// 見えないテーブルを指すことがあるので、外へはテナントに束縛した暗号文で出す（[`Self::seal`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TableCursor {
    updated_at: DateTime<Utc>,
    id: Uuid,
}

impl TableCursor {
    fn of(t: &DataTable) -> Self {
        Self {
            updated_at: t.updated_at,
            id: t.id,
        }
    }

    fn seal(self, codec: &CursorCodec, tenant_id: &str) -> Result<String, DataError> {
        let raw = format!("{}.{}", self.updated_at.timestamp_micros(), self.id);
        codec.seal_table_list(tenant_id, raw.as_bytes())
    }

    /// 読めない位置（改竄・別テナント・前の版の平文の位置を含む）は `cursor_invalid`（400・先頭から読み直す）。
    fn open(codec: &CursorCodec, tenant_id: &str, s: &str) -> Result<Self, DataError> {
        let bad = || DataError::input(InvalidCode::CursorInvalid);
        if s.len() > 256 {
            return Err(bad());
        }
        let raw = codec.open_table_list(tenant_id, s)?;
        let raw = String::from_utf8(raw).map_err(|_| bad())?;
        let (micros, id) = raw.split_once('.').ok_or_else(bad)?;
        let updated_at = micros
            .parse::<i64>()
            .ok()
            .and_then(DateTime::from_timestamp_micros)
            .ok_or_else(bad)?;
        let id = Uuid::parse_str(id).map_err(|_| bad())?;
        Ok(Self { updated_at, id })
    }
}

/// 一覧を 1 回引いた結果（返すテーブルと、続きを読む位置）。
#[derive(Default)]
struct Listed {
    items: Vec<DataTable>,
    /// 続きを読む位置（無ければ読み切った）。
    next: Option<TableCursor>,
}

/// 名前の検索の語（前後の空白を除き、空なら `None`）。NUL は 400、長すぎる語も 400。
fn search_term(q: Option<&str>) -> Result<Option<String>, DataError> {
    let Some(q) = q.map(str::trim).filter(|q| !q.is_empty()) else {
        return Ok(None);
    };
    if q.contains('\0') {
        return Err(DataError::input(InvalidCode::InvalidInput));
    }
    if q.chars().count() > MAX_TABLE_QUERY_CHARS {
        return Err(DataError::input(InvalidCode::ValueTooLong));
    }
    Ok(Some(like_pattern(q)))
}

/// 部分一致の LIKE パターン（`%`・`_`・`\` は文字そのものとして扱う・ESCAPE '\'）。
fn like_pattern(q: &str) -> String {
    let mut out = String::with_capacity(q.len() + 2);
    out.push('%');
    for c in q.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

/// SQL で絞る条件（名前の部分一致の LIKE パターン・続きの位置）。
#[derive(Default)]
struct ListFilter {
    app_id: Option<Uuid>,
    pattern: Option<String>,
    after: Option<TableCursor>,
    /// 追加だけ（`submitter`・D-113）で共有されたテーブルも含める（画面の一覧と、アプリへ見せる一覧だけ）。
    /// 含めない一覧（チャットの AI・ワークフロー・SQL の名前の候補・アプリのイベント購読）は、そのテーブルを
    /// viewer として開く使い手なので、従来どおり viewer の実効集合だけにする。
    include_add_only: bool,
}

impl ListFilter {
    /// 一覧に入れる relation（どれかを持てば入る）。
    fn relations(&self) -> &'static [Relation] {
        if self.include_add_only {
            // 追加だけ（editor・owner も含む実効集合）を足す。
            &[Relation::Viewer, Relation::Submitter]
        } else {
            &[Relation::Viewer]
        }
    }
}

impl DataStore {
    /// 自分が使えるテーブル一覧（FGA viewer 実効集合 → DB 突合の二段・owner 含む）。続きがあるかを捨てる（名前の候補など、
    /// 欠けても困らない使い道だけ）。続きを知りたいときは [`Self::list_tables_listing`]。
    pub async fn list_tables(
        &self,
        ctx: &AuthContext,
        limit: i64,
    ) -> Result<Vec<DataTable>, DataError> {
        Ok(self.list_tables_listing(ctx, limit).await?.items)
    }

    /// [`Self::list_tables`] と同じ一覧に、続きがあるか（[`TableListing::more`]）を添える（チャットの AI・ワークフロー）。
    pub async fn list_tables_listing(
        &self,
        ctx: &AuthContext,
        limit: i64,
    ) -> Result<TableListing, DataError> {
        self.collect_tables(ctx, ListFilter::default(), limit, 1)
            .await
    }

    /// 自分が使えるテーブルの 1 ページ（名前の検索・続きの位置・GAP-42）。並びは [`Self::list_tables`] と同じ
    /// 更新の新しい順で、検索と続きも見えるテーブルの中だけを返す（LIMIT の前に絞る）。
    pub async fn list_tables_page(
        &self,
        ctx: &AuthContext,
        query: &TableListQuery,
    ) -> Result<TablePage, DataError> {
        self.list_page_filtered(ctx, query, false).await
    }

    /// [`Self::list_tables`] に、追加だけで共有されたテーブルを足した一覧（画面の一覧・D-113）。
    pub(crate) async fn list_tables_with_add_only(
        &self,
        ctx: &AuthContext,
        limit: i64,
    ) -> Result<Vec<DataTable>, DataError> {
        let filter = ListFilter {
            include_add_only: true,
            ..ListFilter::default()
        };
        Ok(self.list_tables_filtered(ctx, &filter, limit).await?.items)
    }

    /// [`Self::list_tables_page`] に、追加だけで共有されたテーブルを足した 1 ページ（画面の一覧と名前の検索・D-113）。
    pub(crate) async fn list_tables_page_with_add_only(
        &self,
        ctx: &AuthContext,
        query: &TableListQuery,
    ) -> Result<TablePage, DataError> {
        self.list_page_filtered(ctx, query, true).await
    }

    async fn list_page_filtered(
        &self,
        ctx: &AuthContext,
        query: &TableListQuery,
        include_add_only: bool,
    ) -> Result<TablePage, DataError> {
        let filter = ListFilter {
            app_id: None,
            pattern: search_term(query.q.as_deref())?,
            after: query
                .cursor
                .as_deref()
                .map(|c| TableCursor::open(&self.cursor, &ctx.tenant_id, c))
                .transpose()?,
            include_add_only,
        };
        let listed = self.list_tables_filtered(ctx, &filter, query.limit).await?;
        let next_cursor = listed
            .next
            .map(|c| c.seal(&self.cursor, &ctx.tenant_id))
            .transpose()?;
        Ok(TablePage {
            items: listed.items,
            next_cursor,
        })
    }

    /// アプリ所有 ∩ 自分が viewer のテーブル一覧（app-gateway の所有束縛・Task 9.8）。
    ///
    /// `app_id` の絞り込みを **LIMIT より前に SQL で**行う（可視テーブルが上限を超えても
    /// アプリ所有分が一覧から欠落しない）。
    pub async fn list_app_tables(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
        limit: i64,
    ) -> Result<TableListing, DataError> {
        let filter = ListFilter {
            app_id: Some(app_id),
            ..ListFilter::default()
        };
        self.collect_tables(ctx, filter, limit, APP_TABLE_ROUNDS)
            .await
    }

    /// アプリ所有 ∩ 自分が viewer か追加だけのテーブル一覧（app-gateway の `GET /gw/data/tables`・D-113）。
    ///
    /// 追加だけのテーブルには `add_only` を立てる。イベントの購読は [`Self::list_app_tables`]（viewer だけ）を使う
    /// （他人のレコードの変更を追加だけの人へ流さない）。
    pub async fn list_app_tables_for_reader(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
        limit: i64,
    ) -> Result<TableListing, DataError> {
        let filter = ListFilter {
            app_id: Some(app_id),
            include_add_only: true,
            ..ListFilter::default()
        };
        let listing = self
            .collect_tables(ctx, filter, limit, APP_TABLE_ROUNDS)
            .await?;
        let az = self.authz_memo();
        crate::table_redact::prefetch_table_relations(ctx, &az, &listing.items).await?;
        let mut out = Vec::with_capacity(listing.items.len());
        for mut t in listing.items {
            t.add_only = self.read_scope(ctx, &az, t.id).await?
                == Some(crate::table_access::ReadScope::OwnRecords);
            out.push(t);
        }
        Ok(TableListing {
            items: out,
            more: listing.more,
        })
    }

    /// ページを持たない一覧（[`TableListing`]）。問う数の上限で切れたら、続きを `rounds` 回まで読み足す（`limit` は
    /// 1〜[`MAX_TABLE_PAGE`] に丸める）。
    async fn collect_tables(
        &self,
        ctx: &AuthContext,
        mut filter: ListFilter,
        limit: i64,
        rounds: usize,
    ) -> Result<TableListing, DataError> {
        let limit = limit.clamp(1, MAX_TABLE_PAGE);
        let mut items: Vec<DataTable> = Vec::new();
        for _ in 0..rounds.max(1) {
            let want = limit - i64::try_from(items.len()).unwrap_or(limit);
            let listed = self.list_tables_filtered(ctx, &filter, want).await?;
            items.extend(listed.items);
            match listed.next {
                Some(next) if i64::try_from(items.len()).unwrap_or(limit) < limit => {
                    filter.after = Some(next);
                }
                next => {
                    return Ok(TableListing {
                        items,
                        more: next.is_some(),
                    })
                }
            }
        }
        Ok(TableListing { items, more: true })
    }

    /// アプリが所有する**全**テーブル ID（FGA 突合なし・Task 9.13b アンインストール撤去用）。
    ///
    /// 呼び出し側（InstallService）が mini_app_code アーティファクトの owner を検証してから
    /// 使うこと。ユーザー可視性でフィルタすると失効済み共有の残骸テーブルを撤去できないため、
    /// ここは意図的に tenant＋app_id 束縛のみとする。
    pub async fn table_ids_owned_by_app(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
    ) -> Result<Vec<Uuid>, DataError> {
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM data_table \
             WHERE tenant_id = $1 AND app_id = $2 AND deleted_at IS NULL",
        )
        .bind(&ctx.tenant_id)
        .bind(app_id)
        .fetch_all(self.pools.for_tenant(&ctx.tenant_id))
        .await
        .map_err(map_db)?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    /// 一覧の本体（[モジュール文書](self)）。`limit` は 1〜[`MAX_TABLE_PAGE`] に丸める。
    async fn list_tables_filtered(
        &self,
        ctx: &AuthContext,
        filter: &ListFilter,
        limit: i64,
    ) -> Result<Listed, DataError> {
        let limit = usize::try_from(limit.clamp(1, MAX_TABLE_PAGE)).unwrap_or(1);
        // 実効集合が上限より少なければ、それを正に SQL で絞る（速い・ListObjects 1 往復）。上限に届いたら、
        // DB の候補を OpenFGA で絞る（BatchCheck は 1 往復が重いので、上限の内ではこちらを使わない）。
        let mut known: HashSet<Uuid> = HashSet::new();
        let mut cut = false;
        for relation in filter.relations() {
            let objs = self
                .authz
                .list_objects(&ctx.subject(), *relation, ObjectType::DataTable)
                .await
                .map_err(|e| DataError::Internal(e.to_string()))?;
            cut |= authz::visible::list_may_be_cut(objs.len());
            known.extend(local_ids(ctx, &objs));
        }
        if cut {
            return self.scan_tables(ctx, filter, limit, &known).await;
        }
        if known.is_empty() {
            return Ok(Listed::default());
        }
        let ids: Vec<Uuid> = known.into_iter().collect();
        // 1 件多く引いて続きの有無を知る。
        let rows: Vec<TableRow> = sqlx::query_as(&format!(
            "SELECT {TABLE_COLS} FROM data_table \
             WHERE tenant_id = $1 AND org = $2 AND id = ANY($3) AND deleted_at IS NULL \
               AND ($5::uuid IS NULL OR app_id = $5) \
               AND ($6::text IS NULL OR name ILIKE $6 ESCAPE '\\') \
               AND ($7::timestamptz IS NULL OR (updated_at, id) < ($7, $8::uuid)) \
             ORDER BY updated_at DESC, id DESC LIMIT $4"
        ))
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(&ids)
        .bind(i64::try_from(limit + 1).unwrap_or(i64::MAX))
        .bind(filter.app_id)
        .bind(filter.pattern.as_deref())
        .bind(filter.after.map(|c| c.updated_at))
        .bind(filter.after.map(|c| c.id))
        .fetch_all(self.pools.for_tenant(&ctx.tenant_id))
        .await
        .map_err(map_db)?;
        let mut items: Vec<DataTable> = rows.into_iter().map(TableRow::into_table).collect();
        let more = items.len() > limit;
        items.truncate(limit);
        let next = more.then(|| items.last().map(TableCursor::of)).flatten();
        Ok(Listed { items, next })
    }

    /// DB の候補（テナント・組織・アプリ・名前・続きの位置で絞ったもの）を並びの順に引き、OpenFGA で絞る
    /// （ListObjects が上限に届いたとき・BUG-149）。
    ///
    /// `known` は ListObjects が返した見えるテーブル（問い直さない）。候補は id と並びのキーだけを引き、
    /// 返すテーブルの定義は最後に 1 回で読む（見えない候補の定義を運ばない）。
    async fn scan_tables(
        &self,
        ctx: &AuthContext,
        filter: &ListFilter,
        limit: usize,
        known: &HashSet<Uuid>,
    ) -> Result<Listed, DataError> {
        let subject = ctx.subject();
        let ns = ctx.ns();
        let pool = self.pools.for_tenant(&ctx.tenant_id);
        let mut after = filter.after;
        let mut picked: Vec<TableCursor> = Vec::new();
        let mut checks = 0usize;
        let next = loop {
            // 名前の検索の語では候補を選ばない（当たるかを列で返すだけ・[モジュール文書](self)）。
            let chunk: Vec<(Uuid, DateTime<Utc>, bool)> = sqlx::query_as(
                "SELECT id, updated_at, ($4::text IS NULL OR name ILIKE $4 ESCAPE '\\') AS hit \
                 FROM data_table \
                 WHERE tenant_id = $1 AND org = $2 AND deleted_at IS NULL \
                   AND ($3::uuid IS NULL OR app_id = $3) \
                   AND ($5::timestamptz IS NULL OR (updated_at, id) < ($5, $6::uuid)) \
                 ORDER BY updated_at DESC, id DESC LIMIT $7",
            )
            .bind(&ctx.tenant_id)
            .bind(&ctx.org)
            .bind(filter.app_id)
            .bind(filter.pattern.as_deref())
            .bind(after.map(|c| c.updated_at))
            .bind(after.map(|c| c.id))
            .bind(TABLE_SCAN_CHUNK)
            .fetch_all(pool)
            .await
            .map_err(map_db)?;
            // 名前に当たらない候補も問う（問う数を語によらせない）。
            let unknown: Vec<authz::FgaObject> = chunk
                .iter()
                .filter(|(id, ..)| !known.contains(id))
                .map(|(id, ..)| ns.data_table(&id.to_string()))
                .collect();
            checks += unknown.len() * filter.relations().len();
            let mut held = authz::visible::holds_any(
                self.authz.as_ref(),
                &subject,
                filter.relations(),
                &unknown,
                Consistency::HigherConsistency,
            )
            .await
            .map_err(|e| DataError::Internal(format!("authz: {e}")))?
            .into_iter();
            let mut full = None;
            for (id, updated_at, hit) in &chunk {
                let pos = TableCursor {
                    updated_at: *updated_at,
                    id: *id,
                };
                let visible = known.contains(id) || held.next().unwrap_or(false);
                if visible && *hit {
                    if picked.len() == limit {
                        // 見えるテーブルがまだある（続きはこのページの最後のテーブルの後から）。
                        full = picked.last().copied();
                        break;
                    }
                    picked.push(pos);
                }
                after = Some(pos);
            }
            if full.is_some() {
                break full;
            }
            if i64::try_from(chunk.len()).unwrap_or(i64::MAX) < TABLE_SCAN_CHUNK {
                break None;
            }
            if checks >= TABLE_SCAN_MAX_CHECKS {
                // 問う数の上限。見つけた分と、確かめた最後の候補の後からの続きを返す。
                tracing::debug!(
                    checks,
                    found = picked.len(),
                    "テーブルの一覧: OpenFGA に問う数の上限に届いたので続きの位置を返す"
                );
                break after;
            }
        };
        let items = self.load_picked(ctx, filter, &picked).await?;
        Ok(Listed { items, next })
    }

    /// 選んだテーブルの定義を 1 回で読む（並びは選んだ順・その間に消えたものは落とす）。
    async fn load_picked(
        &self,
        ctx: &AuthContext,
        filter: &ListFilter,
        picked: &[TableCursor],
    ) -> Result<Vec<DataTable>, DataError> {
        if picked.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<Uuid> = picked.iter().map(|c| c.id).collect();
        let rows: Vec<TableRow> = sqlx::query_as(&format!(
            "SELECT {TABLE_COLS} FROM data_table \
             WHERE tenant_id = $1 AND org = $2 AND id = ANY($3) AND deleted_at IS NULL \
               AND ($4::uuid IS NULL OR app_id = $4) \
               AND ($5::text IS NULL OR name ILIKE $5 ESCAPE '\\')"
        ))
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(&ids)
        .bind(filter.app_id)
        .bind(filter.pattern.as_deref())
        .fetch_all(self.pools.for_tenant(&ctx.tenant_id))
        .await
        .map_err(map_db)?;
        let mut by_id: HashMap<Uuid, DataTable> =
            rows.into_iter().map(|r| (r.id, r.into_table())).collect();
        Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
    }
}

/// ListObjects の応答（`data_table:<tenant>|<uuid>`）から、このテナントのテーブルの id を取り出す
/// （テナントの違う識別子・読めない id は落とす）。
fn local_ids<'a>(ctx: &'a AuthContext, objs: &'a [String]) -> impl Iterator<Item = Uuid> + 'a {
    let ns = ctx.ns();
    objs.iter().filter_map(move |o| {
        let (_, id_part) = o.split_once(':')?;
        Uuid::parse_str(ns.strip_object_id(id_part)?).ok()
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn cursor_round_trips_and_rejects_garbage() {
        let codec = CursorCodec::new(&crate::CursorKey::from_bytes([7u8; crate::CURSOR_KEY_LEN]));
        let c = TableCursor {
            updated_at: DateTime::from_timestamp_micros(1_761_400_000_123_456).unwrap(),
            id: Uuid::new_v4(),
        };
        let token = c.seal(&codec, "t-a").unwrap();
        assert_eq!(TableCursor::open(&codec, "t-a", &token).unwrap(), c);
        // 位置（テーブルの id）は外から読めない。
        assert!(!token.contains(&c.id.to_string()));
        // 前の版の平文の位置（base64 の "micros.uuid"）・別テナント・壊れた文字列は読めない。
        let plain = {
            use base64::Engine as _;
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!(
                "{}.{}",
                c.updated_at.timestamp_micros(),
                c.id
            ))
        };
        for (tenant, bad) in [
            ("t-a", ""),
            ("t-a", "!!"),
            ("t-a", "bm90LWEtY3Vyc29y"),
            ("t-a", &"A".repeat(300)),
            ("t-a", plain.as_str()),
            ("t-b", token.as_str()),
        ] {
            let e = TableCursor::open(&codec, tenant, bad).unwrap_err();
            assert!(
                matches!(&e, DataError::InvalidInput(i) if i.code() == InvalidCode::CursorInvalid),
                "{bad}: {e:?}"
            );
        }
    }

    #[test]
    fn search_term_is_a_literal_substring() {
        assert_eq!(search_term(None).unwrap(), None);
        assert_eq!(search_term(Some("  ")).unwrap(), None);
        assert_eq!(
            search_term(Some(" 経費 ")).unwrap().as_deref(),
            Some("%経費%")
        );
        // LIKE の記号は文字そのもの（全件に当たる `%` を作らせない）。
        assert_eq!(
            search_term(Some("10%_off\\")).unwrap().as_deref(),
            Some("%10\\%\\_off\\\\%")
        );
        assert!(search_term(Some("a\0b")).is_err());
        assert!(search_term(Some(&"あ".repeat(129))).is_err());
        assert!(search_term(Some(&"あ".repeat(128))).is_ok());
    }
}

//! テーブル一覧（Task 9.2/9.8）。`store.rs` から分離（1 ファイル行数規約）。
//!
//! FGA viewer 実効集合 → DB 突合の二段（画面の一覧は追加だけ＝`submitter` の実効集合も足す・D-113）。
//! app-gateway の所有束縛（`list_app_tables`）は
//! `app_id` の絞り込みを LIMIT より前に SQL で行う（上限超過時の欠落を防ぐ）。
//!
//! 画面の一覧（`GET /data/tables`）は名前の検索と続きの位置で引く（[`DataStore::list_tables_page`]・GAP-42）。
//! 検索も続きも viewer 実効集合の中だけを SQL で絞るので、見えないテーブルの名前に当たることはない。

use authz::{AuthContext, ObjectType, Relation};
use base64::Engine as _;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::model::DataTable;
use crate::store::{DataStore, TableRow, TABLE_COLS};
use crate::{map_db, DataError, InvalidCode};

/// 一覧の 1 ページの件数の上限。
pub const MAX_TABLE_PAGE: i64 = 200;
/// 名前の検索の文字数の上限（テーブル名の上限と同じ・これより長い語はどの名前にも当たらない）。
const MAX_TABLE_QUERY_CHARS: usize = 128;

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
/// 中身はそのページの最後のテーブルの `updated_at` と `id` で、どちらも一覧の応答に出ている値なので
/// 暗号化しない（レコードのカーソルと違い、内部の連番を含まない・PIT-75 の対象外）。書き換えられても
/// 見えるテーブルの中で読み始めの位置が変わるだけ。
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

    fn encode(self) -> String {
        let raw = format!("{}.{}", self.updated_at.timestamp_micros(), self.id);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
    }

    /// 読めない位置は `cursor_invalid`（400・先頭から読み直す）。
    fn decode(s: &str) -> Result<Self, DataError> {
        let bad = || DataError::input(InvalidCode::CursorInvalid);
        if s.len() > 128 {
            return Err(bad());
        }
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(s)
            .map_err(|_| bad())?;
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

impl DataStore {
    /// 自分が使えるテーブル一覧（FGA viewer 実効集合 → DB 突合の二段・owner 含む）。
    pub async fn list_tables(
        &self,
        ctx: &AuthContext,
        limit: i64,
    ) -> Result<Vec<DataTable>, DataError> {
        self.list_tables_filtered(ctx, &ListFilter::default(), limit)
            .await
    }

    /// 自分が使えるテーブルの 1 ページ（名前の検索・続きの位置・GAP-42）。並びは [`Self::list_tables`] と同じ
    /// 更新の新しい順で、検索と続きも viewer 実効集合の中だけを SQL で絞る（LIMIT の前に絞る）。
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
        self.list_tables_filtered(ctx, &filter, limit).await
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
                .map(TableCursor::decode)
                .transpose()?,
            include_add_only,
        };
        let limit = query.limit.clamp(1, MAX_TABLE_PAGE);
        // 1 件多く引いて続きの有無を知る。
        let mut items = self.list_tables_filtered(ctx, &filter, limit + 1).await?;
        let more = items.len() > usize::try_from(limit).unwrap_or(usize::MAX);
        items.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        let next_cursor = more
            .then(|| items.last().map(|t| TableCursor::of(t).encode()))
            .flatten();
        Ok(TablePage { items, next_cursor })
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
    ) -> Result<Vec<DataTable>, DataError> {
        let filter = ListFilter {
            app_id: Some(app_id),
            ..ListFilter::default()
        };
        self.list_tables_filtered(ctx, &filter, limit).await
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
    ) -> Result<Vec<DataTable>, DataError> {
        let filter = ListFilter {
            app_id: Some(app_id),
            include_add_only: true,
            ..ListFilter::default()
        };
        let tables = self.list_tables_filtered(ctx, &filter, limit).await?;
        let az = self.authz_memo();
        crate::table_redact::prefetch_table_relations(ctx, &az, &tables).await?;
        let mut out = Vec::with_capacity(tables.len());
        for mut t in tables {
            t.add_only = self.read_scope(ctx, &az, t.id).await?
                == Some(crate::table_access::ReadScope::OwnRecords);
            out.push(t);
        }
        Ok(out)
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

    async fn list_tables_filtered(
        &self,
        ctx: &AuthContext,
        filter: &ListFilter,
        limit: i64,
    ) -> Result<Vec<DataTable>, DataError> {
        // 1 ページ（200 件）＋続きの有無を知るための 1 件。
        let limit = limit.clamp(1, MAX_TABLE_PAGE + 1);
        let mut objs = self
            .authz
            .list_objects(&ctx.subject(), Relation::Viewer, ObjectType::DataTable)
            .await
            .map_err(|e| DataError::Internal(e.to_string()))?;
        if filter.include_add_only {
            // 追加だけ（editor・owner も含む実効集合）を足す。重なりは下の DB 突合（`= ANY`）で 1 行になる。
            objs.extend(
                self.authz
                    .list_objects(&ctx.subject(), Relation::Submitter, ObjectType::DataTable)
                    .await
                    .map_err(|e| DataError::Internal(e.to_string()))?,
            );
        }
        let mut ids: Vec<Uuid> = Vec::new();
        for o in objs {
            let Some((_, id_part)) = o.split_once(':') else {
                continue;
            };
            if let Some(local) = ctx.ns().strip_object_id(id_part) {
                if let Ok(id) = Uuid::parse_str(local) {
                    ids.push(id);
                }
            }
        }
        if ids.is_empty() {
            return Ok(Vec::new());
        }
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
        .bind(limit)
        .bind(filter.app_id)
        .bind(filter.pattern.as_deref())
        .bind(filter.after.map(|c| c.updated_at))
        .bind(filter.after.map(|c| c.id))
        .fetch_all(self.pools.for_tenant(&ctx.tenant_id))
        .await
        .map_err(map_db)?;
        Ok(rows.into_iter().map(TableRow::into_table).collect())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn cursor_round_trips_and_rejects_garbage() {
        let c = TableCursor {
            updated_at: DateTime::from_timestamp_micros(1_761_400_000_123_456).unwrap(),
            id: Uuid::new_v4(),
        };
        assert_eq!(TableCursor::decode(&c.encode()).unwrap(), c);
        for bad in ["", "!!", "bm90LWEtY3Vyc29y", &"A".repeat(200)] {
            let e = TableCursor::decode(bad).unwrap_err();
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

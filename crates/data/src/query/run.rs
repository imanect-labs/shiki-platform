//! クエリ実行の入口（Task 15.3）: 束縛 → 正規化 → 認可の合成 → 計画・実行 → 本体取得 → 事後検査。
//!
//! - 読取は 1 本の **REPEATABLE READ・READ ONLY** トランザクションで行う（件数探索・駆動・本体取得が
//!   同じスナップショットを見る）。接続には `plan_cache_mode = force_custom_plan`（計画時に 1 パーティションへ
//!   枝刈りする・query-optimizer.md §6.1）と `statement_timeout`（data-platform §5.2）を `SET LOCAL` する。
//! - 経路は 2 つ。既定は pivot 索引（[`super::planner`]）。必要な索引が `active` でないとき・機能フラグで
//!   本体経路を選んだときは v1 方式（[`super::v1`]）。どちらも同じ IR・全順序・カーソル。
//! - 応答に推定値・統計値・戦略名を載せない（PIT-75）。`record_int` は暗号化カーソルの中にだけ入る。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use authz::AuthContext;
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use super::bound::{bind_query, BoundQuery, FieldKind};
use super::ir::{CountResult, DataQuery, QueryResult};
use super::planner::exec::{Exec, ForcedStrategy, PageOut};
use super::planner::feedback::{QueryExecStats, TableScanMetrics};
use super::planner::guard::Guard;
use super::planner::keys::{from_payload, to_payload, Pos, SortVal};
use super::planner::lower::{fetch_sql, Lowerer};
use super::planner::normalize::{normalize, Rules};
use super::planner::verify::{post_filter, PolicyEnv, RowView};
use super::sql::{bind_query as bind_q, Bind, SqlBuf};
use crate::cursor::CursorScope;
use crate::index::state::load_states;
use crate::index::{IndexState, IndexTable};
use crate::model::{
    DataRecord, DataTable, FieldId, TableKeys, SYSTEM_FIELD_CREATED_AT, SYSTEM_FIELD_OWNER,
    SYSTEM_FIELD_UPDATED_AT,
};
use crate::policy::compile::compile_read_predicate;
use crate::policy::compile_index::IndexGuardCtx;
use crate::policy::material::{self, PolicyMaterial};
use crate::record::RecordRow;
use crate::store::DataStore;
use crate::{map_db, DataError, DEFAULT_AGGREGATE_MIN_ROWS};

/// 件数の既定上限（`AtLeast(10_000)`・data-platform §5.4）。
pub const DEFAULT_COUNT_CAP: u64 = 10_000;
/// data の読取の `statement_timeout` 既定（ミリ秒）。
pub const DEFAULT_READ_TIMEOUT_MS: u64 = 10_000;

/// 読取経路（機能フラグ・data-platform §13 M3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReadPath {
    /// pivot 索引＋自前オプティマイザ（既定）。
    #[default]
    Index,
    /// v1 方式（本体の JSONB を直接評価する単発 SQL）。切戻し・parity 検証用。
    Body,
}

/// 実行の調整（運用・検証用。既定は自然な選択）。
///
/// 環境変数 `SHIKI_DATA_READ_PATH`（`index` / `body`）・`SHIKI_DATA_FORCE_STRATEGY`
/// （`sort` / `filter` / `policy` / `union` / `safe`）・`SHIKI_DATA_SORT_BUDGET`（整数）でも与えられる
/// （CI で `policy_threat_it` を全戦略・両経路で回すため）。どの値でも結果は変わらず、速さだけが変わる。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryTuning {
    pub read_path: ReadPath,
    pub force: Option<ForcedStrategy>,
    /// ソート駆動の走査予算（極端に絞って切替を必ず起こす検証用）。
    pub sort_budget: Option<i64>,
}

impl QueryTuning {
    /// 環境変数から読む（不正な値は無視して既定）。
    pub fn from_env() -> Self {
        let read_path = match std::env::var("SHIKI_DATA_READ_PATH").as_deref() {
            Ok("body") => ReadPath::Body,
            _ => ReadPath::Index,
        };
        let force = match std::env::var("SHIKI_DATA_FORCE_STRATEGY").as_deref() {
            Ok("sort") => Some(ForcedStrategy::Sort),
            Ok("filter") => Some(ForcedStrategy::Filter),
            Ok("policy") => Some(ForcedStrategy::Policy),
            Ok("union") => Some(ForcedStrategy::Union),
            Ok("safe") => Some(ForcedStrategy::Safe),
            _ => None,
        };
        let sort_budget = std::env::var("SHIKI_DATA_SORT_BUDGET")
            .ok()
            .and_then(|s| s.parse().ok());
        QueryTuning {
            read_path,
            force,
            sort_budget,
        }
    }
}

/// デバッグ取得（検証専用）: 実行した文とその実行記録。
#[derive(Debug, Clone)]
pub struct QueryDebug {
    pub stats: QueryExecStats,
    pub statements: Vec<DebugStatement>,
}

/// 実行した文 1 つ（計画の形の回帰テストで EXPLAIN し直すため）。
#[derive(Debug, Clone)]
pub struct DebugStatement {
    pub label: &'static str,
    pub sql: String,
    pub params: Vec<DebugParam>,
}

/// 文のパラメータ（型つき）。
#[derive(Debug, Clone, PartialEq)]
pub enum DebugParam {
    Text(String),
    TextArray(Vec<String>),
    F64(f64),
    UuidArray(Vec<Uuid>),
    I16(i16),
    I32(i32),
    I64(i64),
    I64Array(Vec<i64>),
    Numeric(String),
    NumericArray(Vec<String>),
    Bytes(Vec<u8>),
}

impl From<Bind> for DebugParam {
    fn from(b: Bind) -> Self {
        match b {
            Bind::Text(v) => DebugParam::Text(v),
            Bind::TextArray(v) => DebugParam::TextArray(v),
            Bind::Number(v) => DebugParam::F64(v),
            Bind::UuidArray(v) => DebugParam::UuidArray(v),
            Bind::I16(v) => DebugParam::I16(v),
            Bind::I32(v) => DebugParam::I32(v),
            Bind::I64(v) => DebugParam::I64(v),
            Bind::I64Array(v) => DebugParam::I64Array(v),
            Bind::Numeric(v) => DebugParam::Numeric(v),
            Bind::NumericArray(v) => DebugParam::NumericArray(v),
            Bind::Bytes(v) => DebugParam::Bytes(v),
        }
    }
}

/// 本体取得の行（`record_int` は crate 内に閉じる）。
#[derive(sqlx::FromRow)]
struct FetchedRow {
    record_int: i64,
    #[sqlx(flatten)]
    row: RecordRow,
}

/// 1 リクエストで必要な索引（`active` でなければ本体経路）。
fn needed_indexes(q: &BoundQuery) -> HashSet<(FieldId, IndexTable)> {
    let mut need: HashSet<(FieldId, IndexTable)> = [
        (SYSTEM_FIELD_OWNER, IndexTable::Text),
        (SYSTEM_FIELD_CREATED_AT, IndexTable::Text),
        (SYSTEM_FIELD_UPDATED_AT, IndexTable::Text),
    ]
    .into();
    let mut atoms = Vec::new();
    q.filter.atoms(&mut atoms);
    for a in atoms {
        let f = q.field(a.field);
        let t = match a.leaf {
            super::bound::Leaf::Contains(_) | super::bound::Leaf::EndsWith(_) => IndexTable::Search,
            _ => f.value_table(),
        };
        need.insert((f.id, t));
    }
    for k in &q.sort {
        need.insert((k.field, q.field(k.field).value_table()));
    }
    for g in &q.group_by {
        need.insert((*g, q.field(*g).value_table()));
    }
    for (_, f) in &q.metrics {
        if let Some(f) = f {
            need.insert((*f, IndexTable::Num));
        }
    }
    need
}

/// カーソルの束縛（条件・ソート・検索の指紋。別のクエリへ持ち込めない）。
fn fingerprint(q: &DataQuery) -> Vec<u8> {
    let v = serde_json::json!([q.filter, q.sort, q.search, q.search_fields]);
    Sha256::digest(v.to_string().as_bytes()).to_vec()
}

impl DataStore {
    /// 宣言的クエリを実行する（viewer・行述語＋フィールドマスク合成・Task 15.3）。
    pub async fn run_query(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        query: &DataQuery,
        trace_id: Option<&str>,
    ) -> Result<QueryResult, DataError> {
        let tuning = self.tuning.clone();
        self.run_query_as(ctx, table_id, query, trace_id, "data.query", &tuning)
            .await
            .map(|(r, _)| r)
    }

    /// 検証専用: 調整つきで実行し、実行した文と実行記録も返す（計画の形の回帰・全戦略一致テスト）。
    #[doc(hidden)]
    pub async fn run_query_debug(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        query: &DataQuery,
        tuning: &QueryTuning,
    ) -> Result<(QueryResult, QueryDebug), DataError> {
        self.run_query_as(ctx, table_id, query, None, "data.query", tuning)
            .await
    }

    /// テーブル単位の走査効率などの運用指標（推定値・値は含まない）。
    pub fn query_metrics(&self, table_id: Uuid) -> Option<TableScanMetrics> {
        self.feedback.snapshot(table_id)
    }

    /// 事後検査がプロセス全体で落とした行数（警報の指標・0 であるべき）。
    pub fn post_filter_drops_total() -> u64 {
        super::planner::verify::verify_drops_total()
    }

    pub(crate) async fn run_query_as(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        query: &DataQuery,
        trace_id: Option<&str>,
        action: &str,
        tuning: &QueryTuning,
    ) -> Result<(QueryResult, QueryDebug), DataError> {
        // このリクエストの認可メモ（ページ・件数・lookup / rollup の解決で同じ問いを OpenFGA へ
        // 投げ直さない・リクエストの外へは持ち出せない・Task 15.9・PIT-18）。テーブル viewer・参照先
        // テーブル viewer・名指しロールの check は入口で BatchCheck 1 往復に束ねる。
        let az = self.authz_memo();
        let mut table = self
            .open_for_read(ctx, &az, table_id, action, trace_id)
            .await?;
        let masked = self.masked_fields(ctx, &az, &table).await?;
        let mut bq = bind_query(&table.schema, &masked, query)?;
        // 材料化した派生列で絞る・並べるなら、その前提（4 階層）を今の認可状態で検査し直す。崩れていれば
        // 降格（索引行を同一 Tx で破棄）してから束縛し直す＝その列はフィルタ/ソートに使えなくなる（PIT-71）。
        if bq.fields.values().any(|f| f.derived)
            && self.verify_materialized(ctx, &az, &table).await?
        {
            table = self.fetch_live(ctx, table_id).await?;
            bq = bind_query(&table.schema, &masked, query)?;
        }
        if bq.is_aggregate() {
            // 集計クエリは監査に残す（PIT-17: 反復差分の検知可能性）。
            self.record_audit_best_effort(
                ctx,
                "data.query.aggregate",
                &table_id.to_string(),
                trace_id,
                serde_json::json!({ "group_by": query.group_by }),
            )
            .await;
        }
        bq.filter = normalize(bq.filter, &bq.fields, Rules::ALL);
        // 行述語の材料（リクエストごとに FGA・跨いでキャッシュしない・PIT-18）。row_policy なしは共有も効かない。
        let m = match &table.schema.row_policy {
            Some(p) => material::resolve(ctx, &az, &[&p.read]).await?,
            None => Arc::new(PolicyMaterial::default()),
        };

        let db = self.pools.for_tenant(&ctx.tenant_id);
        let mut tx = db.begin().await.map_err(map_db)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
        sqlx::query(
            "SELECT set_config('plan_cache_mode', 'force_custom_plan', true), \
                    set_config('statement_timeout', $1, true)",
        )
        .bind(self.read_timeout_ms.to_string())
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;

        let out = self
            .execute(&mut tx, ctx, &table, &bq, &m, query, tuning)
            .await?;
        tx.commit().await.map_err(map_db)?;
        let (mut result, mut debug, rows) = out;
        // 第 1 キーの同値群が先読みの上限を超えたソートの組み合わせは、複合索引をオンデマンドで材料化する
        // （data-platform §5.3・§10 と同じ機構。失敗しても読取の結果には影響させない）。
        if debug.stats.composite_requested {
            let set: Vec<(FieldId, bool)> = bq.sort.iter().map(|k| (k.field, k.desc)).collect();
            if let Err(e) = self
                .request_sort_set(&ctx.tenant_id, table_id, table.keys, &set)
                .await
            {
                tracing::warn!(error = %e, %table_id, "複合索引の材料化の要求に失敗");
            }
        }
        // 本体の後段（派生フィールド・マスク・投影）はトランザクションの外で行う。
        if let Some(mut items) = rows {
            self.resolve_derived_fields(ctx, &az, &table, &masked, &mut items)
                .await?;
            Self::apply_mask_records(&masked, &mut items);
            if let Some(sel) = &bq.select {
                for r in &mut items {
                    if let Some(obj) = r.data.as_object_mut() {
                        obj.retain(|k, _| sel.contains(k));
                    }
                }
            }
            debug.stats.returned = items.len() as u64;
            result.items = Some(items);
        }
        result.shares_truncated = m.shares_truncated;
        self.feedback.record(table_id, &debug.stats);
        Ok((result, debug))
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    async fn execute(
        &self,
        conn: &mut PgConnection,
        ctx: &AuthContext,
        table: &DataTable,
        bq: &BoundQuery,
        m: &PolicyMaterial,
        query: &DataQuery,
        tuning: &QueryTuning,
    ) -> Result<(QueryResult, QueryDebug, Option<Vec<DataRecord>>), DataError> {
        let keys = table.keys;
        let states = load_states(conn, keys).await?;
        let active: HashSet<(FieldId, IndexTable)> = states
            .iter()
            .filter(|(_, _, s)| *s == IndexState::Active)
            .map(|(f, t, _)| (*f, *t))
            .collect();
        let needed = needed_indexes(bq);
        // 利用者が指定した列の索引がバックフィル中なら「準備中」（data-platform §10: building の間は
        // 駆動にもフィルタにも使わせない・完了までその列での操作は準備中と表示する）。
        if let Some((f, _, _)) = states.iter().find(|(f, t, s)| {
            *s == IndexState::Building
                && *f >= crate::model::FIRST_USER_FIELD_ID
                && needed.contains(&(*f, *t))
        }) {
            let key = bq.field(*f).key.clone();
            return Err(DataError::Conflict(format!(
                "フィールド '{key}' の索引は準備中です（構築が終わるまでお待ちください）"
            )));
        }
        // システム擬似列の初回バックフィル中（Task 15.2 より前のテーブル）などは本体経路
        // （正しいが遅い・statement_timeout で上限を切る）。
        // 材料化した派生列は索引行だけが値を持つ（本体に無い）。本体経路では評価できないので、機能フラグで
        // 本体経路を選んでいても索引経路で実行する。索引が使えない間・再計算待ちがある間はクエリ対象外
        // （古い値で絞らない・PIT-71）。
        let derived: Vec<&super::bound::BField> =
            bq.fields.values().filter(|f| f.derived).collect();
        let all_active = needed.iter().all(|n| active.contains(n));
        let body_path = !all_active || (tuning.read_path == ReadPath::Body && derived.is_empty());
        if let Some(f) = derived.first().filter(|_| body_path) {
            return Err(DataError::Conflict(format!(
                "フィールド '{}' の索引は準備中です（構築が終わるまでお待ちください）",
                f.key
            )));
        }
        if !derived.is_empty() {
            let ids: Vec<i16> = derived
                .iter()
                .filter_map(|f| i16::try_from(f.id).ok())
                .collect();
            let stale = crate::derived::materialize::pending_fields(conn, keys, &ids).await?;
            if let Some(f) = derived
                .iter()
                .find(|f| i16::try_from(f.id).is_ok_and(|i| stale.contains(&i)))
            {
                return Err(DataError::Conflict(format!(
                    "フィールド '{}' は参照先の変更を受けて再計算中です（完了までフィルタ/ソートに使えません）",
                    f.key
                )));
            }
        }
        // 個別共有（uuid）→ record_int（このテーブルに属するものだけ・外へ出さない）。
        let shared_ints: Vec<i64> = if body_path {
            Vec::new()
        } else {
            let mut v: Vec<i64> =
                Self::record_ints_for(conn, &ctx.tenant_id, keys, &m.shared_record_ids)
                    .await?
                    .into_values()
                    .collect();
            v.sort_unstable();
            v
        };
        let links = if body_path {
            HashMap::new()
        } else {
            self.resolve_links(conn, ctx, bq).await?
        };
        let low = Lowerer {
            q: bq,
            links: &links,
        };
        let scope_binding = fingerprint(query);
        let scope = CursorScope {
            tenant_int: keys.tenant_int,
            table_int: keys.table_int,
            binding: &scope_binding,
        };
        let after = match &query.cursor {
            Some(tok) if !bq.is_aggregate() => {
                Some(from_payload(bq, &self.cursor.open(&scope, tok)?)?)
            }
            _ => None,
        };
        let guard = Guard::new(IndexGuardCtx {
            schema: &table.schema,
            material: m,
            user: &ctx.principal.id,
            active: &active,
            shared_ints: &shared_ints,
        });
        // このソートの組み合わせの複合索引（active のときだけ駆動に使う・building は書込だけ受ける）。
        let composite = if bq.sort.len() > 1 && !body_path {
            let want: Vec<(FieldId, bool)> = bq.sort.iter().map(|k| (k.field, k.desc)).collect();
            crate::index::composite::load_sets(conn, keys)
                .await?
                .into_iter()
                .find(|s| s.active && s.keys == want)
                .map(|s| {
                    let kinds = bq
                        .sort
                        .iter()
                        .map(|k| (bq.field(k.field).kind, k.desc))
                        .collect();
                    (s.id, kinds)
                })
        } else {
            None
        };
        let mut ex = Exec {
            conn,
            low: &low,
            guard: &guard,
            keys,
            prober: super::planner::prober(&self.probe_cache, keys),
            stats: QueryExecStats {
                body_path,
                ..Default::default()
            },
            captured: Vec::new(),
            force: tuning.force,
            budget_override: tuning.sort_budget,
            composite,
            wants_composite: false,
            sort_degraded: false,
            last_pass: 0,
        };
        let mut result = QueryResult {
            items: None,
            groups: None,
            next_cursor: None,
            count: None,
            sort_precision: bq.sort_precision,
            sort_degraded: false,
            shares_truncated: false,
            suppressed: false,
        };
        let limit = i64::from(bq.limit);

        if bq.is_aggregate() {
            let rows = if body_path {
                ex.stats.strategy = "body";
                let mut sql = SqlBuf::scoped(keys.tenant_int, keys.table_int);
                let policy =
                    compile_read_predicate(&table.schema, m, &ctx.principal.id, "r", &mut sql)?;
                let w = super::v1::where_sql(bq, &ctx.tenant_id, &policy, &mut sql)?;
                let text = super::v1::aggregate_sql(bq, &w)?;
                ex.run_raw("aggregate", text, sql.binds).await?
            } else {
                ex.aggregate(&bq.filter).await?
            };
            let k = table
                .schema
                .aggregate_min_rows
                .unwrap_or(DEFAULT_AGGREGATE_MIN_ROWS);
            let (groups, suppressed) = super::aggregate::assemble(bq, &rows, k)?;
            result.groups = Some(groups);
            result.suppressed = suppressed;
            let debug = debug_of(ex.stats, ex.captured, &ex.prober);
            return Ok((result, debug, None));
        }

        // `limit: 0` かつ `with_count` は件数だけ（ページを取らない）。
        let count_only = query.limit == Some(0) && bq.with_count;
        let page: PageOut = if count_only {
            PageOut::default()
        } else if body_path {
            ex.stats.strategy = "body";
            let mut sql = SqlBuf::scoped(keys.tenant_int, keys.table_int);
            let policy =
                compile_read_predicate(&table.schema, m, &ctx.principal.id, "r", &mut sql)?;
            let w = super::v1::where_sql(bq, &ctx.tenant_id, &policy, &mut sql)?;
            let text = super::v1::page_sql(&low, &w, after.as_ref(), limit + 1, &mut sql)?;
            let rows = ex.run_raw("body_page", text, sql.binds).await?;
            body_page(bq, &rows, limit)?
        } else {
            ex.page(&bq.filter, &bq.sort, after.as_ref(), limit).await?
        };

        if bq.with_count {
            // 件数の上限はテナント設定（既定 10,000・data-platform §5.4）。
            let count_cap = self.count_cap_for(ex.conn, &ctx.tenant_id).await?;
            result.count = Some(if body_path {
                let mut sql = SqlBuf::scoped(keys.tenant_int, keys.table_int);
                let policy =
                    compile_read_predicate(&table.schema, m, &ctx.principal.id, "r", &mut sql)?;
                let w = super::v1::where_sql(bq, &ctx.tenant_id, &policy, &mut sql)?;
                let text = super::v1::count_sql(&w, count_cap as i64, &mut sql);
                ex.stats.count_issued = true;
                let rows = ex.run_raw("count", text, sql.binds).await?;
                let n: i64 = rows
                    .first()
                    .map(|r| r.try_get::<i64, _>(0))
                    .transpose()
                    .map_err(map_db)?
                    .unwrap_or(0);
                let n = n.max(0) as u64;
                if n > count_cap {
                    CountResult::AtLeast(count_cap)
                } else {
                    CountResult::Exact(n)
                }
            } else {
                let strategy = ex.stats.strategy;
                let c = ex.count(&bq.filter, count_cap).await?;
                ex.stats.strategy = strategy;
                c
            });
        }

        // 本体を取る（ページの行だけ）→ Rust で行述語を独立に再評価（§7.2）。
        let ints: Vec<i64> = page.rows.iter().map(|r| r.0).collect();
        let mut fetched: Vec<FetchedRow> = if ints.is_empty() {
            Vec::new()
        } else {
            let mut sql = SqlBuf::scoped(keys.tenant_int, keys.table_int);
            let text = fetch_sql(&ctx.tenant_id, ints.clone(), &mut sql);
            ex.stats.statements += 1;
            let rows =
                super::sql::bind_query_as(sqlx::query_as::<_, FetchedRow>(&text), &sql.binds)
                    .fetch_all(&mut *ex.conn)
                    .await
                    .map_err(map_db)?;
            ex.captured.push(super::planner::exec::Captured {
                label: "fetch",
                sql: text,
                binds: sql.binds,
            });
            rows
        };
        let order: HashMap<i64, usize> = ints.iter().enumerate().map(|(i, r)| (*r, i)).collect();
        fetched.sort_by_key(|r| order.get(&r.record_int).copied().unwrap_or(usize::MAX));
        let shared: HashSet<Uuid> = m.shared_record_ids.iter().copied().collect();
        let env = PolicyEnv {
            schema: &table.schema,
            material: m,
            user: &ctx.principal.id,
            shared: &shared,
        };
        ex.stats.verify_dropped = post_filter(
            &env,
            &mut fetched,
            |r| RowView {
                id: r.row.id,
                owner: &r.row.owner,
                data: &r.row.data.0,
            },
            table.id,
        );
        let items: Vec<DataRecord> = fetched
            .into_iter()
            .map(|r| r.row.into_record(&table.schema))
            .collect();
        if let Some(next) = &page.next {
            result.next_cursor = Some(self.cursor.seal(&scope, &to_payload(next))?);
        }
        result.sort_degraded = ex.sort_degraded;
        ex.stats.composite_requested = ex.wants_composite;
        let debug = debug_of(ex.stats, ex.captured, &ex.prober);
        Ok((result, debug, Some(items)))
    }

    /// record_ref の比較値（uuid）を参照先テーブルの `record_int` へ引き直す（同一テナント内）。
    async fn resolve_links(
        &self,
        conn: &mut PgConnection,
        ctx: &AuthContext,
        q: &BoundQuery,
    ) -> Result<HashMap<Uuid, i64>, DataError> {
        let mut by_table: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
        let mut atoms = Vec::new();
        q.filter.atoms(&mut atoms);
        for a in atoms {
            let f = q.field(a.field);
            let (FieldKind::Link, Some(rt)) = (f.kind, f.ref_table) else {
                continue;
            };
            let vals: Vec<&super::bound::Val> = match &a.leaf {
                super::bound::Leaf::Eq(v) => vec![v],
                super::bound::Leaf::In(v) => v.iter().collect(),
                _ => Vec::new(),
            };
            for v in vals {
                if let super::bound::Val::Ref(u) = v {
                    by_table.entry(rt).or_default().push(*u);
                }
            }
        }
        let mut out = HashMap::new();
        for (rt, ids) in by_table {
            let keys: Option<(i32, i64)> = sqlx::query_as(
                "SELECT tenant_int, table_int FROM data_table WHERE tenant_id = $1 AND id = $2",
            )
            .bind(&ctx.tenant_id)
            .bind(rt)
            .fetch_optional(&mut *conn)
            .await
            .map_err(map_db)?;
            if let Some((tenant_int, table_int)) = keys {
                let k = TableKeys {
                    tenant_int,
                    table_int,
                };
                out.extend(Self::record_ints_for(conn, &ctx.tenant_id, k, &ids).await?);
            }
        }
        Ok(out)
    }
}

impl Exec<'_, '_> {
    /// 本体経路の文を実行する（記録つき）。
    pub(crate) async fn run_raw(
        &mut self,
        label: &'static str,
        text: String,
        binds: Vec<Bind>,
    ) -> Result<Vec<sqlx::postgres::PgRow>, DataError> {
        self.stats.statements += 1;
        let rows = bind_q(sqlx::query(&text), &binds)
            .fetch_all(&mut *self.conn)
            .await
            .map_err(map_db)?;
        self.captured.push(super::planner::exec::Captured {
            label,
            sql: text,
            binds,
        });
        Ok(rows)
    }
}

/// 本体経路の行（`record_int`, `k0..`）を位置つきのページにする。
fn body_page(
    q: &BoundQuery,
    rows: &[sqlx::postgres::PgRow],
    limit: i64,
) -> Result<PageOut, DataError> {
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let rint: i64 = r.try_get("record_int").map_err(map_db)?;
        let mut vals = Vec::with_capacity(q.sort.len());
        for (i, k) in q.sort.iter().enumerate() {
            let v: Option<String> = r.try_get(format!("k{i}").as_str()).map_err(map_db)?;
            vals.push(SortVal::from_db(q.field(k.field).kind, v));
        }
        out.push((rint, Pos { vals, rint }));
    }
    let more = out.len() as i64 > limit;
    out.truncate(limit.max(0) as usize);
    let next = if more {
        out.last().map(|r| r.1.clone())
    } else {
        None
    };
    Ok(PageOut { rows: out, next })
}

fn debug_of(
    stats: QueryExecStats,
    captured: Vec<super::planner::exec::Captured>,
    prober: &super::planner::exec::ProberRef<'_>,
) -> QueryDebug {
    let mut statements: Vec<DebugStatement> = super::planner::probe_statements(prober)
        .map(|(sql, binds)| DebugStatement {
            label: "probe",
            sql: sql.clone(),
            params: binds.iter().cloned().map(DebugParam::from).collect(),
        })
        .collect();
    statements.extend(captured.into_iter().map(|c| DebugStatement {
        label: c.label,
        sql: c.sql,
        params: c.binds.into_iter().map(DebugParam::from).collect(),
    }));
    let mut stats = stats;
    stats.probes = prober.used;
    QueryDebug { stats, statements }
}

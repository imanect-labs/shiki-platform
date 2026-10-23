//! `DataStore` — 構造化データの単一チョークポイント（`&AuthContext` 経由）。
//!
//! テーブルの作成・取得・一覧・スキーマ改訂・論理削除。レコード操作は
//! [`crate::record`]、リビジョンは [`crate::revision`]（同じ struct の impl 分割）。
//!
//! # 接続プールは `PoolRouter` から引く（Task 15.1・data-platform §12 第 2 段）
//!
//! 本 crate は接続プールを直接保持しない。テナントの行は必ず
//! `self.pools.for_tenant(&ctx.tenant_id)` から引いたプールで読み書きし、横断一意の採番
//! （`tenant_int` / `table_int`）はレジストリのプールで行う。プール分割（第 2 段）はルータの
//! 差し替えだけで済む。禁止事項は docs/data/shard-safety.md（CI: `tests/shard_safety_lint.rs`）。

use std::sync::Arc;

use authz::{AuthContext, AuthzClient, Relation};
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::types::Json;
use storage::audit::{AuditEntry, AuditRecorder, Decision};
use storage::PoolRouter;
use uuid::Uuid;

use crate::cursor::{CursorCodec, CursorKey};
use crate::derived::links::create_symmetric_fields;
use crate::field_key::{assign_field_ids, upgrade_legacy_schema};
use crate::index::backfill::{build_unique_now, start_backfill, table_has_rows};
use crate::index::state::sync_ledger;
use crate::index::{DEFAULT_MAX_MULTI_VALUES, MAX_MULTI_VALUES_CEILING};
use crate::key_migration::KEY_FORMAT_LEGACY;
use crate::model::{DataTable, TableKeys, TableSchema};
use crate::policy::memo::AuthzMemo;
use crate::query::admission::{ReadGate, DEFAULT_READ_BUDGET_MS, DEFAULT_TENANT_READ_TOKENS};
use crate::query::planner::cost::CostModel;
use crate::query::planner::feedback::Feedback;
use crate::query::planner::{ProbeCache, StatsCache};
use crate::query::run::{QueryTuning, DEFAULT_COUNT_CAP, DEFAULT_READ_TIMEOUT_MS};
use crate::schema::{validate_schema_update, validate_table_name, validate_table_schema};
use crate::schema_cache::SchemaCache;
use crate::validate::RefResolver;
use crate::{map_db, DataError};

/// `tenant_int` で持つ構造化データの表（purge の対象・子 → 親の順）。
const PURGE_TENANT_INT_TABLES: &[&str] = &[
    "data_derived_pending",
    "data_derived_dep",
    "data_index_text",
    "data_index_num",
    "data_link",
    "data_link_field",
    "data_unique",
    "data_index_search",
    "data_index_composite",
    "data_sort_set",
    "data_index_state",
    "data_index_backfill",
    // 索引の降格の後片付け（Task 15.6）。
    "data_index_purge",
    // 一括取込（Task 15.11）。
    "data_bulk_key",
    "data_bulk_staging",
    "data_bulk_import",
    // 自前オプティマイザの統計 4 表（Task 15.12）。
    "data_value_count",
    "data_field_stats",
    "data_link_stats",
    "data_table_stats",
    // リンク値の保存形の移行の進捗（Task 15.4）。
    "data_link_value_migration",
];

/// テナント消去で `tenant_id` を条件に消す表（`data_table` / `data_tenant_settings` は個別に消す）。
/// SSE 配信の短命な状態（Task 15.6）。表名は固定集合のみ。
const PURGE_TENANT_ID_TABLES: &[&str] = &[
    "data_stream_authz_epoch",
    "data_stream_awareness",
    "data_stream_fanout",
];

/// 新規テーブルの入力。
#[derive(Debug, Clone)]
pub struct NewDataTable {
    pub name: String,
    pub schema: TableSchema,
}

/// data_table 行の SELECT 列（[`TableRow`] と同じ並び）。
pub(crate) const TABLE_COLS: &str = "id, name, app_id, schema, schema_version, created_by, \
     created_at, updated_at, tenant_int, table_int, key_format, next_field_id, has_record_shares, \
     planner_fixed";

/// data_table 行。
#[derive(sqlx::FromRow)]
pub(crate) struct TableRow {
    pub(crate) id: Uuid,
    name: String,
    pub(crate) app_id: Option<Uuid>,
    pub(crate) schema: Json<TableSchema>,
    schema_version: i64,
    created_by: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    pub(crate) tenant_int: i32,
    pub(crate) table_int: i64,
    pub(crate) key_format: i16,
    pub(crate) next_field_id: i32,
    /// レコード個別共有タプルが作られたか（§7.4 第 4 階層）。
    pub(crate) has_record_shares: bool,
    /// 固定戦略（Task 15.12・PIT-75）。
    pub(crate) planner_fixed: bool,
}

impl TableRow {
    /// テーブル名。
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// DTO へ変換する。v1 形式（未移行）のスキーマは**表示用に**決定的に 3 層へ引き上げる
    /// （移行ジョブが同じ規則で採番するため、移行の前後で `id` は変わらない）。
    pub(crate) fn into_table(self) -> DataTable {
        let schema = if self.key_format == KEY_FORMAT_LEGACY {
            upgrade_legacy_schema(&self.schema.0).map_or(self.schema.0, |(s, _)| s)
        } else {
            self.schema.0
        };
        DataTable {
            id: self.id,
            name: self.name,
            app_id: self.app_id,
            schema,
            schema_version: self.schema_version,
            created_by: self.created_by,
            created_at: self.created_at,
            updated_at: self.updated_at,
            keys: TableKeys {
                tenant_int: self.tenant_int,
                table_int: self.table_int,
            },
            has_record_shares: self.has_record_shares,
            planner_fixed: self.planner_fixed,
        }
    }
}

/// 構造化データのチョークポイント。
#[derive(Clone)]
pub struct DataStore {
    /// `tenant → pool` の解決（接続プールを直接持たない・Task 15.1）。
    pub(crate) pools: PoolRouter,
    pub(crate) authz: Arc<dyn AuthzClient>,
    pub(crate) audit: AuditRecorder,
    pub(crate) resolver: Arc<dyn RefResolver>,
    /// keyset カーソルの AEAD コーデック（クエリコンパイラ v2 のページング）。
    pub(crate) cursor: Arc<CursorCodec>,
    /// 多値（multi_select / リンク）の 1 レコードあたり要素数の上限（PIT-72・超過は 422）。
    pub(crate) max_multi_values: usize,
    /// 読取の調整（機能フラグ・強制戦略。既定は環境変数から・[`crate::QueryTuning`]）。
    pub(crate) tuning: QueryTuning,
    /// 件数の上限（`AtLeast(上限)`）。
    pub(crate) count_cap: u64,
    /// data の読取の `statement_timeout`（ミリ秒）。
    pub(crate) read_timeout_ms: u64,
    /// 計画時の件数探索の短期キャッシュ（Q の述語だけ・材料は入れない）。
    pub(crate) probe_cache: Arc<ProbeCache>,
    /// 走査効率などの運用指標・補正係数・予測と実測の相関（Task 15.12）。
    pub(crate) feedback: Arc<Feedback>,
    /// 統計表のプロセス内キャッシュ（全員共通・材料は入れない・PIT-75 で planner の外へ出さない）。
    pub(crate) stats_cache: Arc<StatsCache>,
    /// コストモデルの単価（起動時の較正で置き換わる）。
    pub(crate) costs: Arc<CostModel>,
    /// テナントの読取の同時実行の枠（予測コストのトークン・data-platform §16.8）。
    pub(crate) read_gate: Arc<ReadGate>,
    /// 読取予算（受付制御の上限・ミリ秒）。
    pub(crate) read_budget_ms: u64,
    /// スキーマ本文のキャッシュ（`(tenant_id, table_id, schema_version)`・認可材料は載せない・Task 15.9）。
    pub(crate) schema_cache: Arc<SchemaCache>,
    /// SSE の購読の登録簿（プロセス内・Task 15.6）。
    pub(crate) stream: Arc<crate::stream::hub::StreamRegistry>,
    /// SSE 配信の設定（TTL・保持期間・バッチ窓）。
    pub(crate) stream_config: crate::stream::StreamConfig,
    /// プランキャッシュ（クエリ形状 → SQL テキスト・認可材料は載せない・data-platform §16.3）。
    pub(crate) plan_cache: Arc<crate::query::planner::plan_cache::PlanCache>,
    /// 入口の資源上限（SQL 前段面と IR 直投入の共通・data-platform §16.1.1）。
    pub(crate) limits: crate::query::limits::QueryLimits,
}

impl DataStore {
    /// ストアを組む。`pools` は `PoolRouter`（単一プールなら接続プールからの `From` で可）。
    ///
    /// カーソル鍵は起動ごとの乱数鍵になる（再起動・レプリカ間でカーソルが無効になる）。
    /// 本番は [`DataStore::with_cursor_key`] で設定由来の鍵を注入すること。
    pub fn new(
        pools: impl Into<PoolRouter>,
        authz: Arc<dyn AuthzClient>,
        resolver: Arc<dyn RefResolver>,
    ) -> Self {
        let pools = pools.into();
        // 監査ログは現状レジストリと同じプールに置く（分割時に監査の置き場を決め直す）。
        let audit = AuditRecorder::new(pools.registry().clone());
        DataStore {
            pools,
            authz,
            audit,
            resolver,
            cursor: Arc::new(CursorCodec::new(&CursorKey::ephemeral())),
            max_multi_values: DEFAULT_MAX_MULTI_VALUES,
            tuning: QueryTuning::from_env(),
            count_cap: DEFAULT_COUNT_CAP,
            read_timeout_ms: DEFAULT_READ_TIMEOUT_MS,
            probe_cache: Arc::new(ProbeCache::default()),
            feedback: Arc::new(Feedback::default()),
            stats_cache: Arc::new(StatsCache::default()),
            costs: Arc::new(CostModel::default()),
            read_gate: Arc::new(ReadGate::new(DEFAULT_TENANT_READ_TOKENS)),
            read_budget_ms: DEFAULT_READ_BUDGET_MS,
            schema_cache: Arc::new(SchemaCache::default()),
            stream: Arc::default(),
            stream_config: crate::stream::StreamConfig::default(),
            plan_cache: Arc::default(),
            limits: crate::query::limits::QueryLimits::default(),
        }
    }

    /// 読取の調整（機能フラグ `read_path`・強制戦略）を設定する。結果は変わらず速さだけが変わる。
    #[must_use]
    pub fn with_query_tuning(mut self, tuning: QueryTuning) -> Self {
        self.tuning = tuning;
        self
    }

    /// 入口の資源上限を設定する（SQL 前段面と IR 直投入の共通・既定は data-platform §16.1.1 の表）。
    #[must_use]
    pub fn with_query_limits(mut self, limits: crate::query::limits::QueryLimits) -> Self {
        self.limits = limits;
        self
    }

    /// 件数の上限を設定する（既定 10,000・テナントの規模に合わせて変更可）。
    #[must_use]
    pub fn with_count_cap(mut self, cap: u64) -> Self {
        self.count_cap = cap.max(1);
        self
    }

    /// 読取予算（受付制御: 予測コストがこれを超えるクエリは実行しない・ミリ秒・既定 2,000）。
    #[must_use]
    pub fn with_read_budget_ms(mut self, ms: u64) -> Self {
        self.read_budget_ms = ms.max(1);
        self
    }

    /// テナントあたりの読取の同時実行の枠（予測コストのミリ秒・既定 8,000・data-platform §16.8）。
    #[must_use]
    pub fn with_tenant_read_tokens(mut self, tokens: u32) -> Self {
        self.read_gate = Arc::new(ReadGate::new(tokens));
        self
    }

    /// data の読取の `statement_timeout`（ミリ秒・既定 10,000）。
    #[must_use]
    pub fn with_read_timeout_ms(mut self, ms: u64) -> Self {
        self.read_timeout_ms = ms.max(1);
        self
    }

    /// 多値（multi_select / リンク）の 1 レコードあたり要素数の上限を設定する（既定
    /// [`DEFAULT_MAX_MULTI_VALUES`]・1〜32,767）。既存の値は検証しない（下げても過去の行は読める）。
    #[must_use]
    pub fn with_max_multi_values(mut self, n: usize) -> Self {
        self.max_multi_values = n.clamp(1, MAX_MULTI_VALUES_CEILING);
        self
    }

    /// keyset カーソルの暗号鍵を設定する（全レプリカで同じ鍵を使うこと）。
    #[must_use]
    pub fn with_cursor_key(mut self, key: &CursorKey) -> Self {
        self.cursor = Arc::new(CursorCodec::new(key));
        self
    }

    /// テーブルを作成する（スキーマ検証＋索引台帳＋作成者 owner タプル）。
    pub async fn create_table(
        &self,
        ctx: &AuthContext,
        input: NewDataTable,
        trace_id: Option<&str>,
    ) -> Result<DataTable, DataError> {
        self.create_table_bound(ctx, input, None, trace_id).await
    }

    /// アプリ所有テーブルを作成する（インストール時プロビジョン・Task 9.13b）。
    ///
    /// `app_id` を作成時に束縛する（app-gateway data.\* の所有束縛の正）。呼び出しは
    /// InstallService（同意フロー）に限る想定——スタンドアロン API からは `create_table` を使う。
    pub async fn create_table_for_app(
        &self,
        ctx: &AuthContext,
        input: NewDataTable,
        app_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<DataTable, DataError> {
        self.create_table_bound(ctx, input, Some(app_id), trace_id)
            .await
    }

    async fn create_table_bound(
        &self,
        ctx: &AuthContext,
        input: NewDataTable,
        app_id: Option<Uuid>,
        trace_id: Option<&str>,
    ) -> Result<DataTable, DataError> {
        let name = validate_table_name(&input.name)?;
        let mut schema = input.schema;
        let next_field_id = assign_field_ids(None, &mut schema)?;
        // 派生フィールド: 循環検出・値の型・材料化可否（4 階層）をサーバが決める（Task 15.4）。
        let prep = self
            .prepare_derived(ctx, None, app_id, false, &mut schema, None)
            .await?;
        validate_table_schema(&schema)?;
        // 内部整数キーはレジストリで採番する（不変・再利用しない・失敗時は欠番になるだけ）。
        let keys = self.allocate_table_keys(&ctx.tenant_id).await?;

        let db = self.pools.for_tenant(&ctx.tenant_id);
        let mut tx = db.begin().await.map_err(map_db)?;
        let row: TableRow = sqlx::query_as(&format!(
            "INSERT INTO data_table (tenant_id, org, name, schema, created_by, app_id, \
               tenant_int, table_int, key_format, next_field_id, pivot_index_init, \
               link_value_format) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 2, $9, true, 2) \
             RETURNING {TABLE_COLS}"
        ))
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(name)
        .bind(Json(&schema))
        .bind(&ctx.principal.id)
        .bind(app_id)
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .bind(next_field_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                DataError::Conflict(format!("テーブル名 '{name}' は既に存在します"))
            }
            _ => map_db(e),
        })?;
        // 対称列（参照先に置く逆リンク）を作り、採番した ID をこのテーブルのスキーマへ書き戻す。
        let row = if prep.needs_symmetric {
            create_symmetric_fields(&mut tx, &ctx.tenant_id, row.id, &mut schema, None).await?;
            sqlx::query_as(&format!(
                "UPDATE data_table SET schema = $3 WHERE tenant_id = $1 AND id = $2 \
                 RETURNING {TABLE_COLS}"
            ))
            .bind(&ctx.tenant_id)
            .bind(row.id)
            .bind(Json(&schema))
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db)?
        } else {
            row
        };
        // 索引台帳（空テーブルなので全て active・バックフィル不要）とリンク列の宣言。
        sync_ledger(&mut tx, keys, &schema, false).await?;
        let table = row.into_table();
        Self::finish_derived_save(&mut tx, &ctx.tenant_id, &table, None, &prep, false).await?;
        tx.commit().await.map_err(map_db)?;

        // 作成者を owner に（FGA）。失敗したら行を補償削除して孤立行を残さない。
        let id = table.id;
        let obj = ctx.ns().data_table(&id.to_string());
        if let Err(e) = self
            .authz
            .write_tuple(&ctx.subject(), Relation::Owner, &obj)
            .await
        {
            // 補償: 行を消す（索引はランタイム DDL を持たないので物理的な後片付けは無い）。
            if let Err(cleanup) =
                sqlx::query("DELETE FROM data_table WHERE tenant_id = $1 AND id = $2")
                    .bind(&ctx.tenant_id)
                    .bind(id)
                    .execute(db)
                    .await
            {
                tracing::error!(error = %cleanup, table_id = %id, "owner タプル書込失敗後の補償削除にも失敗（孤立行が残存）");
            }
            return Err(DataError::Internal(format!("owner tuple: {e}")));
        }
        // テーブルは確定済み。以降の監査失敗は結果に影響させない（重複再試行を防ぐ）。
        self.record_audit_best_effort(
            ctx,
            "data.table.create",
            &id.to_string(),
            trace_id,
            json!({ "name": name }),
        )
        .await;
        Ok(table)
    }

    /// テーブルのメタ＋スキーマを取得する（viewer）。
    pub async fn get_table(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<DataTable, DataError> {
        self.require(ctx, id, Relation::Viewer, "data.table.get", trace_id)
            .await?;
        self.fetch_live(ctx, id).await
    }

    /// スキーマを改訂する（owner・additive のみ・索引台帳の差分適用・楽観ロック）。
    pub async fn update_table_schema(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        next: TableSchema,
        expected_schema_version: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<DataTable, DataError> {
        self.require(
            ctx,
            id,
            Relation::Owner,
            "data.table.update_schema",
            trace_id,
        )
        .await?;
        // 未移行（v1 形式）なら先に 3 層へ移行させる（一方向・冪等）。
        self.fetch_live(ctx, id).await?;
        let db = self.pools.for_tenant(&ctx.tenant_id);
        let mut tx = db.begin().await.map_err(map_db)?;
        let current: Option<TableRow> = sqlx::query_as(&format!(
            "SELECT {TABLE_COLS} FROM data_table \
             WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL FOR UPDATE"
        ))
        .bind(&ctx.tenant_id)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db)?;
        let current = current.ok_or(DataError::NotFound)?;
        if let Some(expected) = expected_schema_version {
            if expected != current.schema_version {
                return Err(DataError::Conflict(format!(
                    "schema_version が一致しません（現在 {}）",
                    current.schema_version
                )));
            }
        }
        if current.key_format == KEY_FORMAT_LEGACY {
            // fetch_live が移行済みのはず（一方向）。ここへ来るのは並行の異常系のみ。
            return Err(DataError::Conflict(
                "テーブルの識別子移行が完了していません。再試行してください".into(),
            ));
        }
        // id の照合・採番（同じ id の key 変更・存在しない id の指定は 422）→ 互換性検証。
        let mut next = next;
        let next_field_id =
            assign_field_ids(Some((&current.schema.0, current.next_field_id)), &mut next)?;
        // 派生フィールド: 循環検出・値の型・材料化可否（4 階層）をサーバが決める（Task 15.4）。
        let prep = self
            .prepare_derived(
                ctx,
                Some(id),
                current.app_id,
                current.has_record_shares,
                &mut next,
                Some(&current.schema.0),
            )
            .await?;
        if prep.needs_symmetric {
            create_symmetric_fields(
                &mut tx,
                &ctx.tenant_id,
                id,
                &mut next,
                Some(&current.schema.0),
            )
            .await?;
        }
        validate_schema_update(&current.schema.0, &next)?;
        // display_name / order / description の変更はここ（data_table の 1 行）だけで完結し、
        // data_record は 1 行も更新しない（JSONB キーは不変の id 由来・data-platform §4.2）。
        let row: TableRow = sqlx::query_as(&format!(
            "UPDATE data_table \
             SET schema = $3, schema_version = schema_version + 1, next_field_id = $4, \
                 updated_at = now() \
             WHERE tenant_id = $1 AND id = $2 \
             RETURNING {TABLE_COLS}"
        ))
        .bind(&ctx.tenant_id)
        .bind(id)
        .bind(Json(&next))
        .bind(next_field_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db)?;
        // 索引台帳を宣言へ合わせる（増えた索引は既存レコードがあれば building にしてバックフィルを積む・
        // テーブル行は FOR UPDATE 済みなので、並行する書込は遷移の前後どちらかに直列化される）。
        let keys = TableKeys {
            tenant_int: current.tenant_int,
            table_int: current.table_int,
        };
        let has_rows = table_has_rows(&mut tx, keys).await?;
        if sync_ledger(&mut tx, keys, &next, has_rows).await? {
            // unique は同じ Tx で同期構築（重複は 409・一意性の強制に空白期間を作らない）。
            build_unique_now(&mut tx, keys, &next, &ctx.tenant_id).await?;
            start_backfill(&mut tx, keys, &ctx.tenant_id, id).await?;
        }
        let current_schema = current.schema.0.clone();
        let row = row.into_table();
        // 材料化の依存登録・昇格した列の再計算待ち・このテーブルを参照先にする列の降格（PIT-71）。
        Self::finish_derived_save(
            &mut tx,
            &ctx.tenant_id,
            &row,
            Some(&current_schema),
            &prep,
            has_rows,
        )
        .await?;
        // 実データ変更のためスキーマ更新と同一 Tx で監査を残す（原子的・Chain=Yes）。
        self.record_audit_on(
            &mut tx,
            ctx,
            "data.table.update_schema",
            &id.to_string(),
            trace_id,
            json!({ "schema_version": row.schema_version }),
        )
        .await?;
        // 購読者へスキーマの改訂を知らせる（SSE は schema.changed を送って閉じる・Task 15.6）。
        crate::stream::outbox::emit_schema_changed(&mut tx, ctx, &row, trace_id).await?;
        tx.commit().await.map_err(map_db)?;
        // 版は DB 照合で必ず変わって見えるが、古い本文はここで落としておく（世代切替）。
        self.schema_cache.invalidate_table(&ctx.tenant_id, id);
        Ok(row)
    }

    /// テーブルを論理削除する（owner・レコード/履歴は保持・名前は再利用可能になる）。
    pub async fn delete_table(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(), DataError> {
        self.require(ctx, id, Relation::Owner, "data.table.delete", trace_id)
            .await?;
        let mut tx = self
            .pools
            .for_tenant(&ctx.tenant_id)
            .begin()
            .await
            .map_err(map_db)?;
        let updated: Option<(i32, i64)> = sqlx::query_as(
            "UPDATE data_table SET deleted_at = now(), updated_at = now() \
             WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL \
             RETURNING tenant_int, table_int",
        )
        .bind(&ctx.tenant_id)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db)?;
        let Some((tenant_int, table_int)) = updated else {
            return Err(DataError::NotFound);
        };
        // このテーブルを参照先にしていた材料化列は前提が無くなるので降格する（同一 Tx・PIT-71）。
        crate::derived::materialize::demote_dependents(
            &mut tx,
            &ctx.tenant_id,
            TableKeys {
                tenant_int,
                table_int,
            },
            None,
            "参照先テーブルが削除された",
        )
        .await?;
        tx.commit().await.map_err(map_db)?;
        self.schema_cache.invalidate_table(&ctx.tenant_id, id);
        self.record_audit_best_effort(
            ctx,
            "data.table.delete",
            &id.to_string(),
            trace_id,
            json!({}),
        )
        .await;
        Ok(())
    }

    /// テナント消去（SAAS.2）: data_table（CASCADE で record/registry/revision も）と
    /// FGA タプル・**索引テーブルと台帳**（`tenant_int` で持つ表・Task 15.2）を撤去する。
    /// 監査ログは保持（purge 方針は storage と同一）。
    pub async fn purge_tenant(&self, tenant_id: &str) -> Result<u32, DataError> {
        let db = self.pools.for_tenant(tenant_id);
        let tenant_int: Option<i32> =
            sqlx::query_scalar("SELECT tenant_int FROM tenant_placement WHERE tenant_id = $1")
                .bind(tenant_id)
                .fetch_optional(self.pools.registry())
                .await
                .map_err(map_db)?;
        let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM data_table WHERE tenant_id = $1")
            .bind(tenant_id)
            .fetch_all(db)
            .await
            .map_err(map_db)?;
        let ns = authz::Namespace::for_tenant(tenant_id);
        let mut purged = 0u32;
        for id in &ids {
            // FGA タプルを撤去（索引は行なので下の DELETE で消える）。
            self.authz
                .delete_object_tuples(&ns.data_table(&id.to_string()))
                .await
                .map_err(|e| DataError::Internal(format!("fga purge: {e}")))?;
            purged += 1;
        }
        let mut tx = db.begin().await.map_err(map_db)?;
        if let Some(tenant_int) = tenant_int {
            // 子（data_link は data_link_field を参照）→ 親の順。表名は固定集合のみ。
            for table in PURGE_TENANT_INT_TABLES {
                sqlx::query(&format!("DELETE FROM {table} WHERE tenant_int = $1"))
                    .bind(tenant_int)
                    .execute(&mut *tx)
                    .await
                    .map_err(map_db)?;
            }
        }
        for table in PURGE_TENANT_ID_TABLES {
            sqlx::query(&format!("DELETE FROM {table} WHERE tenant_id = $1"))
                .bind(tenant_id)
                .execute(&mut *tx)
                .await
                .map_err(map_db)?;
        }
        sqlx::query("DELETE FROM data_table WHERE tenant_id = $1")
            .bind(tenant_id)
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
        sqlx::query("DELETE FROM data_tenant_settings WHERE tenant_id = $1")
            .bind(tenant_id)
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
        tx.commit().await.map_err(map_db)?;
        // 撤去したテナントのスキーマを世代ごと落とす（purge 前に読み始めた読取も載せ戻せない）。
        self.schema_cache.invalidate_tenant(tenant_id);
        Ok(purged)
    }

    /// テーブルの内部整数キーを採番する（`tenant_int` は配置レジストリ・`table_int` は採番サービス）。
    async fn allocate_table_keys(&self, tenant_id: &str) -> Result<TableKeys, DataError> {
        let mut conn = self.pools.registry().acquire().await.map_err(map_db)?;
        let placement = storage::tenant::ensure_placement(&mut conn, tenant_id)
            .await
            .map_err(|e| DataError::Internal(format!("tenant placement: {e}")))?;
        let table_int = storage::tenant::allocate_table_int(&mut conn)
            .await
            .map_err(|e| DataError::Internal(format!("table_int: {e}")))?;
        Ok(TableKeys {
            tenant_int: placement.tenant_int,
            table_int,
        })
    }

    /// このリクエストの認可メモを作る（Task 15.9）。
    ///
    /// メモは `self` の authz を借用するので `'static` ではなく、リクエスト（公開メソッドの 1 回）の
    /// 外へ持ち出せない（PIT-18: リクエストを跨いだ再利用を型で禁止する）。
    pub(crate) fn authz_memo(&self) -> AuthzMemo<'_> {
        AuthzMemo::new(self.authz.as_ref())
    }

    /// 検証用: テナントについてスキーマキャッシュが保持する `(table_id, schema_version)`。
    #[doc(hidden)]
    pub fn cached_schema_versions(&self, tenant_id: &str) -> Vec<(Uuid, i64)> {
        self.schema_cache.cached_versions(tenant_id)
    }

    /// テーブルへの relation を確かめる（生存テーブルのみ・不足は監査 deny＋Forbidden）。
    ///
    /// 委譲で動く主体（ワークフローの実行主体など）が「このテーブルを委譲されているか」を、
    /// 実際の読み書きを別の主体で行う前に確かめる入口（Task 15.7）。実際の読み書きは各 API が
    /// 自分の主体で改めて認可する（これは追加の門で、代わりにはならない）。
    pub async fn authorize_table(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        relation: Relation,
        action: &str,
        trace_id: Option<&str>,
    ) -> Result<(), DataError> {
        self.require(ctx, table_id, relation, action, trace_id)
            .await?;
        self.fetch_live(ctx, table_id).await.map(|_| ())
    }

    /// テーブルへの relation を要求する（不足は監査 deny＋Forbidden・剥奪即時反映）。
    pub(crate) async fn require(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        relation: Relation,
        action: &str,
        trace_id: Option<&str>,
    ) -> Result<(), DataError> {
        self.require_in(&self.authz_memo(), ctx, id, relation, action, trace_id)
            .await
    }

    /// [`Self::require`] のリクエストメモ版（同じリクエストで先に引いた答えを使う）。
    pub(crate) async fn require_in(
        &self,
        az: &AuthzMemo<'_>,
        ctx: &AuthContext,
        id: Uuid,
        relation: Relation,
        action: &str,
        trace_id: Option<&str>,
    ) -> Result<(), DataError> {
        let obj = ctx.ns().data_table(&id.to_string());
        let ok = az.check(ctx, relation, &obj).await?;
        if !ok {
            let _ = self
                .audit
                .record(
                    ctx,
                    AuditEntry {
                        action,
                        object_type: "data_table",
                        object_id: &id.to_string(),
                        decision: Decision::Deny,
                        trace_id,
                        metadata: json!({ "relation": relation.as_str() }),
                    },
                )
                .await;
            return Err(DataError::Forbidden);
        }
        Ok(())
    }

    /// 既存 Tx 上で監査を残す（実データ変更用・Chain=Yes・書込と原子的）。
    pub(crate) async fn record_audit_on(
        &self,
        conn: &mut sqlx::PgConnection,
        ctx: &AuthContext,
        action: &str,
        object_id: &str,
        trace_id: Option<&str>,
        metadata: serde_json::Value,
    ) -> Result<(), DataError> {
        storage::audit::record_on(
            conn,
            ctx,
            AuditEntry {
                action,
                object_type: "data_table",
                object_id,
                decision: Decision::Allow,
                trace_id,
                metadata,
            },
            storage::audit::Chain::Yes,
        )
        .await
        .map_err(|e| DataError::Internal(format!("audit: {e}")))
    }

    /// コミット済み操作の事後監査（失敗しても結果を覆さない・ログのみ）。
    pub(crate) async fn record_audit_best_effort(
        &self,
        ctx: &AuthContext,
        action: &str,
        object_id: &str,
        trace_id: Option<&str>,
        metadata: serde_json::Value,
    ) {
        if let Err(e) = self
            .record_audit(ctx, action, object_id, trace_id, metadata)
            .await
        {
            tracing::error!(error = %e, action, object_id, "監査ログの書込に失敗（操作自体は成功済み）");
        }
    }

    pub(crate) async fn record_audit(
        &self,
        ctx: &AuthContext,
        action: &str,
        object_id: &str,
        trace_id: Option<&str>,
        metadata: serde_json::Value,
    ) -> Result<(), DataError> {
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action,
                    object_type: "data_table",
                    object_id,
                    decision: Decision::Allow,
                    trace_id,
                    metadata,
                },
            )
            .await
            .map_err(|e| DataError::Internal(format!("audit: {e}")))
    }
}

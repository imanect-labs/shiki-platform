//! 一括書込パス（Task 15.11・data-platform §16.7）。
//!
//! 10 万行のインポートは本体 10 万行 ＋ 索引 80 万行になり、1 行ずつの INSERT では成立しない。一方で
//! **COPY を authoritative な表へ直接流すと、通常の書込経路が担保する不変条件（認可・unique・リンクの
//! カーディナリティ・`rev`・outbox・材料化・索引 parity）がまとめて抜ける**。そこで 3 段にする。
//!
//! ```text
//! ① ステージング表（data_bulk_staging）へ COPY        … 型変換（JSONB の構文）と整形だけ。本体には触れない
//! ② ステージング上で正規検証（validate.rs）            … 通常書込と同じ検証器
//!      認可（editor・参照先の可視性）・型・required・多値要素数上限・unique・リンクのカーディナリティ
//! ③ チャンク Tx で適用（apply.rs）                      … 1 Tx で全件を抱えない
//!      本体 ＋ 索引行 ＋ revision（rev = 1）＋ `Invariant` 材料化を 1 件の create と**同じ入口**で書く
//!      （crate::record_effects。Task 15.6 の outbox もそこで発行すれば一括経路も覆われる）
//! ④ 索引の仕上げ（apply.rs）                            … 遅延構築したなら building を解放してバックフィル、
//!      最後に本体と索引の parity 検証
//! ```
//!
//! **更新行（upsert・Task 15.7）**: [`DataStore::start_bulk_upsert`] は `record_id` を持つ行を既存
//! レコードの更新として扱う。② では形だけを見て、③ で 1 件の更新と同じ入口
//! （`crate::record_update` の `update_locked`）を行ごとのセーブポイントで通す（upsert.rs）。
//!
//! # 冪等・中断再開
//!
//! - 各段は jobq のチャンクジョブ（[`BULK_IMPORT_QUEUE`]）。1 チャンク = 1 Tx で、取込行（`data_bulk_import`）を
//!   `FOR UPDATE` で押さえ、**同じ Tx で**行番号のカーソルとステージング行の状態を進める。落ちたチャンクは
//!   ロールバックされ、再配信で同じ位置からやり直す（取込 ID ＋行番号で二重適用しない）。
//! - 作成するレコードの外部 ID は COPY 時に採番してステージング行に持つ。本体の主キー
//!   `(tenant_int, table_int, id)` と一致するので、万一同じ行を二度適用しようとしても本体は 1 行のまま。
//!
//! # 索引の遅延構築（§10 の `building` を流用）
//!
//! 大きな取込（既定 2 万行以上かつ既存行数以上）は、ユーザー列の値索引（text / num / search）を
//! `building` にして**取込の行についてだけ**索引行を書かずに本体を入れ、適用後にバックフィルで作る。
//! `building` 中も通常書込は write-through する（[`crate::index::state`]）ので、取込中の並行書込は
//! 完成後の索引から欠落しない。保持中（`index_hold`）はバックフィルが `active` へ切り替えない。
//! unique（`data_unique`）・リンク（`data_link`）・擬似列・材料化列は遅延させない（一意性・カーディナリティの
//! 強制と、全件を走査できる駆動索引を空白にしないため）。
//!
//! # 不変条件
//!
//! - 取込の主体は `AuthContext` として保存し（principal・org・tenant）、ジョブで組み直す（昇格しない）。
//!   各チャンクで editor を OpenFGA へ問い直す（剥奪は次のチャンクから効く）。
//! - `record_int` / `table_int` は外へ出さない。観測 API は件数・行番号・行キー・エラー文言だけを返す。

mod apply;
pub(crate) mod csv;
mod job;
mod resolver;
mod upsert;
mod validate;

use std::fmt::Write as _;
use storage::audit::AuditAction;

use authz::{AuthContext, PrincipalKind, Relation};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::model::TableKeys;
use crate::store::DataStore;
use crate::{map_db, DataError};

pub use csv::{MAX_BULK_CSV_BYTES, ROW_KEY_COLUMN};
pub use job::BulkImportWorker;

/// 一括取込ジョブのキュー名。
pub const BULK_IMPORT_QUEUE: &str = "data_bulk_import";
/// 1 チャンク（1 Tx）で検証・適用する行数。
pub const BULK_CHUNK: i64 = 500;
/// 1 回の取込の最大行数。
pub const MAX_BULK_ROWS: usize = 1_000_000;
/// 1 行（JSON 直列化後）の最大バイト数。
pub const MAX_BULK_ROW_BYTES: usize = 256 * 1024;
/// 行キーの最大バイト数。
pub const MAX_BULK_ROW_KEY_BYTES: usize = 200;
/// `auto` で索引を遅延構築する最小行数（data-platform §10 の「2 万行以上は裏で作る」と同じ値）。
pub const LAZY_INDEX_MIN_ROWS: i64 = 20_000;

/// ステージング行の状態（`data_bulk_staging.state`）。
pub(crate) const ST_STAGED: i16 = 0;
pub(crate) const ST_VALID: i16 = 1;
pub(crate) const ST_INVALID: i16 = 2;
pub(crate) const ST_APPLIED: i16 = 3;
pub(crate) const ST_REJECTED: i16 = 4;

/// 索引の作り方。
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum BulkIndexMode {
    /// 行数で決める（[`LAZY_INDEX_MIN_ROWS`] 以上かつ既存行数以上なら遅延構築）。
    #[default]
    Auto,
    /// 適用と同じ Tx で全索引行を書く（取込中も全列でクエリできる）。
    Immediate,
    /// 値索引を `building` にして適用後にバックフィルする（取込中は当該列のクエリが 409「準備中」）。
    Deferred,
}

impl BulkIndexMode {
    fn as_str(self) -> &'static str {
        match self {
            BulkIndexMode::Auto => "auto",
            BulkIndexMode::Immediate => "immediate",
            BulkIndexMode::Deferred => "deferred",
        }
    }

    fn parse(s: &str) -> BulkIndexMode {
        match s {
            "immediate" => BulkIndexMode::Immediate,
            "deferred" => BulkIndexMode::Deferred,
            _ => BulkIndexMode::Auto,
        }
    }
}

/// 取込の 1 行。
#[derive(Debug, Clone, PartialEq)]
pub struct BulkImportRow {
    /// 呼び出し側の行キー（任意・取込内で一意）。エラー報告で行を特定する。
    pub key: Option<String>,
    /// レコードの値（外部形 `key → 値`・create と同じ形）。
    pub data: Value,
}

/// 一括 upsert の 1 行（`record_id` があれば既存レコードの更新・無ければ新規作成）。
///
/// 更新行は 1 件の更新と同じ入口（行述語つきロック・書込述語・`rev`・マスク列・検証器・outbox）を
/// 適用時に行ロックの下で通る（[`crate::DataStore::update_record`] と同じ意味論・Task 15.7）。
#[derive(Debug, Clone, PartialEq)]
pub struct BulkUpsertRow {
    /// 呼び出し側の行キー（任意・取込内で一意）。
    pub key: Option<String>,
    /// 更新対象の外部 ID（`None` は新規作成）。
    pub record_id: Option<Uuid>,
    /// 更新の楽観ロック（`None` は照合しない＝行ロックの下の後勝ち）。作成行には指定できない。
    pub expected_rev: Option<i64>,
    /// 作成行はレコードの値（外部形）、更新行は merge patch（`null` はフィールド除去）。
    pub data: Value,
}

impl From<BulkImportRow> for BulkUpsertRow {
    fn from(r: BulkImportRow) -> Self {
        BulkUpsertRow {
            key: r.key,
            record_id: None,
            expected_rev: None,
            data: r.data,
        }
    }
}

/// ステージング行の操作（`data_bulk_staging.op`）。
pub(crate) const OP_CREATE: i16 = 0;
pub(crate) const OP_UPDATE: i16 = 1;

/// 取込のオプション。
#[derive(Debug, Clone, Copy, Default)]
pub struct BulkImportOptions {
    pub index_mode: BulkIndexMode,
    /// 検証で不合格の行が 1 つでもあれば何も適用しない（既定は合格した行だけを適用する）。
    pub abort_on_invalid: bool,
}

/// 取込の進捗（運用・UI の観測用・内部キーを含まない）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub struct BulkImportStatus {
    pub id: Uuid,
    pub table_id: Uuid,
    /// `validating` / `applying` / `indexing` / `done` / `failed`。
    pub status: String,
    pub rows_total: i64,
    /// 検証を通った行（適用済み・適用時の拒否を含む）。
    pub rows_valid: i64,
    pub rows_invalid: i64,
    pub rows_applied: i64,
    /// 検証後、適用時に並行書込と競合して弾かれた行（unique・リンクのカーディナリティ）。
    pub rows_rejected: i64,
    /// 索引を遅延構築している（このテーブルの値索引を `building` に保持している）。
    pub index_deferred: bool,
    pub parity_checked_at: Option<DateTime<Utc>>,
    /// 取込後の parity 検証の不一致件数（0 で本体と索引が一致）。
    pub parity_mismatches: Option<i32>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// 不合格・拒否された行（行番号は 1 始まり・CSV なら見出し行を除いたデータの何行目か）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub struct BulkRowError {
    pub row_no: i64,
    pub key: Option<String>,
    pub message: String,
}

/// [`DataStore::bulk_import_step`] の 1 回の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkImportStep {
    /// このステップの後の状態。
    pub status: String,
    /// このステップで検証・適用・バックフィルした行数。
    pub processed: u64,
    /// 取込が終わった（`done` / `failed`）。
    pub done: bool,
}

/// `data_bulk_import` の 1 行（crate 内専用・内部キーを含む）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct ImportRow {
    pub(crate) id: Uuid,
    pub(crate) tenant_int: i32,
    pub(crate) table_int: i64,
    pub(crate) table_id: Uuid,
    pub(crate) principal_kind: String,
    pub(crate) principal_id: String,
    pub(crate) org: String,
    pub(crate) trace_id: Option<String>,
    pub(crate) status: String,
    pub(crate) schema_version: i64,
    pub(crate) abort_on_invalid: bool,
    pub(crate) index_mode: String,
    pub(crate) index_hold: bool,
    pub(crate) validate_cursor: i64,
    pub(crate) apply_cursor: i64,
    pub(crate) rows_valid: i64,
    pub(crate) rows_invalid: i64,
    pub(crate) first_record_int: Option<i64>,
}

/// [`ImportRow`] の SELECT 列。
pub(crate) const IMPORT_COLS: &str = "id, tenant_int, table_int, table_id, principal_kind, \
     principal_id, org, trace_id, status, schema_version, abort_on_invalid, index_mode, index_hold, \
     validate_cursor, apply_cursor, rows_valid, rows_invalid, first_record_int";

impl ImportRow {
    pub(crate) fn keys(&self) -> TableKeys {
        TableKeys {
            tenant_int: self.tenant_int,
            table_int: self.table_int,
        }
    }

    pub(crate) fn index_mode(&self) -> BulkIndexMode {
        BulkIndexMode::parse(&self.index_mode)
    }

    /// 取込の主体の `AuthContext` を組み直す（昇格しない・グループ等の IdP 由来の属性は持たない。
    /// data の認可は principal の id と種別だけで OpenFGA に問う）。
    pub(crate) fn auth_context(&self, tenant_id: &str) -> Result<AuthContext, DataError> {
        let kind: PrincipalKind =
            serde_json::from_value(Value::String(self.principal_kind.clone()))
                .map_err(|_| DataError::Internal("取込の主体の種別が不正です".into()))?;
        Ok(AuthContext::new(
            authz::Principal {
                kind,
                id: self.principal_id.clone(),
                email: None,
                groups: Vec::new(),
                roles: Vec::new(),
                tenant_id: Some(tenant_id.to_string()),
            },
            self.org.clone(),
            tenant_id.to_string(),
        ))
    }
}

/// 取込行を読む（`lock` なら `FOR UPDATE`）。
pub(crate) async fn load_import(
    conn: &mut PgConnection,
    tenant_int: i32,
    id: Uuid,
    lock: bool,
) -> Result<Option<ImportRow>, DataError> {
    let sql = format!(
        "SELECT {IMPORT_COLS} FROM data_bulk_import WHERE tenant_int = $1 AND id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    sqlx::query_as(&sql)
        .bind(tenant_int)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db)
}

/// テーブル `keys` の値索引を保持している取込があるか（保持中はバックフィルが `active` へ切り替えない）。
pub(crate) async fn index_held(
    conn: &mut PgConnection,
    keys: TableKeys,
) -> Result<bool, DataError> {
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM data_bulk_import \
         WHERE tenant_int = $1 AND table_int = $2 AND index_hold)",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db)
}

/// 取込ジョブを同じ Tx で積む。
pub(crate) async fn enqueue_import(
    conn: &mut PgConnection,
    tenant_id: &str,
    table_id: Uuid,
    import_id: Uuid,
    trace_id: Option<&str>,
) -> Result<(), DataError> {
    jobq::enqueue_on(
        conn,
        jobq::NewJob {
            queue: BULK_IMPORT_QUEUE,
            tenant_id,
            payload: &json!({ "import_id": import_id, "table_id": table_id, "v": 1 }),
            trace_id,
            max_attempts: 10,
        },
    )
    .await
    .map_err(|e| DataError::Internal(format!("jobq: {e}")))?;
    Ok(())
}

/// COPY の text 形式のフィールドへエスケープする（`\` / 改行 / 復帰 / タブ）。
fn copy_escape(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
}

/// ステージングへ送る COPY のデータ行を組む（行の形の検査・行キーと更新対象の重複検査を兼ねる）。
#[derive(Default)]
struct CopyRows {
    buf: String,
    seen_keys: std::collections::HashSet<String>,
    seen_targets: std::collections::HashSet<Uuid>,
    /// 更新行の数（監査用）。
    updates: i64,
}

impl CopyRows {
    /// 1 行を COPY の text 形式で積む（列順は `start_bulk_upsert` の COPY 文と同じ）。
    fn push_row(
        &mut self,
        tenant_int: i32,
        import_id: Uuid,
        row_no: usize,
        row: &BulkUpsertRow,
    ) -> Result<(), DataError> {
        if !row.data.is_object() {
            return Err(DataError::Invalid(format!(
                "{row_no} 行目: data は JSON オブジェクトである必要があります"
            )));
        }
        // NUL は Postgres の jsonb が保存できない（取込の開始で 400・500 にしない・検証 D-8）。
        if crate::validate::contains_nul(&row.data)
            || row.key.as_deref().is_some_and(|k| k.contains('\0'))
        {
            return Err(DataError::input_detail(
                crate::InvalidCode::InvalidInput,
                None,
                format!("{row_no} 行目: NUL 文字（U+0000）は使えません"),
            ));
        }
        let payload = row.data.to_string();
        if payload.len() > MAX_BULK_ROW_BYTES {
            return Err(DataError::Invalid(format!(
                "{row_no} 行目が大きすぎます（最大 {MAX_BULK_ROW_BYTES} バイト）"
            )));
        }
        let buf = &mut self.buf;
        let _ = write!(buf, "{tenant_int}\t{import_id}\t{row_no}\t");
        match &row.key {
            Some(k) => {
                if k.len() > MAX_BULK_ROW_KEY_BYTES || k.chars().any(char::is_control) {
                    return Err(DataError::Invalid(format!(
                        "{row_no} 行目の行キーが不正です（最大 {MAX_BULK_ROW_KEY_BYTES} バイト・制御文字不可）"
                    )));
                }
                if !self.seen_keys.insert(k.clone()) {
                    return Err(DataError::Invalid(format!(
                        "{row_no} 行目の行キー '{k}' が重複しています"
                    )));
                }
                copy_escape(buf, k);
            }
            None => buf.push_str("\\N"),
        }
        let (record_id, op) = if let Some(id) = row.record_id {
            if !self.seen_targets.insert(id) {
                return Err(DataError::Invalid(format!(
                    "{row_no} 行目: 同じレコードを 1 回の取込で 2 回更新できません"
                )));
            }
            self.updates += 1;
            (id, OP_UPDATE)
        } else {
            if row.expected_rev.is_some() {
                return Err(DataError::Invalid(format!(
                    "{row_no} 行目: expected_rev は更新行（record_id あり）にだけ指定できます"
                )));
            }
            (Uuid::new_v4(), OP_CREATE)
        };
        let _ = write!(buf, "\t{record_id}\t");
        copy_escape(buf, &payload);
        let _ = write!(buf, "\t{op}\t");
        match row.expected_rev {
            Some(rev) => {
                let _ = write!(buf, "{rev}");
            }
            None => buf.push_str("\\N"),
        }
        buf.push('\n');
        Ok(())
    }
}

/// 1 回の COPY 送信のバッファ上限（バイト）。
const COPY_FLUSH_BYTES: usize = 1 << 20;

impl DataStore {
    /// 一括取込を始める（① ステージング表への COPY・editor）。取込 ID を返す。
    ///
    /// 行の形（JSON オブジェクト・サイズ・行キーの一意）だけをここで検査し、型・必須・unique・参照の
    /// 検証と適用はジョブ（[`Self::bulk_import_step`]）が行う。ステージングの COPY・取込行・最初の
    /// ジョブ・監査は同じ Tx（全部入るか何も入らないか）。
    pub async fn start_bulk_import(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        rows: Vec<BulkImportRow>,
        opts: BulkImportOptions,
        trace_id: Option<&str>,
    ) -> Result<Uuid, DataError> {
        let rows = rows.into_iter().map(BulkUpsertRow::from).collect();
        self.start_bulk_upsert(ctx, table_id, rows, opts, trace_id)
            .await
    }

    /// 一括 upsert を始める（更新行と作成行を混ぜた取込・editor）。取込 ID を返す。
    ///
    /// 作成行は [`Self::start_bulk_import`] と同じ経路（検証 → チャンク適用）。更新行は検証段では形だけを
    /// 見て、適用段で 1 件の更新と同じ入口を行ロックの下で通す（検証から適用までの間の並行書込も
    /// `rev`・行述語の評価に反映される）。同じレコードを 2 回更新する取込は拒否する。
    pub async fn start_bulk_upsert(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        rows: Vec<BulkUpsertRow>,
        opts: BulkImportOptions,
        trace_id: Option<&str>,
    ) -> Result<Uuid, DataError> {
        self.start_bulk_upsert_with_id(ctx, table_id, Uuid::new_v4(), rows, opts, trace_id)
            .await
    }

    /// 取込 ID を呼び出し側が決める [`Self::start_bulk_upsert`]（**同じ ID の 2 回目は何もせず同じ ID を返す**）。
    ///
    /// ワークフローの `data.bulk_upsert` はステップの冪等キーから取込 ID を決め、effect_journal の
    /// 回収窓（予約したワーカーが落ちた後の再実行）でも二重に取り込まない（続きはジョブが進める）。
    /// 別のテーブルの取込 ID と衝突したら 409。
    pub async fn start_bulk_upsert_with_id(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        import_id: Uuid,
        rows: Vec<BulkUpsertRow>,
        opts: BulkImportOptions,
        trace_id: Option<&str>,
    ) -> Result<Uuid, DataError> {
        self.require(
            ctx,
            table_id,
            Relation::Editor,
            AuditAction::DataRecordBulkImport,
            trace_id,
        )
        .await?;
        let table = self.fetch_live(ctx, table_id).await?;
        if rows.is_empty() {
            return Err(DataError::Invalid("取り込む行がありません".into()));
        }
        // 読めない（マスクされた）列へ書く作成行を含む取込は全体を 403（1 件の作成と同じ単一点・PIT-19）。
        // 検証段でも行ごとに確かめ直す（開始から検証までの間のポリシー変更に追従する）。更新行は適用段で
        // 1 件の更新と同じ入口（行ロックの下）が行ごとに拒否する。
        let az = self.authz_memo();
        let masked = self.ensure_fields_writable(ctx, &az, &table, None).await?;
        for r in rows.iter().filter(|r| r.record_id.is_none()) {
            if let Some(obj) = r.data.as_object() {
                Self::ensure_writable_fields(&masked, obj)?;
            }
        }
        if rows.len() > MAX_BULK_ROWS {
            return Err(DataError::Invalid(format!(
                "1 回に取り込める行は {MAX_BULK_ROWS} 行までです"
            )));
        }
        let keys = table.keys;
        if let Some(existing) = self
            .existing_import(&ctx.tenant_id, keys.tenant_int, import_id)
            .await?
        {
            return if existing == keys.table_int {
                Ok(import_id)
            } else {
                Err(DataError::Conflict(
                    "取込 ID が別のテーブルで使われています".into(),
                ))
            };
        }
        let mut staged = CopyRows::default();
        let mut tx = self
            .pools
            .for_tenant(&ctx.tenant_id)
            .begin()
            .await
            .map_err(map_db)?;
        let kind = serde_json::to_value(ctx.principal.kind)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .ok_or_else(|| DataError::Internal("主体の種別を直列化できません".into()))?;
        let total = i64::try_from(rows.len()).unwrap_or(i64::MAX);
        let inserted = sqlx::query(
            "INSERT INTO data_bulk_import (tenant_int, id, table_int, table_id, principal_kind, \
               principal_id, org, trace_id, schema_version, abort_on_invalid, index_mode, rows_total) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        )
        .bind(keys.tenant_int)
        .bind(import_id)
        .bind(keys.table_int)
        .bind(table_id)
        .bind(&kind)
        .bind(&ctx.principal.id)
        .bind(&ctx.org)
        .bind(trace_id)
        .bind(table.schema_version)
        .bind(opts.abort_on_invalid)
        .bind(opts.index_mode.as_str())
        .bind(total)
        .execute(&mut *tx)
        .await;
        match inserted {
            Ok(_) => {}
            // 同じ取込 ID を並行に始めた（先に入った方が取り込む・こちらは同じ ID を返す）。
            Err(sqlx::Error::Database(db)) if db.is_unique_violation() => {
                drop(tx);
                return match self
                    .existing_import(&ctx.tenant_id, keys.tenant_int, import_id)
                    .await?
                {
                    Some(t) if t == keys.table_int => Ok(import_id),
                    _ => Err(DataError::Conflict(
                        "取込 ID が別のテーブルで使われています".into(),
                    )),
                };
            }
            Err(e) => return Err(map_db(e)),
        }
        {
            // ① COPY（authoritative な表には触れない）。値は COPY のデータ行で送り、SQL 文は固定。
            let mut copy = tx
                .copy_in_raw(
                    "COPY data_bulk_staging \
                       (tenant_int, import_id, row_no, row_key, record_id, payload, op, expected_rev) \
                     FROM STDIN",
                )
                .await
                .map_err(map_db)?;
            for (i, row) in rows.iter().enumerate() {
                staged.push_row(keys.tenant_int, import_id, i + 1, row)?;
                if staged.buf.len() >= COPY_FLUSH_BYTES {
                    copy.send(std::mem::take(&mut staged.buf).into_bytes())
                        .await
                        .map_err(map_db)?;
                }
            }
            if !staged.buf.is_empty() {
                copy.send(std::mem::take(&mut staged.buf).into_bytes())
                    .await
                    .map_err(map_db)?;
            }
            copy.finish().await.map_err(map_db)?;
        }
        enqueue_import(&mut tx, &ctx.tenant_id, table_id, import_id, trace_id).await?;
        self.record_audit_on(
            &mut tx,
            ctx,
            AuditAction::DataRecordBulkImport,
            &table_id.to_string(),
            trace_id,
            json!({
                "import_id": import_id,
                "rows": total,
                "updates": staged.updates,
                "index_mode": opts.index_mode.as_str(),
            }),
        )
        .await?;
        tx.commit().await.map_err(map_db)?;
        Ok(import_id)
    }

    /// CSV の一括取込を始める（editor・見出しの照合と形の変換だけをここで行い、検証と適用はジョブ）。
    ///
    /// editor を先に確かめる（権限の無い主体に列の照合の結果＝スキーマの形を返さない）。変換の規則は
    /// [`csv`] を参照。取込 ID を返す。
    pub async fn start_bulk_import_csv(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        bytes: &[u8],
        opts: BulkImportOptions,
        trace_id: Option<&str>,
    ) -> Result<Uuid, DataError> {
        self.require(
            ctx,
            table_id,
            Relation::Editor,
            AuditAction::DataRecordBulkImport,
            trace_id,
        )
        .await?;
        let table = self.fetch_live(ctx, table_id).await?;
        let rows = csv::csv_rows(&table.schema, bytes)?;
        self.start_bulk_import(ctx, table_id, rows, opts, trace_id)
            .await
    }

    /// 取込 ID の取込が既にあればそのテーブルの内部キー（取込 ID の冪等な再開用）。
    async fn existing_import(
        &self,
        tenant_id: &str,
        tenant_int: i32,
        import_id: Uuid,
    ) -> Result<Option<i64>, DataError> {
        sqlx::query_scalar(
            "SELECT table_int FROM data_bulk_import WHERE tenant_int = $1 AND id = $2",
        )
        .bind(tenant_int)
        .bind(import_id)
        .fetch_optional(self.pools.for_tenant(tenant_id))
        .await
        .map_err(map_db)
    }

    /// 取込の進捗（editor かつ取込の作成者・テーブル owner・組織管理者）。別のテーブルの取込 ID・
    /// 見せない取込は「見つからない」。
    pub async fn bulk_import_status(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        import_id: Uuid,
    ) -> Result<BulkImportStatus, DataError> {
        let keys = self.bulk_import_gate(ctx, table_id, import_id).await?;
        #[allow(clippy::type_complexity)]
        let row: Option<(
            String,
            i64,
            i64,
            i64,
            i64,
            i64,
            bool,
            Option<DateTime<Utc>>,
            Option<i32>,
            Option<String>,
            DateTime<Utc>,
            Option<DateTime<Utc>>,
        )> = sqlx::query_as(
            "SELECT status, rows_total, rows_valid, rows_invalid, rows_applied, rows_rejected, \
                    (index_hold OR (index_mode = 'deferred' AND status = 'validating')), \
                    parity_checked_at, parity_mismatches, last_error, created_at, finished_at \
             FROM data_bulk_import WHERE tenant_int = $1 AND id = $2 AND table_int = $3",
        )
        .bind(keys.tenant_int)
        .bind(import_id)
        .bind(keys.table_int)
        .fetch_optional(self.pools.for_tenant(&ctx.tenant_id))
        .await
        .map_err(map_db)?;
        let (status, total, valid, invalid, applied, rejected, deferred, pc, pm, le, ca, fa) =
            row.ok_or(DataError::NotFound)?;
        Ok(BulkImportStatus {
            id: import_id,
            table_id,
            status,
            rows_total: total,
            rows_valid: valid,
            rows_invalid: invalid,
            rows_applied: applied,
            rows_rejected: rejected,
            index_deferred: deferred,
            parity_checked_at: pc,
            parity_mismatches: pm,
            last_error: le,
            created_at: ca,
            finished_at: fa,
        })
    }

    /// 不合格・拒否された行（行番号の昇順・keyset）。
    pub async fn bulk_import_errors(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        import_id: Uuid,
        after_row: i64,
        limit: i64,
    ) -> Result<Vec<BulkRowError>, DataError> {
        let keys = self.bulk_import_gate(ctx, table_id, import_id).await?;
        let mut conn = self
            .pools
            .for_tenant(&ctx.tenant_id)
            .acquire()
            .await
            .map_err(map_db)?;
        let rows: Vec<(i64, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT row_no, row_key, error FROM data_bulk_staging \
             WHERE tenant_int = $1 AND import_id = $2 AND row_no > $3 AND state IN ($4, $5) \
             ORDER BY row_no LIMIT $6",
        )
        .bind(keys.tenant_int)
        .bind(import_id)
        .bind(after_row)
        .bind(ST_INVALID)
        .bind(ST_REJECTED)
        .bind(limit.clamp(1, 1000))
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db)?;
        Ok(rows
            .into_iter()
            .map(|(row_no, key, error)| BulkRowError {
                row_no,
                key,
                message: error.unwrap_or_default(),
            })
            .collect())
    }

    /// 観測 API の入口（editor・生存テーブル・**取込の作成者かテーブル owner か組織管理者**）。
    ///
    /// 取込の状態と行エラー（行の値の断片を含みうる）は、同じテーブルの editor でも他人の取込なら
    /// 見せない（B-P3-4）。組織管理者もテーブルの editor であることが前提（アンビエント権限の禁止）。
    /// 見せない取込・別のテーブルの取込は「見つからない」（存在を区別しない）。
    async fn bulk_import_gate(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        import_id: Uuid,
    ) -> Result<TableKeys, DataError> {
        let az = self.authz_memo();
        self.require_in(
            &az,
            ctx,
            table_id,
            Relation::Editor,
            AuditAction::DataRecordBulkImportStatus,
            None,
        )
        .await?;
        let keys = self.fetch_live(ctx, table_id).await?.keys;
        let mut conn = self
            .pools
            .for_tenant(&ctx.tenant_id)
            .acquire()
            .await
            .map_err(map_db)?;
        let imp = load_import(&mut conn, keys.tenant_int, import_id, false)
            .await?
            .filter(|i| i.table_int == keys.table_int)
            .ok_or(DataError::NotFound)?;
        drop(conn);
        let creator = imp.auth_context(&ctx.tenant_id)?;
        if creator.principal.kind == ctx.principal.kind && creator.principal.id == ctx.principal.id
        {
            return Ok(keys);
        }
        let ns = ctx.ns();
        if az
            .check(ctx, Relation::Owner, &ns.data_table(&table_id.to_string()))
            .await?
            || az
                .check(ctx, Relation::Admin, &ns.organization(&ctx.org))
                .await?
        {
            return Ok(keys);
        }
        Err(DataError::NotFound)
    }

    /// 取込を 1 ステップ（現在の段の 1 チャンク・最大 `max_rows` 行）進める。冪等・中断再開可。
    ///
    /// 恒久的な失敗（テーブルの削除・editor の剥奪）は取込を `failed` にして `Ok` を返す。一時的な失敗
    /// （DB 等）は `Err` を返し、チャンクはロールバック済み（次回は同じ位置から）。
    pub async fn bulk_import_step(
        &self,
        tenant_id: &str,
        table_id: Uuid,
        import_id: Uuid,
        max_rows: i64,
    ) -> Result<BulkImportStep, DataError> {
        let Some(keys) = self.table_keys_of(tenant_id, table_id).await? else {
            return Err(DataError::NotFound);
        };
        let mut conn = self
            .pools
            .for_tenant(tenant_id)
            .acquire()
            .await
            .map_err(map_db)?;
        let imp = load_import(&mut conn, keys.tenant_int, import_id, false)
            .await?
            .filter(|i| i.table_int == keys.table_int)
            .ok_or(DataError::NotFound)?;
        drop(conn);
        let max = max_rows.max(1);
        let result = match imp.status.as_str() {
            "validating" => self.validate_chunk(tenant_id, &imp, max).await,
            "applying" => self.apply_chunk(tenant_id, &imp, max).await,
            "indexing" => self.index_chunk(tenant_id, &imp).await,
            _ => Ok(0),
        };
        let processed = match result {
            Ok(n) => n,
            Err(e @ (DataError::Forbidden | DataError::NotFound)) => {
                // 恒久的（主体の editor 剥奪・テーブルの削除）: 取込を止める（適用済みの行は残る）。
                self.fail_import(tenant_id, &imp, &e).await?;
                0
            }
            Err(e) => return Err(e),
        };
        let mut conn = self
            .pools
            .for_tenant(tenant_id)
            .acquire()
            .await
            .map_err(map_db)?;
        let status = load_import(&mut conn, keys.tenant_int, import_id, false)
            .await?
            .map_or_else(|| "failed".to_string(), |i| i.status);
        let done = matches!(status.as_str(), "done" | "failed");
        Ok(BulkImportStep {
            status,
            processed,
            done,
        })
    }

    /// 取込を最後まで進める（同期実行・テスト／小さい取込向け）。
    pub async fn run_bulk_import(
        &self,
        tenant_id: &str,
        table_id: Uuid,
        import_id: Uuid,
    ) -> Result<BulkImportStep, DataError> {
        loop {
            let step = self
                .bulk_import_step(tenant_id, table_id, import_id, BULK_CHUNK)
                .await?;
            if step.done {
                return Ok(step);
            }
        }
    }

    /// 恒久的な失敗で取込を止める。値索引を保持していれば `indexing` へ進めて解放とバックフィルを
    /// 必ず行う（保持したまま終わると当該列が永久に「準備中」になる）。
    async fn fail_import(
        &self,
        tenant_id: &str,
        imp: &ImportRow,
        e: &DataError,
    ) -> Result<(), DataError> {
        let mut tx = self
            .pools
            .for_tenant(tenant_id)
            .begin()
            .await
            .map_err(map_db)?;
        let closed: Option<String> = sqlx::query_scalar(
            "UPDATE data_bulk_import SET last_error = $3, updated_at = now(), \
               status = CASE WHEN index_hold THEN 'indexing' ELSE 'failed' END, \
               finished_at = CASE WHEN index_hold THEN NULL ELSE now() END \
             WHERE tenant_int = $1 AND id = $2 AND status NOT IN ('done', 'failed') \
             RETURNING status",
        )
        .bind(imp.tenant_int)
        .bind(imp.id)
        .bind(e.to_string())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db)?;
        if closed.as_deref() == Some("failed") {
            // 作業領域を片付ける（保持していれば索引の仕上げの最後に片付く）。
            apply::cleanup_staging(&mut tx, imp).await?;
        }
        tx.commit().await.map_err(map_db)?;
        tracing::warn!(import_id = %imp.id, error = %e, "一括取込を中止しました");
        Ok(())
    }
}

//! ② ステージング上の正規検証（data-platform §16.7）。
//!
//! 1 件の create と**同じ検証器**を通す:
//!
//! | 検査 | 実装 |
//! |---|---|
//! | 認可（書込可否） | テーブルの editor をチャンクごとに OpenFGA へ問い直す（[`DataStore::require_in`]） |
//! | 型・required・選択肢・多値要素数上限・派生列 / 対称列への書込拒否 | [`validate_record_data`]（create と同一関数） |
//! | 参照先レコードの存在と**書き手からの可視性**（PIT-20） | [`DataStore::check_record_refs_many`]（create と同一関数・リンク列ごとに 1 回） |
//! | unique | 既存の `data_unique` と、取込内の他の行（`data_bulk_key` の予約・チャンクを跨ぐ） |
//! | リンクのカーディナリティ | 単一値の側の要素数は検証器、参照先側（一対一・一対多）は既存の逆方向行と取込内の予約 |
//!
//! unique とカーディナリティの最終的な強制は適用時の DB（部分一意・`data_unique`）で、ここは
//! 取込の行ごとにエラーを返すための前倒しの検査。検証から適用までの間の並行書込との競合は
//! 適用時に行単位で拒否する（[`super::apply`]）。

use std::collections::{BTreeSet, HashSet};
use storage::audit::AuditAction;

use authz::Relation;
use serde_json::{Map, Value};
use sqlx::types::Json;
use sqlx::PgConnection;
use uuid::Uuid;

use super::resolver::ChunkResolver;
use super::{
    load_import, BulkIndexMode, ImportRow, LAZY_INDEX_MIN_ROWS, OP_UPDATE, ST_INVALID, ST_STAGED,
    ST_VALID,
};
use crate::index::normalize::{field_keys, FieldKeys};
use crate::index::state::field_id_i16;
use crate::index::IndexTable;
use crate::model::{DataTable, FieldType, TableSchema};
use crate::store::DataStore;
use crate::validate::validate_record_data;
use crate::{map_db, DataError};

/// 取込内で 1 行だけが持てる値（unique の正規化値・一対一 / 一対多リンクの参照先）。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct Claim {
    pub field: i16,
    pub key: String,
    /// `Some(参照先テーブル)` ならリンクの参照先（`key` は参照先レコードの外部 ID）。
    pub link: Option<LinkClaim>,
    /// エラー文言に出すフィールドの参照名。
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct LinkClaim {
    pub ref_table: Uuid,
    /// 対称列（逆方向行の field_id）。
    pub symmetric: i16,
}

impl Claim {
    fn message(&self) -> String {
        if self.link.is_some() {
            format!(
                "'{}' のリンク先は既に他のレコードから参照されています（一対一・一対多のカーディナリティ）",
                self.label
            )
        } else {
            format!(
                "'{}' の値が既存のレコードまたは取込内の別の行と重複しています（unique）",
                self.label
            )
        }
    }

    fn pair(&self) -> (i16, String) {
        (self.field, self.key.clone())
    }
}

/// 検証済みの 1 行が予約する値。
pub(super) fn claims_of(
    schema: &TableSchema,
    data: &Map<String, Value>,
    max_multi_values: usize,
) -> Result<Vec<Claim>, DataError> {
    let mut out = Vec::new();
    for f in &schema.fields {
        let Some(v) = data.get(&f.key).filter(|v| !v.is_null()) else {
            continue;
        };
        if f.unique && !f.field_type.is_derived() {
            if let Some(FieldKeys::Unique(norm)) =
                field_keys(f, IndexTable::Unique, v, max_multi_values)?
            {
                out.push(Claim {
                    field: field_id_i16(f.id)?,
                    key: norm,
                    link: None,
                    label: f.key.clone(),
                });
            }
        }
        if f.field_type != FieldType::RecordRef || f.is_reverse_link() {
            continue;
        }
        // 参照先側が単一（一対一・一対多）のリンクだけ。対称列（逆方向行）で DB が強制する。
        let (Some(link), Some(ref_table)) = (&f.link, f.ref_table) else {
            continue;
        };
        let Some(sym) = link
            .symmetric_field
            .filter(|_| link.cardinality.dst_single())
        else {
            continue;
        };
        for id in v.as_array().into_iter().flatten().filter_map(Value::as_str) {
            out.push(Claim {
                field: field_id_i16(f.id)?,
                key: id.to_string(),
                link: Some(LinkClaim {
                    ref_table,
                    symmetric: field_id_i16(sym)?,
                }),
                label: f.key.clone(),
            });
        }
    }
    Ok(out)
}

/// 読めない列への書込を含む行の文言（列名・値を含めない）。
/// 存在しない列への書込と同じ文言（列の存在オラクルを作らない）。
pub(super) const MASKED_WRITE_MESSAGE: &str = crate::FIELD_UNAVAILABLE_MESSAGE;

/// 検証エラーを行のエラー文言へ（入力の誤りだけを行に付け、内部エラーは呼び出し側が伝播する）。
pub(super) fn row_message(e: DataError) -> Result<String, DataError> {
    match e {
        DataError::Invalid(m) => Ok(m),
        DataError::InvalidInput(e) => Ok(e.message().to_string()),
        e if e.is_conflict() => Ok(e.conflict_detail().unwrap_or_default()),
        other => Err(other),
    }
}

impl DataStore {
    /// 検証を 1 チャンク進める（最後のチャンクで `applying` へ）。検証した行数を返す。
    #[allow(clippy::too_many_lines)] // 検証器 → 参照先 → 予約 → 書戻しを 1 チャンクで順に行う。
    pub(super) async fn validate_chunk(
        &self,
        tenant_id: &str,
        imp: &ImportRow,
        max: i64,
    ) -> Result<u64, DataError> {
        let ctx = imp.auth_context(tenant_id)?;
        let az = self.authz_memo();
        self.require_in(
            &az,
            &ctx,
            imp.table_id,
            Relation::Editor,
            AuditAction::DataRecordBulkImport,
            imp.trace_id.as_deref(),
        )
        .await?;
        let table = self.fetch_live(&ctx, imp.table_id).await?;
        if table.schema_version != imp.schema_version {
            self.restart_validation(tenant_id, imp, table.schema_version)
                .await?;
            return Ok(0);
        }
        let staged: Vec<(i64, Json<Value>, i16)> = sqlx::query_as(
            "SELECT row_no, payload, op FROM data_bulk_staging \
             WHERE tenant_int = $1 AND import_id = $2 AND row_no > $3 \
             ORDER BY row_no LIMIT $4",
        )
        .bind(imp.tenant_int)
        .bind(imp.id)
        .bind(imp.validate_cursor)
        .bind(max)
        .fetch_all(self.pools.for_tenant(tenant_id))
        .await
        .map_err(map_db)?;
        // 更新行（upsert・Task 15.7）は merge patch を持つだけで、検証は適用段で 1 件の更新と同じ入口を
        // 行ロックの下で通す（現在値とのマージ後でないと required・参照先・unique を判定できない）。
        let is_update: Vec<bool> = staged.iter().map(|r| r.2 == OP_UPDATE).collect();
        let rows: Vec<(i64, Json<Value>)> = staged.into_iter().map(|(n, p, _)| (n, p)).collect();

        // 型・required・多値要素数上限（create と同じ検証器。DB の Tx の外で行う）。
        let resolver = ChunkResolver::new(self.resolver.as_ref());
        // 読めない（マスクされた）列への書込は行ごとに拒否（create と同じ単一点・PIT-19）。
        // 文言に列名・値を含めない。
        let audit = crate::mask::WriteAudit::new(
            AuditAction::DataRecordBulkImport,
            imp.trace_id.as_deref(),
        );
        let masked = self
            .ensure_fields_writable(&ctx, &az, &table, None, audit)
            .await?;
        let mut denied = false;
        // FSM 管理テーブルの作成行の状態は初期状態に固定する（1 件の作成と同じ・検証 2-C-P2-3）。
        let fsm = self
            .fsm_initial(&ctx, &table, imp.trace_id.as_deref())
            .await?;
        let mut outcomes: Vec<Result<Map<String, Value>, String>> = Vec::with_capacity(rows.len());
        for (i, (_, payload)) in rows.iter().enumerate() {
            if is_update[i] {
                outcomes.push(
                    payload.0.as_object().cloned().ok_or_else(|| {
                        "patch は JSON オブジェクトである必要があります".to_string()
                    }),
                );
                continue;
            }
            if let Some(obj) = payload.0.as_object() {
                if Self::ensure_writable_fields(&masked, &table.schema, obj).is_err() {
                    denied = true;
                    outcomes.push(Err(MASKED_WRITE_MESSAGE.to_string()));
                    continue;
                }
            }
            let mut payload = payload.0.clone();
            if let (Some(fi), Value::Object(m)) = (&fsm, &mut payload) {
                if let Err(e) = fi.apply(m) {
                    outcomes.push(Err(row_message(e)?));
                    continue;
                }
            }
            let named = |k: &str| payload.as_object().is_some_and(|o| o.contains_key(k));
            let r = validate_record_data(
                &ctx,
                &table.schema,
                &payload,
                &resolver,
                self.max_multi_values,
            )
            .await
            .map_err(|e| crate::scrub_input_field(e, |k| named(k) || !masked.contains(k)));
            outcomes.push(match r {
                Ok(m) => Ok(m),
                Err(e) => Err(row_message(e)?),
            });
        }
        if denied {
            // 読めない列・存在しない列への書込はチャンクごとに 1 件の deny で残す（検証 2-B-P3-4）。
            self.audit_field_deny(&ctx, table.id, audit).await;
        }
        // 参照先の存在と書き手からの可視性（create と同じ関数・リンク列ごとに 1 回で引く）。
        let ok_idx: Vec<usize> = (0..outcomes.len())
            .filter(|i| outcomes[*i].is_ok() && !is_update[*i])
            .collect();
        let maps: Vec<&Map<String, Value>> = ok_idx
            .iter()
            .filter_map(|i| outcomes[*i].as_ref().ok())
            .collect();
        let ref_errors = self
            .check_record_refs_many(&ctx, &az, &table.schema, &maps)
            .await?;
        for (i, e) in ok_idx.into_iter().zip(ref_errors) {
            if let Some(e) = e {
                outcomes[i] = Err(row_message(e)?);
            }
        }
        let claims: Vec<Vec<Claim>> = outcomes
            .iter()
            .zip(&is_update)
            .map(|(o, upd)| match o {
                Ok(m) if !*upd => claims_of(&table.schema, m, self.max_multi_values),
                _ => Ok(Vec::new()),
            })
            .collect::<Result<_, _>>()?;

        let mut tx = self
            .pools
            .for_tenant(tenant_id)
            .begin()
            .await
            .map_err(map_db)?;
        let cur = load_import(&mut tx, imp.tenant_int, imp.id, true)
            .await?
            .ok_or(DataError::NotFound)?;
        if cur.status != "validating"
            || cur.validate_cursor != imp.validate_cursor
            || cur.schema_version != imp.schema_version
        {
            // 別のワーカーが先に進めた（再配信の重複）。何もしない。
            tx.rollback().await.map_err(map_db)?;
            return Ok(0);
        }
        // unique・参照先の一意性: 既存データと取込内の予約（チャンクを跨ぐ）と、このチャンク内の先の行。
        let taken = taken_claims(&mut tx, tenant_id, &table, imp, &claims).await?;
        let mut seen: HashSet<(i16, String)> = HashSet::new();
        for (i, cl) in claims.iter().enumerate() {
            if outcomes[i].is_err() {
                continue;
            }
            if let Some(c) = cl
                .iter()
                .find(|c| taken.contains(&c.pair()) || seen.contains(&c.pair()))
            {
                outcomes[i] = Err(c.message());
                continue;
            }
            seen.extend(cl.iter().map(Claim::pair));
        }
        reserve_claims(&mut tx, imp, &rows, &outcomes, &claims).await?;
        let (valid, invalid) = write_outcomes(&mut tx, imp, &rows, &outcomes).await?;
        let last = rows.last().map_or(cur.validate_cursor, |r| r.0);
        sqlx::query(
            "UPDATE data_bulk_import SET validate_cursor = $3, rows_valid = rows_valid + $4, \
               rows_invalid = rows_invalid + $5, updated_at = now() \
             WHERE tenant_int = $1 AND id = $2",
        )
        .bind(imp.tenant_int)
        .bind(imp.id)
        .bind(last)
        .bind(valid)
        .bind(invalid)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        if i64::try_from(rows.len()).unwrap_or(i64::MAX) < max {
            let mut done = cur.clone();
            done.rows_valid += valid;
            done.rows_invalid += invalid;
            self.finish_validation(&mut tx, tenant_id, &done, &table)
                .await?;
        }
        tx.commit().await.map_err(map_db)?;
        Ok(rows.len() as u64)
    }

    /// 検証の完了: 全件か無かの指定で不合格があれば中止、そうでなければ `applying` へ
    /// （索引を遅延構築するならここで値索引を `building` に保持する）。
    async fn finish_validation(
        &self,
        tx: &mut PgConnection,
        tenant_id: &str,
        imp: &ImportRow,
        table: &DataTable,
    ) -> Result<(), DataError> {
        if imp.abort_on_invalid && imp.rows_invalid > 0 && imp.apply_cursor == 0 {
            sqlx::query(
                "UPDATE data_bulk_import SET status = 'failed', finished_at = now(), \
                   last_error = $3, updated_at = now() \
                 WHERE tenant_int = $1 AND id = $2",
            )
            .bind(imp.tenant_int)
            .bind(imp.id)
            .bind(format!(
                "検証で不合格の行が {} 行あるため、何も適用しませんでした",
                imp.rows_invalid
            ))
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
            return super::apply::cleanup_staging(tx, imp).await;
        }
        sqlx::query(
            "UPDATE data_bulk_import SET status = 'applying', updated_at = now() \
             WHERE tenant_int = $1 AND id = $2",
        )
        .bind(imp.tenant_int)
        .bind(imp.id)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        if !imp.index_hold && should_defer(tx, imp).await? {
            let held = super::apply::hold_index(tx, tenant_id, table, imp).await?;
            if !held {
                tracing::info!(import_id = %imp.id, "別の取込が索引を保持中のため、同期で索引を書きます");
            }
        }
        Ok(())
    }

    /// スキーマが改訂された: 未適用の行を新しいスキーマで検証し直す（適用済みの行はそのまま）。
    pub(super) async fn restart_validation(
        &self,
        tenant_id: &str,
        imp: &ImportRow,
        schema_version: i64,
    ) -> Result<(), DataError> {
        let mut tx = self
            .pools
            .for_tenant(tenant_id)
            .begin()
            .await
            .map_err(map_db)?;
        let cur = load_import(&mut tx, imp.tenant_int, imp.id, true)
            .await?
            .ok_or(DataError::NotFound)?;
        if cur.schema_version == schema_version
            || !matches!(cur.status.as_str(), "validating" | "applying")
        {
            tx.rollback().await.map_err(map_db)?;
            return Ok(());
        }
        sqlx::query(
            "UPDATE data_bulk_staging SET state = $4, normalized = NULL, error = NULL \
             WHERE tenant_int = $1 AND import_id = $2 AND row_no > $3 AND state <> $4",
        )
        .bind(cur.tenant_int)
        .bind(cur.id)
        .bind(cur.apply_cursor)
        .bind(ST_STAGED)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        sqlx::query(
            "DELETE FROM data_bulk_key WHERE tenant_int = $1 AND import_id = $2 AND row_no > $3",
        )
        .bind(cur.tenant_int)
        .bind(cur.id)
        .bind(cur.apply_cursor)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        let (valid, invalid): (i64, i64) = sqlx::query_as(
            "SELECT count(*) FILTER (WHERE state IN (3, 4)), count(*) FILTER (WHERE state = 2) \
             FROM data_bulk_staging WHERE tenant_int = $1 AND import_id = $2 AND row_no <= $3",
        )
        .bind(cur.tenant_int)
        .bind(cur.id)
        .bind(cur.apply_cursor)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db)?;
        sqlx::query(
            "UPDATE data_bulk_import SET status = 'validating', schema_version = $3, \
               validate_cursor = apply_cursor, rows_valid = $4, rows_invalid = $5, updated_at = now() \
             WHERE tenant_int = $1 AND id = $2",
        )
        .bind(cur.tenant_int)
        .bind(cur.id)
        .bind(schema_version)
        .bind(valid)
        .bind(invalid)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        tx.commit().await.map_err(map_db)?;
        tracing::info!(import_id = %imp.id, "スキーマが改訂されたため、未適用の行を検証し直します");
        Ok(())
    }
}

/// 索引を遅延構築するか（`auto` は 2 万行以上かつ既存行数以上＝取込が表の大半を占めるとき）。
async fn should_defer(conn: &mut PgConnection, imp: &ImportRow) -> Result<bool, DataError> {
    Ok(match imp.index_mode() {
        BulkIndexMode::Immediate => false,
        BulkIndexMode::Deferred => true,
        BulkIndexMode::Auto => {
            if imp.rows_valid < LAZY_INDEX_MIN_ROWS {
                return Ok(false);
            }
            // 既存行数は取込の行数で打ち切って数える（大きな表を全件数えない）。
            let existing: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM (SELECT 1 FROM data_record \
                 WHERE tenant_int = $1 AND table_int = $2 LIMIT $3) s",
            )
            .bind(imp.tenant_int)
            .bind(imp.table_int)
            .bind(imp.rows_valid)
            .fetch_one(&mut *conn)
            .await
            .map_err(map_db)?;
            existing < imp.rows_valid
        }
    })
}

/// 既に使われている予約（取込内の予約・既存の unique 値・既存のリンクの参照先）。
async fn taken_claims(
    conn: &mut PgConnection,
    tenant_id: &str,
    table: &DataTable,
    imp: &ImportRow,
    claims: &[Vec<Claim>],
) -> Result<HashSet<(i16, String)>, DataError> {
    let all: BTreeSet<&Claim> = claims.iter().flatten().collect();
    let mut taken = HashSet::new();
    if all.is_empty() {
        return Ok(taken);
    }
    let fields: Vec<i16> = all.iter().map(|c| c.field).collect();
    let keys: Vec<&str> = all.iter().map(|c| c.key.as_str()).collect();
    let reserved: Vec<(i16, String)> = sqlx::query_as(
        "SELECT field_id, key FROM data_bulk_key WHERE tenant_int = $1 AND import_id = $2 \
           AND (field_id, key) IN (SELECT * FROM unnest($3::int2[], $4::text[]))",
    )
    .bind(imp.tenant_int)
    .bind(imp.id)
    .bind(&fields)
    .bind(&keys)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db)?;
    taken.extend(reserved);
    let (uf, uk): (Vec<i16>, Vec<&str>) = all
        .iter()
        .filter(|c| c.link.is_none())
        .map(|c| (c.field, c.key.as_str()))
        .unzip();
    if !uf.is_empty() {
        let existing: Vec<(i16, String)> = sqlx::query_as(
            "SELECT field_id, val_norm FROM data_unique WHERE tenant_int = $1 AND table_int = $2 \
               AND (field_id, val_norm) IN (SELECT * FROM unnest($3::int2[], $4::text[]))",
        )
        .bind(table.keys.tenant_int)
        .bind(table.keys.table_int)
        .bind(&uf)
        .bind(&uk)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db)?;
        taken.extend(existing);
    }
    // リンクの参照先: 参照先テーブルの逆方向行（主キーの前方一致）に既に行があれば使用済み。
    let mut by_link: std::collections::BTreeMap<(i16, &LinkClaim), Vec<Uuid>> =
        std::collections::BTreeMap::new();
    for c in all.iter().filter(|c| c.link.is_some()) {
        if let (Some(l), Ok(id)) = (&c.link, Uuid::parse_str(&c.key)) {
            by_link.entry((c.field, l)).or_default().push(id);
        }
    }
    for ((field, link), ids) in by_link {
        let ref_int: Option<i64> =
            sqlx::query_scalar("SELECT table_int FROM data_table WHERE tenant_id = $1 AND id = $2")
                .bind(tenant_id)
                .bind(link.ref_table)
                .fetch_optional(&mut *conn)
                .await
                .map_err(map_db)?;
        let Some(ref_int) = ref_int else { continue };
        let targets: Vec<(Uuid, i64)> = sqlx::query_as(
            "SELECT id, record_int FROM data_record \
             WHERE tenant_int = $1 AND table_int = $2 AND id = ANY($3::uuid[])",
        )
        .bind(table.keys.tenant_int)
        .bind(ref_int)
        .bind(&ids)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db)?;
        let ints: Vec<i64> = targets.iter().map(|t| t.1).collect();
        let used: HashSet<i64> = sqlx::query_scalar::<_, i64>(
            "SELECT DISTINCT src_record FROM data_link \
             WHERE tenant_int = $1 AND table_int = $2 AND field_id = $3 \
               AND src_record = ANY($4::int8[])",
        )
        .bind(table.keys.tenant_int)
        .bind(ref_int)
        .bind(link.symmetric)
        .bind(&ints)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db)?
        .into_iter()
        .collect();
        for (id, ri) in targets {
            if used.contains(&ri) {
                taken.insert((field, id.to_string()));
            }
        }
    }
    Ok(taken)
}

/// 合格した行の予約を書く（取込内で後から同じ値を持つ行を不合格にする）。
async fn reserve_claims(
    conn: &mut PgConnection,
    imp: &ImportRow,
    rows: &[(i64, Json<Value>)],
    outcomes: &[Result<Map<String, Value>, String>],
    claims: &[Vec<Claim>],
) -> Result<(), DataError> {
    let mut row_nos = Vec::new();
    let mut fields = Vec::new();
    let mut keys = Vec::new();
    for ((row, outcome), cl) in rows.iter().zip(outcomes).zip(claims) {
        if outcome.is_err() {
            continue;
        }
        for c in cl {
            row_nos.push(row.0);
            fields.push(c.field);
            keys.push(c.key.as_str());
        }
    }
    if row_nos.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO data_bulk_key (tenant_int, import_id, field_id, key, row_no) \
         SELECT $1, $2, u.f, u.k, u.r FROM unnest($3::int2[], $4::text[], $5::int8[]) AS u(f, k, r) \
         ON CONFLICT DO NOTHING",
    )
    .bind(imp.tenant_int)
    .bind(imp.id)
    .bind(&fields)
    .bind(&keys)
    .bind(&row_nos)
    .execute(&mut *conn)
    .await
    .map_err(map_db)?;
    Ok(())
}

/// 検証結果をステージング行へ書く。`(合格数, 不合格数)`。
async fn write_outcomes(
    conn: &mut PgConnection,
    imp: &ImportRow,
    rows: &[(i64, Json<Value>)],
    outcomes: &[Result<Map<String, Value>, String>],
) -> Result<(i64, i64), DataError> {
    let mut row_nos = Vec::with_capacity(rows.len());
    let mut states = Vec::with_capacity(rows.len());
    let mut normalized: Vec<Option<String>> = Vec::with_capacity(rows.len());
    let mut errors: Vec<Option<&str>> = Vec::with_capacity(rows.len());
    let (mut valid, mut invalid) = (0i64, 0i64);
    for (row, outcome) in rows.iter().zip(outcomes) {
        row_nos.push(row.0);
        match outcome {
            Ok(m) => {
                valid += 1;
                states.push(ST_VALID);
                normalized.push(Some(Value::Object(m.clone()).to_string()));
                errors.push(None);
            }
            Err(msg) => {
                invalid += 1;
                states.push(ST_INVALID);
                normalized.push(None);
                errors.push(Some(msg.as_str()));
            }
        }
    }
    if row_nos.is_empty() {
        return Ok((0, 0));
    }
    sqlx::query(
        "UPDATE data_bulk_staging s SET state = u.st, normalized = u.n::jsonb, error = u.e \
         FROM unnest($3::int8[], $4::int2[], $5::text[], $6::text[]) AS u(row_no, st, n, e) \
         WHERE s.tenant_int = $1 AND s.import_id = $2 AND s.row_no = u.row_no",
    )
    .bind(imp.tenant_int)
    .bind(imp.id)
    .bind(&row_nos)
    .bind(&states)
    .bind(&normalized)
    .bind(&errors)
    .execute(&mut *conn)
    .await
    .map_err(map_db)?;
    Ok((valid, invalid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::derived::types::{LinkCardinality, LinkDef};
    use crate::model::FieldDef;
    use serde_json::json;

    fn f(id: u16, key: &str, ty: FieldType) -> FieldDef {
        let mut d = FieldDef::new(key, ty);
        d.id = id;
        d
    }

    #[test]
    fn claims_cover_unique_values_and_single_target_links_only() {
        let mut code = f(16, "code", FieldType::Text);
        code.unique = true;
        let memo = f(17, "memo", FieldType::Text);
        let target = Uuid::new_v4();
        let mut one = f(18, "one", FieldType::RecordRef);
        one.ref_table = Some(target);
        one.link = Some(LinkDef {
            cardinality: LinkCardinality::OneToOne,
            symmetric_field: Some(30),
            symmetric_key: None,
            reverse: false,
        });
        let mut many = f(19, "many", FieldType::RecordRef);
        many.ref_table = Some(target);
        many.link = Some(LinkDef {
            cardinality: LinkCardinality::ManyToMany,
            symmetric_field: Some(31),
            symmetric_key: None,
            reverse: false,
        });
        let schema = TableSchema {
            fields: vec![code, memo, one, many],
            status_field: None,
            row_policy: None,
            field_policy: vec![],
            aggregate_min_rows: None,
            fsm_ref: None,
            rag: None,
        };
        let a = Uuid::new_v4().to_string();
        let b = Uuid::new_v4().to_string();
        let data = json!({"code": "X-1", "memo": "m", "one": [a], "many": [b]});
        let got = claims_of(&schema, data.as_object().unwrap(), 50).unwrap();
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0].field, 16);
        assert_eq!(got[0].key, "X-1");
        assert!(got[0].link.is_none());
        assert!(got[0].message().contains("unique"));
        assert_eq!(got[1].field, 18);
        assert_eq!(got[1].key, a);
        assert_eq!(got[1].link.as_ref().unwrap().symmetric, 30);
        assert!(got[1].message().contains("カーディナリティ"));
    }

    #[test]
    fn only_input_errors_become_row_messages() {
        assert_eq!(
            row_message(DataError::Invalid("x".into())).unwrap(),
            "x".to_string()
        );
        assert!(row_message(DataError::Conflict("c".into())).is_ok());
        assert_eq!(
            row_message(DataError::UniqueViolation).unwrap(),
            "unique 制約に違反しています"
        );
        assert!(row_message(DataError::Internal("db".into())).is_err());
        assert!(row_message(DataError::Forbidden).is_err());
    }
}

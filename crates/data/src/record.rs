//! レコード CRUD（Task 9.2）＋楽観ロック（Task 9.5）。
//!
//! # 読取経路について（Task 9.3 への布石）
//!
//! 本モジュールの SELECT は現段階ではテーブル ReBAC（viewer/editor）のみで守られる。
//! Task 9.3 で行レベル述語エンジンが入ると、**全読取はクエリ実行チョークポイント
//! （query/executor）へ集約し、row_policy 述語が無条件 AND 合成される**。
//! 生 SQL は本 crate の外に公開しない（design §4.10・PIT-21）。

use authz::{AuthContext, Relation};
use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use sqlx::types::Json;
use storage::audit::AuditAction;
use uuid::Uuid;

use crate::field_key::{decode_record, encode_record};
use crate::index::state::{lock_plan, IndexPlan};
use crate::index::write::IndexWriter;
use crate::model::{DataRecord, FieldType, TableSchema};
use crate::policy::memo::AuthzMemo;
use crate::record_effects::CreatedRecord;
use crate::revision::{diff_fields, insert_revision, RevisionInsert};
use crate::store::DataStore;
use crate::stream::outbox::{emit_record_changes, Before, ChangeKind, RecordChange};
use crate::validate::validate_record_data;
use crate::{map_db, DataError};

/// data_record 行。
#[derive(Clone, sqlx::FromRow)]
pub(crate) struct RecordRow {
    pub(crate) id: Uuid,
    pub(crate) table_id: Uuid,
    pub(crate) data: Json<Value>,
    pub(crate) rev: i64,
    pub(crate) owner: String,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
}

/// 書込の RETURNING（索引行の書込に `record_int` が要る）。**`record_int` は crate 内に閉じる**
/// （DTO へは [`RecordRow`] だけを渡す・PIT-75）。
#[derive(sqlx::FromRow)]
pub(crate) struct WrittenRow {
    pub(crate) record_int: i64,
    #[sqlx(flatten)]
    pub(crate) row: RecordRow,
}

/// data_record の SELECT 列（[`RecordRow`] と同じ並び・エイリアス `r` 付き）。
///
/// **`record_int` / `tenant_int` / `table_int` は選ばない**（外部 DTO に載る経路へ内部キーを
/// 流さない・PIT-75）。
pub(crate) const RECORD_COLS: &str =
    "r.id, r.table_id, r.data, r.rev, r.owner, r.created_at, r.updated_at";
/// 書込の RETURNING（[`WrittenRow`]・crate 内専用）。
pub(crate) const WRITTEN_RETURNING: &str =
    "record_int, id, table_id, data, rev, owner, created_at, updated_at";

impl RecordRow {
    /// 保存形（`"f{id}"` キー）を外部形（`key`）へ戻して DTO にする。
    pub(crate) fn into_record(self, schema: &TableSchema) -> DataRecord {
        DataRecord {
            id: self.id,
            table_id: self.table_id,
            data: Value::Object(decode_record(schema, &self.data.0)),
            rev: self.rev,
            owner: self.owner,
            created_at: self.created_at,
            updated_at: self.updated_at,
            suppressed_fields: Vec::new(),
            link_labels: std::collections::BTreeMap::new(),
        }
    }
}

impl DataStore {
    /// 書込 Tx 上の索引ライタ（[`lock_plan`] の計画に従う）。
    pub(crate) fn index_writer<'a>(
        &self,
        plan: &'a IndexPlan,
        schema: &'a TableSchema,
        tenant_id: &'a str,
    ) -> IndexWriter<'a> {
        IndexWriter {
            plan,
            schema,
            tenant_id,
            max_multi_values: self.max_multi_values,
        }
    }

    /// レコードを作成する（editor・サーバ検証・rev=1・リビジョン同時記録）。
    pub async fn create_record(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        data: Value,
        trace_id: Option<&str>,
    ) -> Result<DataRecord, DataError> {
        let az = self.authz_memo();
        self.require_in(
            &az,
            ctx,
            table_id,
            Relation::Editor,
            AuditAction::DataRecordCreate,
            trace_id,
        )
        .await?;
        let table = self.fetch_live(ctx, table_id).await?;
        // 読めない（マスクされた）列への書込は作成でも 403（更新・取込と同じ単一点・PIT-19）。
        let masked = self
            .ensure_fields_writable(
                ctx,
                &az,
                &table,
                data.as_object(),
                crate::mask::WriteAudit::new(AuditAction::DataRecordCreate, trace_id),
            )
            .await?;
        // FSM 管理テーブルの作成時の状態は初期状態に固定する（省略は初期状態・違えば 400・検証 2-C-P2-3）。
        let mut data = data;
        if let Some(fi) = self.fsm_initial(ctx, &table, trace_id).await? {
            if let Value::Object(m) = &mut data {
                fi.apply(m)?;
            }
        }
        let named = |k: &str| data.as_object().is_some_and(|o| o.contains_key(k));
        let normalized = validate_record_data(
            ctx,
            &table.schema,
            &data,
            self.resolver.as_ref(),
            self.max_multi_values,
        )
        .await
        // 列名を返すのは書き手が名指しした列か読める列だけ（必須の欠落が読めない列の名前を漏らさない）。
        .map_err(|e| crate::scrub_input_field(e, |k| named(k) || !masked.contains(k)))?;
        // 以降の status 変更は遷移 API 経由（Task 9.10・更新は reject_fsm_status_write が拒否）。
        self.check_record_refs(ctx, &table.schema, &normalized)
            .await?;

        let mut tx = self
            .pools
            .for_tenant(&ctx.tenant_id)
            .begin()
            .await
            .map_err(map_db)?;
        // 索引の計画（テーブル行を FOR KEY SHARE・レコード行より先に取る）。
        let plan = lock_plan(
            &mut tx,
            &ctx.tenant_id,
            table_id,
            table.keys,
            table.schema_version,
        )
        .await?;
        // record_int は列 default（プール共通シーケンス）で採番し、外へは返さない。
        let written: WrittenRow = sqlx::query_as(&format!(
            "INSERT INTO data_record (tenant_id, table_id, org, data, owner, tenant_int, table_int) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             RETURNING {WRITTEN_RETURNING}"
        ))
        .bind(&ctx.tenant_id)
        .bind(table_id)
        .bind(&ctx.org)
        .bind(Json(Value::Object(encode_record(&table.schema, &normalized))))
        .bind(&ctx.principal.id)
        .bind(table.keys.tenant_int)
        .bind(table.keys.table_int)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_unique)?;
        // 索引行（全フィールド＋擬似列）・材料化・リビジョンを同一 Tx で書く（一括書込パスと同じ入口・
        // crate::record_effects）。
        let row = written.row;
        self.after_records_created(
            &mut tx,
            ctx,
            &table,
            &plan,
            &[CreatedRecord {
                record_int: written.record_int,
                id: row.id,
                owner: &row.owner,
                created_at: row.created_at,
                updated_at: row.updated_at,
                data: &normalized,
            }],
            |_, _| false,
            false,
            trace_id,
        )
        .await?;
        // 実データ変更の監査は書込と同一 Tx（原子的・コミット後の監査失敗で成功を偽らない）。
        self.record_audit_record_on(
            &mut tx,
            ctx,
            AuditAction::DataRecordCreate,
            row.id,
            trace_id,
            serde_json::json!({ "table_id": table_id }),
        )
        .await?;
        tx.commit().await.map_err(map_db)?;
        Ok(row.into_record(&table.schema))
    }

    /// レコードを取得する（viewer・行述語つき）。
    ///
    /// 行述語で不可視の行は存在しない行と**同一応答**（404・存在オラクルなし・PIT-21）。
    pub async fn get_record(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<DataRecord, DataError> {
        // テーブルの生存チェック込み（削除済みテーブルの残存 FGA タプルで読ませない）。
        // 認可の問いはリクエストのメモに束ねる（Task 15.9）。
        let az = self.authz_memo();
        let table = self
            .open_for_read(ctx, &az, table_id, AuditAction::DataRecordGet, trace_id)
            .await?;
        let row = self
            .select_visible_by_id(ctx, &az, &table, id)
            .await?
            .ok_or(DataError::NotFound)?;
        let mut records = vec![row.into_record(&table.schema)];
        // 派生値は閲覧者本人の権限で計算し、マスク列は計算にも応答にも出さない（PIT-19・PIT-20）。
        let masked = self.masked_fields(ctx, &az, &table).await?;
        self.project_read(ctx, &az, &table, &masked, &mut records)
            .await?;
        Ok(records.remove(0))
    }

    /// レコードを削除する（editor・楽観ロック・削除リビジョン記録）。
    pub async fn delete_record(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        id: Uuid,
        expected_rev: i64,
        trace_id: Option<&str>,
    ) -> Result<(), DataError> {
        let az = self.authz_memo();
        self.require_in(
            &az,
            ctx,
            table_id,
            Relation::Editor,
            AuditAction::DataRecordDelete,
            trace_id,
        )
        .await?;
        // テーブルの生存チェック込み（削除済みテーブルの残存 FGA タプルで消させない）。
        let table = self.fetch_live(ctx, table_id).await?;
        let mut tx = self
            .pools
            .for_tenant(&ctx.tenant_id)
            .begin()
            .await
            .map_err(map_db)?;
        let plan = lock_plan(
            &mut tx,
            &ctx.tenant_id,
            table_id,
            table.keys,
            table.schema_version,
        )
        .await?;
        // 行述語つきロック: 不可視行は NotFound（rev オラクル封じ・PIT-21）。
        let current = self
            .lock_visible_by_id(ctx, &az, &mut tx, &table, id)
            .await?
            .ok_or(DataError::NotFound)?;
        if !self.write_allowed(ctx, &az, &table, id).await? {
            return Err(DataError::Forbidden);
        }
        if current.rev != expected_rev {
            return Err(DataError::RevConflict {
                current: current.rev,
            });
        }
        let record_int: i64 = sqlx::query_scalar(
            "DELETE FROM data_record \
             WHERE tenant_int = $4 AND table_int = $5 AND tenant_id = $1 AND table_id = $2 \
               AND id = $3 \
             RETURNING record_int",
        )
        .bind(&ctx.tenant_id)
        .bind(table_id)
        .bind(id)
        .bind(table.keys.tenant_int)
        .bind(table.keys.table_int)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db)?;
        let old_map = decode_record(&table.schema, &current.data.0);
        // そのレコードの索引行を全て消す（同一 Tx）。
        self.index_writer(&plan, &table.schema, &ctx.tenant_id)
            .on_delete(&mut tx, record_int, &old_map)
            .await?;
        crate::derived::materialize::after_record_write(
            &mut tx,
            &ctx.tenant_id,
            &table,
            record_int,
            None,
        )
        .await?;
        let patches = diff_fields(&old_map, &Map::new());
        insert_revision(
            &mut tx,
            ctx,
            RevisionInsert {
                table_id,
                record_id: id,
                rev: current.rev + 1,
                change_kind: "delete",
                patch: &patches,
                trace_id,
            },
        )
        .await?;
        // outbox（同一 Tx）。削除は after が無いので、before で見えていた購読者へ record.removed が出る。
        emit_record_changes(
            &mut tx,
            ctx,
            &table,
            &[RecordChange {
                record_id: id,
                record_int,
                rev: current.rev + 1,
                kind: ChangeKind::Delete,
                before: Some(Before {
                    owner: &current.owner,
                    stored: &current.data.0,
                }),
                after: None,
            }],
            trace_id,
        )
        .await?;
        self.record_audit_record_on(
            &mut tx,
            ctx,
            AuditAction::DataRecordDelete,
            id,
            trace_id,
            serde_json::json!({ "table_id": table_id }),
        )
        .await?;
        tx.commit().await.map_err(map_db)?;
        Ok(())
    }

    /// FSM 管理テーブルの status フィールドへの**直接 update** を拒否する（遷移 API を迂回させない）。
    pub(crate) fn reject_fsm_status_write<'a>(
        table: &crate::model::DataTable,
        fields: impl Iterator<Item = &'a String>,
    ) -> Result<(), DataError> {
        let (Some(_), Some(status)) = (&table.schema.fsm_ref, &table.schema.status_field) else {
            return Ok(());
        };
        for f in fields {
            if f == status {
                // 利用者向けの文言（列は書き手が名指ししたもの・表示名で示す・検証 2-C-P2-4）。
                let label = table
                    .schema
                    .field(status)
                    .map(|d| d.display_name.as_str())
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or(status);
                return Err(DataError::input_detail(
                    crate::InvalidCode::FsmInvalid,
                    Some(status),
                    format!("「{label}」は状態の遷移でだけ変えられます（遷移できる先から選んでください）"),
                ));
            }
        }
        Ok(())
    }

    /// record_ref 値の存在検証（参照先テーブル・同一テナント内）。
    ///
    /// 参照整合は「書き手が参照先を**見える**こと」まで要求する（PIT-20）。検査は
    /// [`Self::check_record_refs_many`]（一括書込パスと同じ関数）に 1 件で渡す。
    pub(crate) async fn check_record_refs(
        &self,
        ctx: &AuthContext,
        schema: &TableSchema,
        data: &Map<String, Value>,
    ) -> Result<(), DataError> {
        let az = self.authz_memo();
        match self
            .check_record_refs_many(ctx, &az, schema, &[data])
            .await?
            .pop()
            .flatten()
        {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// record_ref 値の存在・可視性の検証（複数レコード・リンク列ごとに参照先を 1 回で引く）。
    ///
    /// 返り値はレコードごとの検証エラー（`None` は合格）。参照先の行は**書き手本人の権限で**引く
    /// （PIT-20: テーブル viewer ＋参照先テーブルの行述語の二段。存在オラクル・不可視行への参照の
    /// 持ち込みの双方を塞ぐ）。不可視・不在・テーブル削除済みは「見つからない」と同一の応答。
    pub(crate) async fn check_record_refs_many(
        &self,
        ctx: &AuthContext,
        az: &AuthzMemo<'_>,
        schema: &TableSchema,
        records: &[&Map<String, Value>],
    ) -> Result<Vec<Option<DataError>>, DataError> {
        let mut errors: Vec<Option<DataError>> = records.iter().map(|_| None).collect();
        for f in &schema.fields {
            if f.field_type != FieldType::RecordRef {
                continue;
            }
            let Some(ref_table) = f.ref_table else {
                continue;
            };
            let mut per_record: Vec<(usize, Vec<Uuid>)> = Vec::new();
            for (i, data) in records.iter().enumerate() {
                if errors[i].is_some() {
                    continue;
                }
                let Some(v) = data.get(&f.key) else { continue };
                // 書込検証で UUID の配列へ正規化済み（文字列は移行前の v1 の保存形・念のため受ける）。
                let ids: Vec<Uuid> = match v {
                    Value::String(s) => {
                        if let Ok(id) = Uuid::parse_str(s) {
                            vec![id]
                        } else {
                            errors[i] = Some(DataError::input_detail(
                                crate::InvalidCode::ValueTypeMismatch,
                                Some(&f.key),
                                format!("'{}' は UUID 文字列である必要があります", f.key),
                            ));
                            continue;
                        }
                    }
                    Value::Array(a) => a
                        .iter()
                        .filter_map(Value::as_str)
                        .filter_map(|s| Uuid::parse_str(s).ok())
                        .collect(),
                    _ => continue,
                };
                if !ids.is_empty() {
                    per_record.push((i, ids));
                }
            }
            if per_record.is_empty() {
                continue;
            }
            let all: Vec<Uuid> = per_record
                .iter()
                .flat_map(|(_, ids)| ids.iter().copied())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            let visible = self
                .visible_ref_rows(
                    ctx,
                    az,
                    ref_table,
                    &all,
                    &std::collections::BTreeSet::new(),
                    0,
                )
                .await?;
            for (i, ids) in per_record {
                if ids.iter().any(|id| !visible.rows.contains_key(id)) {
                    errors[i] = Some(DataError::input_detail(
                        crate::InvalidCode::InvalidOption,
                        Some(&f.key),
                        format!("'{}' の参照先レコードが見つかりません", f.key),
                    ));
                }
            }
        }
        Ok(errors)
    }
}

/// unique 制約違反を 409 へ写す。
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn map_unique(e: sqlx::Error) -> DataError {
    match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => DataError::UniqueViolation,
        _ => map_db(e),
    }
}

//! レコード変更を主体の見え方で返す（ワークフローのデータ変更トリガ・wait(event)・GAP-07）。
//!
//! outbox の公開部（`data.record.changed` / `data.record.transitioned`）は値を載せない
//! （[`crate::stream::outbox`]）。ワークフローのトリガは、変更を**誰の見え方で**受け取るかを決めてから
//! 値を作る必要がある（トリガは委譲元・wait は run の実行主体）。ここはその見え方を、レコードの読取
//! （`get_record`）と変更履歴（`list_revisions`）と同じ関数で作る単一の入口:
//!
//! 1. テーブルの viewer とテーブルの生存を確かめる（見えないテーブルは `NotFound` / `Forbidden`）。
//! 2. **今の行**が主体に見えるかを行述語で確かめる（見えない・削除済みなら `None`）。判定は呼んだ時点の
//!    行で行う（SSE の after 判定と同じ）。
//! 3. その rev のリビジョンを 1 件読み、`list_revisions` と同じくマスク列を落とし、リンク値を見える参照先
//!    だけにする。更新で見える差分が 0 件なら `None`（読めない列だけの変更では起こさない）。遷移で状態列が
//!    読めなければ `None`。
//! 4. `record` を `get_record` と同じ投影（派生値の解決・マスク）で作る。
//!
//! `None`（発火しない）であったことは記録しない（不可視の変更の存在を残さない）。
//! 一括取込の目印（private payload の `bulk`）は [`DataStore::bulk_flags`] が真偽だけを返す。

use std::collections::HashMap;

use authz::AuthContext;
use chrono::{DateTime, Utc};
use serde_json::{json, Map, Value};
use sqlx::types::Json;
use storage::audit::AuditAction;
use uuid::Uuid;

use crate::model::{DataRecord, FieldPatch, RecordRevision};
use crate::store::DataStore;
use crate::{map_db, DataError};

/// 変更の種別（ワークフローのイベント source の材料）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeEventKind {
    /// 1 件の作成（`data.record.changed` / create）。
    Create,
    /// 値の更新（FSM 遷移による状態列の更新を含む・`data.record.changed` / update）。
    Update,
    /// FSM 遷移（`data.record.transitioned`）。
    Transition,
}

/// outbox の 1 件から作る変更の参照（値を持たない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeEventRef {
    pub table_id: Uuid,
    pub record_id: Uuid,
    /// 変更後の rev（outbox の version・公開部の `rev`）。
    pub rev: i64,
    pub kind: ChangeEventKind,
}

/// 主体の見え方で作った変更（マスク後・見える参照先だけ）。
#[derive(Debug, Clone)]
pub struct ChangeEventView {
    pub table_id: Uuid,
    pub record_id: Uuid,
    pub rev: i64,
    /// リビジョンの種別（create / update / transition）。
    pub change: String,
    /// 遷移元・遷移先の状態（状態列が変わった変更だけ・読めなければ `None`）。
    pub from: Option<String>,
    pub to: Option<String>,
    /// 主体に読める列のうち、変わった列の参照名（名前順）。
    pub changed: Vec<String>,
    /// 変わった列ごとの `{ old, new }`（`changed` と同じ列）。
    pub changes: Map<String, Value>,
    /// 変更した主体（リビジョンの `changed_by`・ワークフロー経由なら委譲元）。
    pub actor: String,
    pub occurred_at: DateTime<Utc>,
    /// 今の行（`get_record` と同じ投影・呼んだ時点の値）。
    pub record: DataRecord,
}

#[derive(sqlx::FromRow)]
struct RevisionRow {
    record_id: Uuid,
    rev: i64,
    changed_by: String,
    change_kind: String,
    patch: Json<Vec<FieldPatch>>,
    created_at: DateTime<Utc>,
}

impl DataStore {
    /// レコード変更 1 件を `ctx` の見え方で返す（見えない変更は `Ok(None)`）。
    ///
    /// テーブルが見えない・relation が無いときは `Err(NotFound / Forbidden)`（呼び出し側は「発火しない」に
    /// 倒す）。それ以外の `Err` は一時的な障害で、呼び出し側は配り直す。
    pub async fn change_event_view(
        &self,
        ctx: &AuthContext,
        ev: &ChangeEventRef,
        trace_id: Option<&str>,
    ) -> Result<Option<ChangeEventView>, DataError> {
        let az = self.authz_memo();
        let table = self
            .open_for_read(ctx, &az, ev.table_id, AuditAction::DataRecordGet, trace_id)
            .await?;
        let Some(row) = self
            .select_visible_by_id(ctx, &az, &table, ev.record_id)
            .await?
        else {
            return Ok(None);
        };
        let rev: Option<RevisionRow> = sqlx::query_as(
            "SELECT record_id, rev, changed_by, change_kind, patch, created_at \
             FROM data_record_revision \
             WHERE tenant_id = $1 AND table_id = $2 AND record_id = $3 AND rev = $4",
        )
        .bind(&ctx.tenant_id)
        .bind(ev.table_id)
        .bind(ev.record_id)
        .bind(ev.rev)
        .fetch_optional(self.pools.for_tenant(&ctx.tenant_id))
        .await
        .map_err(map_db)?;
        let Some(rev) = rev else {
            return Ok(None);
        };
        let masked = self.masked_fields(ctx, &az, &table).await?;
        let mut patch = rev.patch.0;
        crate::revision::upgrade_link_patches(&table.schema, &mut patch);
        Self::apply_mask_patches(&masked, &mut patch);
        let mut revs = [RecordRevision {
            record_id: rev.record_id,
            rev: rev.rev,
            changed_by: rev.changed_by,
            change_kind: rev.change_kind,
            patch,
            created_at: rev.created_at,
        }];
        self.filter_link_patches(ctx, &az, &table, &masked, &mut revs)
            .await?;
        let [rev] = revs;
        let status = table
            .schema
            .status_field
            .as_deref()
            .filter(|k| !masked.contains(*k));
        let state = status.and_then(|k| rev.patch.iter().find(|p| p.field == k));
        match ev.kind {
            // 読めない列だけの変更では起こさない（変わったことも読めない列の情報）。
            ChangeEventKind::Update if rev.patch.is_empty() => return Ok(None),
            // 遷移は状態列が読めるときだけ（遷移元・遷移先そのものが状態列の値）。
            ChangeEventKind::Transition if state.is_none() => return Ok(None),
            _ => {}
        }
        let mut records = vec![row.into_record(&table.schema)];
        self.project_read(ctx, &az, &table, &masked, &mut records)
            .await?;
        let record = records.remove(0);
        let as_state = |v: &Value| v.as_str().map(str::to_string);
        let mut fields: Vec<String> = rev.patch.iter().map(|p| p.field.clone()).collect();
        fields.sort();
        let diffs = rev
            .patch
            .iter()
            .map(|p| (p.field.clone(), json!({ "old": p.old, "new": p.new })))
            .collect();
        Ok(Some(ChangeEventView {
            table_id: ev.table_id,
            record_id: ev.record_id,
            rev: ev.rev,
            change: rev.change_kind,
            from: state.and_then(|p| as_state(&p.old)),
            to: state.and_then(|p| as_state(&p.new)),
            changed: fields,
            changes: diffs,
            actor: rev.changed_by,
            occurred_at: rev.created_at,
            record,
        }))
    }

    /// outbox のイベントのうち一括取込（CSV の取込・一括 upsert）の行かどうか（id → 真偽）。
    ///
    /// private payload の `bulk` だけを読み、真偽だけを返す（旧値などの非公開の材料は外へ出さない）。
    /// 目印の無いイベントは `false`。ワークフローのトリガは一括取込では発火しない（D-26）。
    pub async fn bulk_flags(
        &self,
        tenant_id: &str,
        event_ids: &[i64],
    ) -> Result<HashMap<i64, bool>, DataError> {
        if event_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let mut conn = self
            .pools
            .for_tenant(tenant_id)
            .acquire()
            .await
            .map_err(map_db)?;
        let privates = storage::event::private_payloads(&mut conn, event_ids)
            .await
            .map_err(|e| DataError::Internal(format!("outbox: {e}")))?;
        Ok(event_ids
            .iter()
            .map(|id| {
                let bulk = privates
                    .get(id)
                    .and_then(|p| p.get("bulk"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                (*id, bulk)
            })
            .collect())
    }
}

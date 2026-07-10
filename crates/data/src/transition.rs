//! FSM 遷移サービス（Task 9.10）。
//!
//! status を進める**唯一の経路**（record の直接 update では status を変えられない・
//! [`crate::record`] が禁止）。1 トランザクションで:
//! ①行ロック（行述語込み・不可視 404） ②現 status ∈ from 検証（定義外 422→Invalid）
//! ③actor 述語を**当該行に**評価（不許可 403） ④status 更新＋rev+1＋revision(transition)
//! ⑤outbox `data.record.transitioned`（同一 Tx） ⑥監査 Chain::Yes。
//!
//! **副作用はここに含めない**（通知/転記/AI は Phase 10 workflow-engine のトリガ）。
//! ワークフローが status を進める場合も必ずこの API を叩く（ガード・認可を再評価）。

use authz::{AuthContext, Relation};
use serde_json::{json, Value};
use storage::audit::{AuditEntry, Chain, Decision};
use storage::event::{emit_on, WriteEvent, WriteOp};
use uuid::Uuid;

use crate::fsm::FsmBody;
use crate::revision::{insert_revision, RevisionInsert};
use crate::store::DataStore;
use crate::{map_db, DataError};

/// outbox payload の `event_type`（storage 書込イベントと区別する。storage/workflow relay は
/// これが付いたイベントを storage.write として扱わずスキップし、Phase 10.3 の data トリガが
/// 消費する）。
pub const TRANSITION_EVENT_TYPE: &str = "data.record.transitioned";

impl DataStore {
    /// FSM 遷移を実行する（唯一の status 変更経路）。
    ///
    /// `fsm` / `status_field` は呼び出し側（api 層）が fsm_ref のピンバージョンを
    /// artifact チョークポイント経由で解決して渡す（data crate は artifact へ直接依存しない）。
    #[allow(clippy::too_many_arguments)] // 遷移の入力束（fsm 定義＋行識別＋楽観ロック）。
    pub async fn transition_record(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        record_id: Uuid,
        to: &str,
        expected_rev: i64,
        fsm: &FsmBody,
        status_field: &str,
        trace_id: Option<&str>,
    ) -> Result<crate::model::DataRecord, DataError> {
        self.require(
            ctx,
            table_id,
            Relation::Editor,
            "data.record.transition",
            trace_id,
        )
        .await?;
        let table = self.fetch_live(ctx, table_id).await?;

        let mut tx = self.db.begin().await.map_err(map_db)?;
        // ①行ロック（行述語込み・不可視は 404＝存在オラクルなし）。
        let current = self
            .lock_visible_by_id(ctx, &mut tx, &table, record_id)
            .await?
            .ok_or(DataError::NotFound)?;
        if current.rev != expected_rev {
            return Err(DataError::Conflict(format!(
                "rev が一致しません（現在 {}・指定 {expected_rev}）",
                current.rev
            )));
        }
        let from = current
            .data
            .0
            .get(status_field)
            .and_then(Value::as_str)
            .ok_or_else(|| {
                DataError::Invalid(format!("status フィールド '{status_field}' がありません"))
            })?
            .to_string();
        // ②定義外遷移は 422（Invalid）。
        let transition = fsm
            .transition(&from, to)
            .ok_or_else(|| DataError::Invalid(format!("定義外の遷移です（{from} → {to}）")))?;
        // ③actor 述語を当該行に評価（不許可 403）。読取述語ではなく actor 述語を使う。
        if !self
            .actor_allows(ctx, &table, record_id, &transition.actor)
            .await?
        {
            let _ = self
                .audit
                .record(
                    ctx,
                    AuditEntry {
                        action: "data.record.transition",
                        object_type: "data_record",
                        object_id: &record_id.to_string(),
                        decision: Decision::Deny,
                        trace_id,
                        metadata: json!({ "table_id": table_id, "from": from, "to": to }),
                    },
                )
                .await;
            return Err(DataError::Forbidden);
        }

        // ④status 更新＋rev+1＋revision。
        let new_rev = current.rev + 1;
        let mut data = current.data.0.as_object().cloned().unwrap_or_default();
        data.insert(status_field.to_string(), Value::String(to.to_string()));
        let row: crate::record::RecordRow = sqlx::query_as(
            "UPDATE data_record SET data = $4, rev = $5, updated_at = now() \
             WHERE tenant_id = $1 AND table_id = $2 AND id = $3 \
             RETURNING id, table_id, data, rev, owner, created_at, updated_at",
        )
        .bind(&ctx.tenant_id)
        .bind(table_id)
        .bind(record_id)
        .bind(sqlx::types::Json(Value::Object(data)))
        .bind(new_rev)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db)?;
        let patch = vec![crate::model::FieldPatch {
            field: status_field.to_string(),
            old: Value::String(from.clone()),
            new: Value::String(to.to_string()),
        }];
        insert_revision(
            &mut tx,
            ctx,
            RevisionInsert {
                table_id,
                record_id,
                rev: new_rev,
                change_kind: "transition",
                patch: &patch,
                trace_id,
            },
        )
        .await?;
        // ⑤outbox（同一 Tx）: 副作用は発行しない・イベントのみ（Phase 10 トリガが消費）。
        emit_on(
            &mut tx,
            ctx,
            WriteEvent {
                node_id: record_id,
                version: new_rev,
                op: WriteOp::Update,
                payload: json!({
                    "event_type": TRANSITION_EVENT_TYPE,
                    "table_id": table_id,
                    "record_id": record_id,
                    "from": from,
                    "to": to,
                }),
            },
            trace_id,
        )
        .await
        .map_err(|e| DataError::Internal(format!("outbox: {e}")))?;
        // ⑥監査（実データ変更・書込と同一 Tx・Chain=Yes）。
        storage::audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: "data.record.transition",
                object_type: "data_record",
                object_id: &record_id.to_string(),
                decision: Decision::Allow,
                trace_id,
                metadata: json!({ "table_id": table_id, "from": from, "to": to, "rev": new_rev }),
            },
            Chain::Yes,
        )
        .await
        .map_err(|e| DataError::Internal(format!("audit: {e}")))?;
        tx.commit().await.map_err(map_db)?;
        Ok(row.into_record())
    }
}

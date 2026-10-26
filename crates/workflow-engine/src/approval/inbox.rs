//! 承認の受信箱の台帳の照会と、判断に添えるコメントの台帳（API が使う・GAP-08）。
//!
//! ここは台帳（「誰の受信箱に出すか」）だけを引く。**行が本人に見えるか・今の状態が停止時の状態か**は、API が
//! 承認者本人の `AuthContext` で data のチョークポイントを通して読み直して判定する（二段の 2 段目）。担当者で
//! あることは権限の正ではない（判断は FSM の遷移が actor 述語で改めて認可する）。
//!
//! 受信箱に出すのは、台帳が保留中で、run が実行中で、step が承認を待っている（waiting_event）ものだけ。
//! 期限（承認の期限と run の期限の早い方）を過ぎたものも出さない（回収の tick を待たずに隠す）。

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::types::Json;
use sqlx::PgPool;
use uuid::Uuid;

use super::ApprovalChoiceSpec;
use crate::ir::params::ApprovalDecisionKind;
use crate::run::RunStoreError;

/// 受信箱の 1 件（台帳の行・行の値は持たない）。
#[derive(Debug, Clone)]
pub struct ApprovalRow {
    pub approval_id: Uuid,
    pub workflow_id: Uuid,
    pub run_id: Uuid,
    pub table_id: Uuid,
    pub record_id: Uuid,
    pub title: String,
    pub instructions: Option<String>,
    pub show_fields: Vec<String>,
    pub choices: Vec<ApprovalChoiceSpec>,
    pub from_state: String,
    pub suspend_rev: i64,
    /// 判断の期限（承認の期限と run の期限の早い方・どちらも無ければ None）。
    pub deadline: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    /// 台帳の status。
    pub status: String,
    /// いま判断を受け付けるか（保留中・run が実行中・step が待機中・期限前）。
    pub decidable: bool,
}

impl ApprovalRow {
    /// 判断の種別の選択肢。
    #[must_use]
    pub fn choice(&self, kind: ApprovalDecisionKind) -> Option<&ApprovalChoiceSpec> {
        self.choices.iter().find(|c| c.kind == kind)
    }
}

/// 受信箱の keyset（新しい順・作成時刻と承認 id）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InboxCursor {
    pub created_at: DateTime<Utc>,
    pub approval_id: Uuid,
}

impl InboxCursor {
    /// URL に載せる形（`<RFC3339>_<uuid>`）。
    #[must_use]
    pub fn encode(&self) -> String {
        format!(
            "{}_{}",
            self.created_at
                .to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
            self.approval_id
        )
    }

    /// [`Self::encode`] の逆（形が合わなければ None）。
    #[must_use]
    pub fn decode(s: &str) -> Option<Self> {
        let (ts, id) = s.rsplit_once('_')?;
        Some(InboxCursor {
            created_at: DateTime::parse_from_rfc3339(ts).ok()?.with_timezone(&Utc),
            approval_id: Uuid::parse_str(id).ok()?,
        })
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    approval_id: Uuid,
    workflow_id: Uuid,
    run_id: Uuid,
    table_id: Uuid,
    record_id: Uuid,
    title: String,
    instructions: Option<String>,
    show_fields: Vec<String>,
    decisions: Json<Value>,
    from_state: String,
    suspend_rev: i64,
    deadline: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    status: String,
    decidable: bool,
}

impl From<Row> for ApprovalRow {
    fn from(r: Row) -> Self {
        ApprovalRow {
            approval_id: r.approval_id,
            workflow_id: r.workflow_id,
            run_id: r.run_id,
            table_id: r.table_id,
            record_id: r.record_id,
            title: r.title,
            instructions: r.instructions,
            show_fields: r.show_fields,
            choices: serde_json::from_value(r.decisions.0).unwrap_or_default(),
            from_state: r.from_state,
            suspend_rev: r.suspend_rev,
            deadline: r.deadline,
            created_at: r.created_at,
            status: r.status,
            decidable: r.decidable,
        }
    }
}

/// 台帳の列（受信箱と 1 件の取得で共通）。期限は承認と run の早い方。
const COLUMNS: &str = "a.approval_id, a.workflow_id, a.run_id, a.table_id, a.record_id, a.title, \
     a.instructions, a.show_fields, a.decisions, a.from_state, a.suspend_rev, \
     CASE WHEN a.deadline IS NULL THEN r.timeout_at \
          WHEN r.timeout_at IS NULL THEN a.deadline \
          ELSE least(a.deadline, r.timeout_at) END AS deadline, \
     a.created_at, a.status, \
     COALESCE(a.status = 'pending' AND r.status = 'running' AND s.status = 'waiting_event' \
      AND (a.deadline IS NULL OR a.deadline > now()) \
      AND (r.timeout_at IS NULL OR r.timeout_at > now()), false) AS decidable";

/// 担当者（本人・所属ロール）の述語（別名 `a`・$3 = 本人の id・$4 = 所属ロールの配列）。
const ASSIGNED: &str = "EXISTS (SELECT 1 FROM workflow_approval_assignee x \
     WHERE x.tenant_id = a.tenant_id AND x.approval_id = a.approval_id \
       AND ((x.kind = 'user' AND x.subject = $3) OR (x.kind = 'role' AND x.subject = ANY($4))))";

/// 受信箱の台帳（API が承認者本人の権限で行を読み直して出す・[`super`]）。
#[derive(Clone)]
pub struct ApprovalStore {
    db: PgPool,
}

#[allow(clippy::needless_pass_by_value)]
fn map_db(e: sqlx::Error) -> RunStoreError {
    RunStoreError::Internal(format!("db: {e}"))
}

impl ApprovalStore {
    #[must_use]
    pub fn new(db: PgPool) -> Self {
        ApprovalStore { db }
    }

    /// 本人（`user`）か所属ロール（`roles`）宛ての、いま判断できる承認（新しい順・`after` より古いもの）。
    pub async fn inbox(
        &self,
        tenant_id: &str,
        org: &str,
        user: &str,
        roles: &[String],
        after: Option<InboxCursor>,
        limit: i64,
    ) -> Result<Vec<ApprovalRow>, RunStoreError> {
        let sql = format!(
            "SELECT {COLUMNS} FROM workflow_approval a \
             JOIN workflow_run r ON r.tenant_id = a.tenant_id AND r.run_id = a.run_id \
             JOIN step_execution s ON s.tenant_id = a.tenant_id AND s.run_id = a.run_id \
                                  AND s.step_path = a.step_path \
             WHERE a.tenant_id = $1 AND a.org = $2 AND a.status = 'pending' \
               AND r.status = 'running' AND s.status = 'waiting_event' \
               AND (a.deadline IS NULL OR a.deadline > now()) \
               AND (r.timeout_at IS NULL OR r.timeout_at > now()) \
               AND {ASSIGNED} \
               AND ($5::timestamptz IS NULL OR (a.created_at, a.approval_id) < ($5, $6)) \
             ORDER BY a.created_at DESC, a.approval_id DESC LIMIT $7"
        );
        let rows: Vec<Row> = sqlx::query_as(&sql)
            .bind(tenant_id)
            .bind(org)
            .bind(user)
            .bind(roles)
            .bind(after.map(|c| c.created_at))
            .bind(after.map_or(Uuid::nil(), |c| c.approval_id))
            .bind(limit)
            .fetch_all(&self.db)
            .await
            .map_err(map_db)?;
        Ok(rows.into_iter().map(ApprovalRow::from).collect())
    }

    /// 本人か所属ロール宛ての承認 1 件（保留中でなくても返す・担当者でなければ None）。
    pub async fn get_assigned(
        &self,
        tenant_id: &str,
        org: &str,
        user: &str,
        roles: &[String],
        approval_id: Uuid,
    ) -> Result<Option<ApprovalRow>, RunStoreError> {
        let sql = format!(
            "SELECT {COLUMNS} FROM workflow_approval a \
             JOIN workflow_run r ON r.tenant_id = a.tenant_id AND r.run_id = a.run_id \
             LEFT JOIN step_execution s ON s.tenant_id = a.tenant_id AND s.run_id = a.run_id \
                                       AND s.step_path = a.step_path \
             WHERE a.tenant_id = $1 AND a.org = $2 AND a.approval_id = $5 AND {ASSIGNED}"
        );
        let row: Option<Row> = sqlx::query_as(&sql)
            .bind(tenant_id)
            .bind(org)
            .bind(user)
            .bind(roles)
            .bind(approval_id)
            .fetch_optional(&self.db)
            .await
            .map_err(map_db)?;
        Ok(row.map(ApprovalRow::from))
    }

    /// 判断に添えるコメントを、遷移の後の rev（`rev`）の鍵で先に置く（遷移と同時に起床しても拾えるように）。
    ///
    /// 同じ承認・同じ rev の鍵が既にあれば（別の担当者が同じ版で判断中）置かずに `false`。遷移に失敗したら
    /// [`Self::drop_comment`] で外す。
    pub async fn put_comment(
        &self,
        tenant_id: &str,
        approval_id: Uuid,
        rev: i64,
        author: &str,
        decision: ApprovalDecisionKind,
        body: &str,
    ) -> Result<bool, RunStoreError> {
        let done = sqlx::query(
            "INSERT INTO workflow_approval_comment (tenant_id, approval_id, rev, author, decision, body) \
             VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING",
        )
        .bind(tenant_id)
        .bind(approval_id)
        .bind(rev)
        .bind(author)
        .bind(decision.as_str())
        .bind(body)
        .execute(&self.db)
        .await
        .map_err(map_db)?;
        Ok(done.rows_affected() == 1)
    }

    /// [`Self::put_comment`] で置いたコメントを外す（遷移に失敗した判断のコメントを残さない）。
    pub async fn drop_comment(
        &self,
        tenant_id: &str,
        approval_id: Uuid,
        rev: i64,
        author: &str,
    ) -> Result<(), RunStoreError> {
        sqlx::query(
            "DELETE FROM workflow_approval_comment \
             WHERE tenant_id = $1 AND approval_id = $2 AND rev = $3 AND author = $4",
        )
        .bind(tenant_id)
        .bind(approval_id)
        .bind(rev)
        .bind(author)
        .execute(&self.db)
        .await
        .map_err(map_db)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_round_trips() {
        let c = InboxCursor {
            created_at: DateTime::parse_from_rfc3339("2026-10-26T01:02:03.123456Z")
                .unwrap_or_default()
                .with_timezone(&Utc),
            approval_id: Uuid::new_v4(),
        };
        assert_eq!(InboxCursor::decode(&c.encode()), Some(c));
        assert_eq!(InboxCursor::decode("nope"), None);
    }
}

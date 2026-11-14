//! B2 の自動実行（event・cron）の関数の直近の失敗（GAP-60 の残り・migration 0150）。
//!
//! 自動実行の失敗（シークレット・トークン・実行の誤り・関数の失敗）を、作成者が見られる場所に残す。関数ごとに 1 行で、
//! 行があるのは「その関数の最後の自動実行が失敗した」とき。次に成功したら消す（実行のたびに行を足さない）。
//!
//! 残す文は作成者に見せてよいものだけ（[`crate::FunctionFailure`] と同じ整え方。スタックの行・内部の文・URL・ほかの人の
//! 情報を含まない）。それでも作成者のコードが投げた文にはアプリが読んだレコードの中身が入りうるので、インストールの
//! 一覧では、アプリの持ち主（artifact の owner・インストールとアンインストールができる人）にだけ返す
//! （[`InstallService::function_failures`]）。

use std::collections::{HashMap, HashSet};

use authz::{AuthContext, CheckTuple, Consistency, Relation};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{map_db, AppPlatformError, FunctionFailure, InstallService};

/// 自動実行のきっかけ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionTrigger {
    /// イベント（テーブルの変更など・manifest の `server.events`）。
    Event,
    /// スケジュール（manifest の `server.cron`）。
    Cron,
}

impl FunctionTrigger {
    /// 記録する名前（migration 0150 の check と同じ）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            FunctionTrigger::Event => "event",
            FunctionTrigger::Cron => "cron",
        }
    }
}

/// 自動実行の関数の直近の失敗（GET /apps/installations の `function_failures`・アプリの持ち主にだけ返す）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct AppFunctionFailure {
    /// 失敗した関数（manifest の `server.functions` の名前）。
    pub function: String,
    /// きっかけ（`event`・`cron`）。
    pub trigger: String,
    /// 理由（作成者が投げた誤りは `script_error` など・[`FunctionFailure::code`] と同じ語彙）。
    pub code: String,
    /// 作成者に見せてよい理由の文（スタックの行・内部の文・URL を含まない）。
    pub message: String,
    /// 最後に失敗した時刻。
    pub failed_at: DateTime<Utc>,
    /// 続けて失敗し始めた時刻。
    pub first_failed_at: DateTime<Utc>,
    /// 続けて失敗した回数。
    pub failures: i32,
}

#[derive(sqlx::FromRow)]
struct Row {
    app_id: Uuid,
    function: String,
    trigger: String,
    code: String,
    message: String,
    failed_at: DateTime<Utc>,
    first_failed_at: DateTime<Utc>,
    failures: i32,
}

/// 1 つのアプリについて返す失敗の数の上限（一覧の応答を大きくしない）。
const MAX_FAILURES_PER_APP: usize = 5;

/// 自動実行の関数の直近の失敗を書く（event・cron のトリガが呼ぶ）。
#[derive(Clone)]
pub struct FunctionFailureStore {
    db: PgPool,
}

impl FunctionFailureStore {
    pub fn new(db: PgPool) -> Self {
        FunctionFailureStore { db }
    }

    /// 失敗を記録する（同じ関数の行があれば、理由と時刻を書き換え、続けて失敗した回数を足す）。
    pub async fn record_failure(
        &self,
        tenant_id: &str,
        org: &str,
        app_id: Uuid,
        function: &str,
        trigger: FunctionTrigger,
        failure: &FunctionFailure,
    ) -> Result<(), AppPlatformError> {
        sqlx::query(
            "INSERT INTO app_function_failure \
                 (tenant_id, org, app_id, function, trigger, code, message) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (tenant_id, app_id, function) DO UPDATE SET \
                 org = EXCLUDED.org, trigger = EXCLUDED.trigger, code = EXCLUDED.code, \
                 message = EXCLUDED.message, failed_at = now(), \
                 failures = app_function_failure.failures + 1",
        )
        .bind(tenant_id)
        .bind(org)
        .bind(app_id)
        .bind(function)
        .bind(trigger.as_str())
        .bind(&failure.code)
        .bind(&failure.message)
        .execute(&self.db)
        .await
        .map_err(map_db)?;
        Ok(())
    }

    /// 成功を記録する（直近の失敗の行を消す。行が無ければ何もしない）。
    pub async fn record_success(
        &self,
        tenant_id: &str,
        app_id: Uuid,
        function: &str,
    ) -> Result<(), AppPlatformError> {
        sqlx::query(
            "DELETE FROM app_function_failure \
             WHERE tenant_id = $1 AND app_id = $2 AND function = $3",
        )
        .bind(tenant_id)
        .bind(app_id)
        .bind(function)
        .execute(&self.db)
        .await
        .map_err(map_db)?;
        Ok(())
    }
}

impl InstallService {
    /// 呼ぶ人が持ち主のアプリについて、自動実行の関数の直近の失敗を返す（新しい順・アプリごとに 5 件まで）。
    ///
    /// テナントと呼ぶ人の組織で絞り（PIT-45）、失敗のあるアプリだけを OpenFGA でまとめて確かめる（artifact の
    /// owner・[`crate::AppUsageStore`] と同じ）。持ち主でないアプリの失敗は返さない（関数の文にアプリが読んだ
    /// レコードの中身が入りうる）。
    pub async fn function_failures(
        &self,
        ctx: &AuthContext,
        app_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, Vec<AppFunctionFailure>>, AppPlatformError> {
        if app_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT app_id, function, trigger, code, message, failed_at, first_failed_at, failures \
             FROM app_function_failure \
             WHERE tenant_id = $1 AND org = $2 AND app_id = ANY($3) \
             ORDER BY failed_at DESC, function",
        )
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(app_ids)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        if rows.is_empty() {
            return Ok(HashMap::new());
        }
        let mut failing: Vec<Uuid> = Vec::new();
        for row in &rows {
            if !failing.contains(&row.app_id) {
                failing.push(row.app_id);
            }
        }
        let checks: Vec<CheckTuple> = failing
            .iter()
            .map(|id| CheckTuple {
                subject: ctx.subject(),
                relation: Relation::Owner,
                object: ctx.ns().artifact(&id.to_string()),
            })
            .collect();
        let allowed = self
            .authz
            .batch_check(&checks, Consistency::MinimizeLatency)
            .await
            .map_err(|e| AppPlatformError::Internal(format!("authz: {e}")))?;
        let owned: HashSet<Uuid> = failing
            .into_iter()
            .zip(allowed)
            .filter_map(|(id, ok)| ok.then_some(id))
            .collect();
        let mut out: HashMap<Uuid, Vec<AppFunctionFailure>> = HashMap::new();
        for row in rows.into_iter().filter(|r| owned.contains(&r.app_id)) {
            let list = out.entry(row.app_id).or_default();
            if list.len() < MAX_FAILURES_PER_APP {
                list.push(AppFunctionFailure {
                    function: row.function,
                    trigger: row.trigger,
                    code: row.code,
                    message: row.message,
                    failed_at: row.failed_at,
                    first_failed_at: row.first_failed_at,
                    failures: row.failures,
                });
            }
        }
        Ok(out)
    }
}

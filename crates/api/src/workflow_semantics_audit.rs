//! 保存済みワークフローのうち、条件の意味論の切替（2026-10・query-optimizer.md §3.1）で分岐の結果が
//! 変わり得るものを一覧にする（Task 15.7）。`shiki-admin workflow-conditions` の本体。
//!
//! 検出の規則は保存時の警告と同じ関数（[`workflow_engine::semantics_warnings`]）。DB では
//! 「比較演算子 eq / neq / in / contains / exists / is_null を含む」版だけを JSON パスで先に絞り
//! （全件を読んでからフィルタしない）、keyset で少しずつ読む。運用ツールなので全テナントを走査する
//! （`tenant` で絞れる）。読取専用。

use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;
use workflow_engine::{semantics_warnings, WorkflowIr};

/// 1 件の検出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConditionFinding {
    pub tenant_id: String,
    pub workflow_id: Uuid,
    pub name: String,
    pub version: i64,
    /// 条件の位置（ノード id。トリガの filter は None）。
    pub node_id: Option<String>,
    pub path: Option<String>,
    pub message: String,
}

/// 走査の位置（tenant_id, workflow_id, version の keyset）。
pub type AuditCursor = (String, Uuid, i64);

/// 1 ページ分を走査する。`all_versions = false` は各ワークフローの最新版だけ。次の位置も返す
/// （`None` で終わり）。
pub async fn scan_page(
    db: &PgPool,
    tenant: Option<&str>,
    all_versions: bool,
    after: Option<&AuditCursor>,
    limit: i64,
) -> Result<(Vec<ConditionFinding>, Option<AuditCursor>), sqlx::Error> {
    let (a_tenant, a_id, a_ver) = after
        .cloned()
        .unwrap_or_else(|| (String::new(), Uuid::nil(), i64::MIN));
    let rows: Vec<(String, Uuid, String, i64, Value)> = sqlx::query_as(
        "SELECT a.tenant_id, a.id, a.name, v.version, v.body \
         FROM artifact a \
         JOIN artifact_version v ON v.tenant_id = a.tenant_id AND v.artifact_id = a.id \
         WHERE a.kind = 'workflow' AND a.deleted_at IS NULL \
           AND ($1::text IS NULL OR a.tenant_id = $1) \
           AND ($2 OR v.version = a.current_version) \
           AND (a.tenant_id, a.id, v.version) > ($3, $4, $5) \
           AND jsonb_path_exists(v.body, \
                 '$.** ? (@.op == \"eq\" || @.op == \"neq\" || @.op == \"in\" || \
                          @.op == \"contains\" || @.op == \"exists\" || @.op == \"is_null\")') \
         ORDER BY a.tenant_id, a.id, v.version \
         LIMIT $6",
    )
    .bind(tenant)
    .bind(all_versions)
    .bind(&a_tenant)
    .bind(a_id)
    .bind(a_ver)
    .bind(limit.max(1))
    .fetch_all(db)
    .await?;
    let next = if i64::try_from(rows.len()).unwrap_or(i64::MAX) < limit.max(1) {
        None
    } else {
        rows.last().map(|r| (r.0.clone(), r.1, r.3))
    };
    let mut out = Vec::new();
    for (tenant_id, workflow_id, name, version, body) in rows {
        // 現行のスキーマで読めない古い版は対象外（実行もされない）。
        let Ok(ir) = WorkflowIr::from_json(&body) else {
            continue;
        };
        for w in semantics_warnings(&ir) {
            out.push(ConditionFinding {
                tenant_id: tenant_id.clone(),
                workflow_id,
                name: name.clone(),
                version,
                node_id: w.node_id,
                path: w.path,
                message: w.message,
            });
        }
    }
    Ok((out, next))
}

//! 用途（#751）: 用途ラベルの内訳・部署×用途・使われている機能・よく使われるスキル等。

use serde::Serialize;
use sqlx::PgPool;
use utoipa::ToSchema;

use super::{
    check_scope, departments, dept_label, guarded, ratio, ReportError, Scope, ACTIVE_THREAD,
    DEPT_FILTER,
};
use crate::insights::{category_label, settings::InsightSettings, MIN_GROUP_SIZE};

/// 用途 1 行。
#[derive(Debug, Serialize, ToSchema)]
pub struct CategoryRow {
    pub key: String,
    pub label: String,
    pub threads: i64,
    pub share: f64,
    /// 使った人（5 人未満は `null`）。
    pub users: Option<i64>,
    /// 前期の割合（前期に会話が無ければ `null`）。
    pub share_prev: Option<f64>,
}

/// 部署×用途の 1 行（各用途の割合・行で 100%）。
#[derive(Debug, Serialize, ToSchema)]
pub struct MatrixRow {
    pub role_id: String,
    pub label: String,
    pub members: i64,
    /// `columns` の順。少人数・会話なしは `null`。
    pub shares: Vec<Option<f64>>,
}

/// 機能の利用。
#[derive(Debug, Serialize, ToSchema)]
pub struct FeatureRow {
    pub key: String,
    pub label: String,
    /// 使った人。
    pub users: i64,
    /// 回数。
    pub count: i64,
}

/// よく使われるもの（スキル・ミニアプリ・ワークフロー）。
#[derive(Debug, Serialize, ToSchema)]
pub struct TopItem {
    pub id: String,
    pub name: String,
    /// 使った人（5 人未満は `null`）。
    pub users: Option<i64>,
    pub count: i64,
}

/// 用途。
#[derive(Debug, Serialize, ToSchema)]
pub struct UseCases {
    pub classify_enabled: bool,
    /// 最後に分類した時刻（まだ無ければ `null`）。
    pub last_classified_at: Option<chrono::DateTime<chrono::Utc>>,
    pub total_threads: i64,
    pub categories: Vec<CategoryRow>,
    /// 部署×用途の列（用途キー）と表示名。
    pub matrix_columns: Vec<String>,
    pub matrix_labels: Vec<String>,
    pub matrix: Vec<MatrixRow>,
    pub active_users: i64,
    pub features: Vec<FeatureRow>,
    pub top_skills: Vec<TopItem>,
    pub top_miniapps: Vec<TopItem>,
    pub top_workflows: Vec<TopItem>,
}

/// 用途ごとの会話数と利用者数（多い順）。
pub(crate) async fn category_counts(
    db: &PgPool,
    s: &Scope,
) -> Result<Vec<(String, i64, i64)>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT ti.category, count(*), count(DISTINCT ti.owner) \
         FROM thread_insight ti \
         WHERE ti.tenant_id = $1 AND ti.org = $2 AND {ACTIVE_THREAD} \
           AND {} \
         GROUP BY 1 ORDER BY 2 DESC, 1",
        DEPT_FILTER.replace("user_id IN", "ti.owner IN")
    ))
    .bind(&s.tenant_id)
    .bind(&s.org)
    .bind(s.from)
    .bind(s.to)
    .bind(s.dept.as_deref())
    .fetch_all(db)
    .await
}

async fn matrix(db: &PgPool, s: &Scope, columns: &[String]) -> Result<Vec<MatrixRow>, sqlx::Error> {
    let rows: Vec<(String, String, i64)> = sqlx::query_as(&format!(
        "SELECT r.role_id, ti.category, count(*) \
         FROM thread_insight ti \
         JOIN directory_user_role r ON r.tenant_id = ti.tenant_id AND r.org = ti.org \
          AND r.user_id = ti.owner AND r.source = 'group' \
         WHERE ti.tenant_id = $1 AND ti.org = $2 AND {ACTIVE_THREAD} \
         GROUP BY 1, 2"
    ))
    .bind(&s.tenant_id)
    .bind(&s.org)
    .bind(s.from)
    .bind(s.to)
    .fetch_all(db)
    .await?;
    Ok(departments(db, &s.tenant_id, &s.org)
        .await?
        .into_iter()
        .map(|(role_id, display, members)| {
            let mine: Vec<&(String, String, i64)> =
                rows.iter().filter(|(r, _, _)| *r == role_id).collect();
            let total: i64 = mine.iter().map(|(_, _, n)| *n).sum();
            let shares = columns
                .iter()
                .map(|c| {
                    let n = mine
                        .iter()
                        .find(|(_, cat, _)| cat == c)
                        .map_or(0, |(_, _, n)| *n);
                    ratio(n, total).and_then(|r| guarded(members, r))
                })
                .collect();
            MatrixRow {
                label: dept_label(&role_id, &display),
                role_id,
                members,
                shares,
            }
        })
        .collect())
}

/// 機能ごとの (キー, 表示名, 列)。
const FEATURES: &[(&str, &str, &str)] = &[
    ("doc_search", "社内文書の検索", "doc_searches"),
    ("web_search", "Web 検索", "web_searches"),
    ("agent", "エージェントモード", "agent_runs"),
    ("code", "コード実行", "code_runs"),
    ("skill", "スキル", "skill_uses"),
    ("miniapp", "ミニアプリ", "miniapp_uses"),
    ("workflow", "ワークフロー", "workflow_runs"),
];

async fn features(db: &PgPool, s: &Scope) -> Result<Vec<FeatureRow>, sqlx::Error> {
    let values = FEATURES
        .iter()
        .map(|(key, _, col)| format!("('{key}', {col})"))
        .collect::<Vec<_>>()
        .join(", ");
    let rows: Vec<(String, i64, i64)> = sqlx::query_as(&format!(
        "SELECT v.k, count(DISTINCT user_id) FILTER (WHERE v.n > 0), \
                COALESCE(sum(v.n), 0)::bigint \
         FROM ai_activity_daily, LATERAL (VALUES {values}) AS v(k, n) \
         WHERE tenant_id = $1 AND org = $2 AND day BETWEEN $3 AND $4 AND {DEPT_FILTER} \
         GROUP BY v.k"
    ))
    .bind(&s.tenant_id)
    .bind(&s.org)
    .bind(s.from)
    .bind(s.to)
    .bind(s.dept.as_deref())
    .fetch_all(db)
    .await?;
    Ok(FEATURES
        .iter()
        .map(|(key, label, _)| {
            let (users, count) = rows
                .iter()
                .find(|(k, _, _)| k == key)
                .map_or((0, 0), |(_, u, c)| (*u, *c));
            FeatureRow {
                key: (*key).into(),
                label: (*label).into(),
                users,
                count,
            }
        })
        .collect())
}

fn top_items(rows: Vec<(String, String, i64, i64)>) -> Vec<TopItem> {
    rows.into_iter()
        .map(|(id, name, users, count)| TopItem {
            id,
            name,
            users: (users >= MIN_GROUP_SIZE).then_some(users),
            count,
        })
        .collect()
}

/// よく使われるスキル・ミニアプリ（監査ログ）とワークフロー（実行記録）。
async fn tops(
    db: &PgPool,
    s: &Scope,
) -> Result<(Vec<TopItem>, Vec<TopItem>, Vec<TopItem>), sqlx::Error> {
    let range = |t: &str| {
        format!(
            "{t}.created_at >= ($3::date)::timestamp AT TIME ZONE 'Asia/Tokyo' \
             AND {t}.created_at < ($4::date + 1)::timestamp AT TIME ZONE 'Asia/Tokyo'"
        )
    };
    let skills: Vec<(String, String, i64, i64)> = sqlx::query_as(&format!(
        "SELECT al.object_id, COALESCE(MAX(a.name), al.object_id), \
                count(DISTINCT al.actor), count(*) \
         FROM audit_log al \
         LEFT JOIN artifact a ON a.tenant_id = al.tenant_id AND a.id::text = al.object_id \
         WHERE al.tenant_id = $1 AND al.org = $2 AND al.action = 'skill.apply' \
           AND al.decision = 'allow' AND {} \
           AND {} \
         GROUP BY 1 ORDER BY 4 DESC LIMIT 5",
        range("al"),
        DEPT_FILTER.replace("user_id IN", "al.actor IN")
    ))
    .bind(&s.tenant_id)
    .bind(&s.org)
    .bind(s.from)
    .bind(s.to)
    .bind(s.dept.as_deref())
    .fetch_all(db)
    .await?;
    let apps: Vec<(String, String, i64, i64)> = sqlx::query_as(&format!(
        "SELECT al.object_id, \
                COALESCE((SELECT i.app_name FROM app_installation i \
                           WHERE i.tenant_id = al.tenant_id AND i.app_id::text = al.object_id \
                           LIMIT 1), al.object_id), \
                count(DISTINCT al.actor), count(*) \
         FROM audit_log al \
         WHERE al.tenant_id = $1 AND al.org = $2 AND al.action = 'gateway.call' \
           AND al.decision = 'allow' AND {} \
           AND {} \
         GROUP BY 1, al.tenant_id ORDER BY 4 DESC LIMIT 5",
        range("al"),
        DEPT_FILTER.replace("user_id IN", "al.actor IN")
    ))
    .bind(&s.tenant_id)
    .bind(&s.org)
    .bind(s.from)
    .bind(s.to)
    .bind(s.dept.as_deref())
    .fetch_all(db)
    .await?;
    let workflows: Vec<(String, String, i64, i64)> = sqlx::query_as(&format!(
        "SELECT w.workflow_id::text, COALESCE(MAX(a.name), w.workflow_id::text), \
                count(DISTINCT COALESCE(w.delegator, w.principal)), \
                count(*) FILTER (WHERE w.status = 'succeeded') \
         FROM workflow_run w \
         LEFT JOIN artifact a ON a.tenant_id = w.tenant_id AND a.id = w.workflow_id \
         WHERE w.tenant_id = $1 AND w.org = $2 AND {} \
           AND {} \
         GROUP BY 1 ORDER BY 4 DESC LIMIT 5",
        range("w"),
        DEPT_FILTER.replace("user_id IN", "COALESCE(w.delegator, w.principal) IN")
    ))
    .bind(&s.tenant_id)
    .bind(&s.org)
    .bind(s.from)
    .bind(s.to)
    .bind(s.dept.as_deref())
    .fetch_all(db)
    .await?;
    Ok((top_items(skills), top_items(apps), top_items(workflows)))
}

/// 用途を組み立てる。
pub async fn use_cases(
    db: &PgPool,
    s: &Scope,
    settings: &InsightSettings,
) -> Result<UseCases, ReportError> {
    check_scope(db, s).await?;
    let counts = category_counts(db, s).await?;
    let prev = category_counts(db, &s.previous()).await?;
    let total: i64 = counts.iter().map(|c| c.1).sum();
    let total_prev: i64 = prev.iter().map(|c| c.1).sum();
    let categories: Vec<CategoryRow> = counts
        .iter()
        .map(|(key, threads, users)| CategoryRow {
            key: key.clone(),
            label: category_label(key).to_string(),
            threads: *threads,
            share: ratio(*threads, total).unwrap_or(0.0),
            users: (*users >= MIN_GROUP_SIZE).then_some(*users),
            share_prev: ratio(
                prev.iter().find(|p| p.0 == *key).map_or(0, |p| p.1),
                total_prev,
            ),
        })
        .collect();
    // 部署×用途の列は多い用途から 6 つ（部署で絞っているときは出さない）。
    let columns: Vec<String> = categories.iter().take(6).map(|c| c.key.clone()).collect();
    let matrix = if s.dept.is_some() {
        Vec::new()
    } else {
        matrix(db, s, &columns).await?
    };
    let last_classified_at: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "SELECT max(classified_at) FROM thread_insight WHERE tenant_id = $1 AND org = $2",
    )
    .bind(&s.tenant_id)
    .bind(&s.org)
    .fetch_one(db)
    .await?;
    let (top_skills, top_miniapps, top_workflows) = tops(db, s).await?;
    Ok(UseCases {
        classify_enabled: settings.classify_enabled,
        last_classified_at,
        total_threads: total,
        matrix_labels: columns
            .iter()
            .map(|c| category_label(c).to_string())
            .collect(),
        matrix_columns: columns,
        categories,
        matrix,
        active_users: super::active_users(db, s).await?,
        features: features(db, s).await?,
        top_skills,
        top_miniapps,
        top_workflows,
    })
}

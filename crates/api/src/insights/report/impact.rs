//! 効果（#751）: 推定削減時間・満足度・👎 の理由・成果物・品質の兆候・費用対効果。
//!
//! 削減時間は**前提にもとづく推定値**: 用途ごとの「完了した会話」×「人がやると何分か」
//! （管理者の設定）と、ワークフローの成功した実行 ×「1 回で浮く分数」。完了とは、その期間に
//! 正常に終わった生成があり、👎 が付いていない会話。

use serde::Serialize;
use sqlx::PgPool;
use utoipa::ToSchema;

use super::{ratio, ReportError, Scope, DEPT_FILTER};
use crate::insights::{category_label, settings::InsightSettings, CATEGORIES};

const RANGE: &str = "($3::date)::timestamp AT TIME ZONE 'Asia/Tokyo'";
const RANGE_END: &str = "($4::date + 1)::timestamp AT TIME ZONE 'Asia/Tokyo'";

/// 削減時間の 1 行。
#[derive(Debug, Serialize, ToSchema)]
pub struct SavingRow {
    pub key: String,
    pub label: String,
    /// 完了した会話（ワークフローは成功した実行）。
    pub completed: i64,
    /// 前提（分/件）。
    pub minutes: i32,
    pub hours: f64,
}

/// 満足度の 1 行。
#[derive(Debug, Serialize, ToSchema)]
pub struct SatisfactionRow {
    pub key: String,
    pub label: String,
    pub up: i64,
    pub down: i64,
    pub rate: Option<f64>,
}

/// 👎 の理由。
#[derive(Debug, Serialize, ToSchema)]
pub struct ReasonRow {
    pub key: String,
    pub label: String,
    pub count: i64,
}

/// 成果物。
#[derive(Debug, Serialize, ToSchema)]
pub struct Outputs {
    /// AI が作成・更新した文書（回答の文書カード）。
    pub documents: i64,
    /// 作られたスキル。
    pub skills_created: i64,
    /// 作られたワークフロー。
    pub workflows_created: i64,
}

/// 品質の兆候。
#[derive(Debug, Serialize, ToSchema)]
pub struct Quality {
    pub runs: i64,
    /// 失敗した生成の割合。
    pub failed_rate: Option<f64>,
    /// 途中で止めた割合。
    pub cancelled_rate: Option<f64>,
    /// 評価のうち 👎 の割合。
    pub down_rate: Option<f64>,
}

/// 効果。
#[derive(Debug, Serialize, ToSchema)]
pub struct Impact {
    pub classify_enabled: bool,
    pub hourly_wage_jpy: Option<i32>,
    pub workflow_minutes: i32,
    /// 用途ごと（分類がオフなら空）。
    pub savings: Vec<SavingRow>,
    pub workflow: SavingRow,
    pub total_hours: f64,
    pub yen_saved: Option<i64>,
    /// 用途別の満足度（分類がオフなら空）。
    pub satisfaction: Vec<SatisfactionRow>,
    /// モデル別の満足度。
    pub by_model: Vec<SatisfactionRow>,
    pub ratings: i64,
    pub satisfaction_overall: Option<f64>,
    pub reasons: Vec<ReasonRow>,
    pub outputs: Outputs,
    pub quality: Quality,
    /// AI の利用料（利用量画面と同じ会計）。
    pub cost_usd_micros: i64,
    /// 円換算（利用量画面の円換算レートが設定されていれば）。
    pub cost_jpy: Option<i64>,
    /// 削減時間の金額 ÷ 利用料（どちらも円で出せるときだけ）。
    pub return_ratio: Option<f64>,
}

/// 👎 の理由の表示名。
fn reason_label(key: &str) -> &str {
    match key {
        "inaccurate" => "内容が不正確",
        "not_found" => "社内の情報が見つからない",
        "not_followed" => "指示どおりでない",
        "slow" => "遅い",
        "how_to" => "やり方がわからなかった",
        "other" => "その他",
        _ => "理由なし",
    }
}

#[allow(clippy::cast_precision_loss)]
fn hours(completed: i64, minutes: i32) -> f64 {
    completed as f64 * f64::from(minutes) / 60.0
}

async fn completed_by_category(db: &PgPool, s: &Scope) -> Result<Vec<(String, i64)>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT ti.category, count(*) FROM thread_insight ti \
         WHERE ti.tenant_id = $1 AND ti.org = $2 \
           AND EXISTS (SELECT 1 FROM generation_run g WHERE g.thread_id = ti.thread_id \
                        AND g.status = 'done' AND g.created_at >= {RANGE} \
                        AND g.created_at < {RANGE_END}) \
           AND NOT EXISTS (SELECT 1 FROM message_feedback f \
                            WHERE f.thread_id = ti.thread_id AND f.rating = -1) \
           AND {} \
         GROUP BY 1",
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

/// 評価を key（用途 or モデル）ごとに数える。
async fn feedback_by(
    db: &PgPool,
    s: &Scope,
    key_sql: &str,
    join: &str,
) -> Result<Vec<(String, i64, i64)>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {key_sql}, count(*) FILTER (WHERE f.rating = 1), \
                count(*) FILTER (WHERE f.rating = -1) \
         FROM message_feedback f {join} \
         WHERE f.tenant_id = $1 AND f.org = $2 \
           AND f.created_at >= {RANGE} AND f.created_at < {RANGE_END} \
           AND {} \
         GROUP BY 1 ORDER BY 2 + 3 DESC",
        DEPT_FILTER.replace("user_id IN", "f.user_id IN")
    ))
    .bind(&s.tenant_id)
    .bind(&s.org)
    .bind(s.from)
    .bind(s.to)
    .bind(s.dept.as_deref())
    .fetch_all(db)
    .await
}

/// 👎 の理由ごとの件数。
async fn reasons(db: &PgPool, s: &Scope) -> Result<Vec<ReasonRow>, sqlx::Error> {
    let rows: Vec<(Option<String>, i64)> = sqlx::query_as(&format!(
        "SELECT category, count(*) FROM message_feedback \
         WHERE tenant_id = $1 AND org = $2 AND rating = -1 \
           AND created_at >= {RANGE} AND created_at < {RANGE_END} AND {DEPT_FILTER} \
         GROUP BY 1 ORDER BY 2 DESC"
    ))
    .bind(&s.tenant_id)
    .bind(&s.org)
    .bind(s.from)
    .bind(s.to)
    .bind(s.dept.as_deref())
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(key, count)| {
            let key = key.unwrap_or_else(|| "none".into());
            ReasonRow {
                label: reason_label(&key).to_string(),
                key,
                count,
            }
        })
        .collect())
}

/// 期間内に作られた (スキル, ワークフロー) の数。
async fn created_artifacts(db: &PgPool, s: &Scope) -> Result<(i64, i64), sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT count(*) FILTER (WHERE kind = 'skill'), count(*) FILTER (WHERE kind = 'workflow') \
         FROM artifact \
         WHERE tenant_id = $1 AND org = $2 AND deleted_at IS NULL \
           AND created_at >= {RANGE} AND created_at < {RANGE_END} AND {}",
        DEPT_FILTER.replace("user_id IN", "owner IN")
    ))
    .bind(&s.tenant_id)
    .bind(&s.org)
    .bind(s.from)
    .bind(s.to)
    .bind(s.dept.as_deref())
    .fetch_one(db)
    .await
}

/// 効果を組み立てる。
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
pub async fn impact(
    db: &PgPool,
    s: &Scope,
    settings: &InsightSettings,
    jpy_per_usd_centi: Option<i32>,
) -> Result<Impact, ReportError> {
    super::check_scope(db, s).await?;
    let done = completed_by_category(db, s).await?;
    let savings: Vec<SavingRow> = if settings.classify_enabled {
        CATEGORIES
            .iter()
            .map(|(key, label, _)| {
                let completed = done.iter().find(|(k, _)| k == key).map_or(0, |(_, n)| *n);
                let minutes = settings.minutes_for(key);
                SavingRow {
                    key: (*key).to_string(),
                    label: (*label).to_string(),
                    completed,
                    minutes,
                    hours: hours(completed, minutes),
                }
            })
            .filter(|r| r.completed > 0)
            .collect()
    } else {
        Vec::new()
    };
    let (wf_ok, documents, runs, failed, cancelled, cost): (i64, i64, i64, i64, i64, i64) =
        sqlx::query_as(&format!(
            "SELECT COALESCE(sum(workflow_ok), 0)::bigint, COALESCE(sum(documents), 0)::bigint, \
                    COALESCE(sum(runs), 0)::bigint, COALESCE(sum(failed_runs), 0)::bigint, \
                    COALESCE(sum(cancelled_runs), 0)::bigint, \
                    COALESCE(sum(cost_usd_micros), 0)::bigint \
             FROM ai_activity_daily \
             WHERE tenant_id = $1 AND org = $2 AND day BETWEEN $3 AND $4 AND {DEPT_FILTER}"
        ))
        .bind(&s.tenant_id)
        .bind(&s.org)
        .bind(s.from)
        .bind(s.to)
        .bind(s.dept.as_deref())
        .fetch_one(db)
        .await?;
    let cost = if s.dept.is_some() {
        cost
    } else {
        org_cost(db, s).await?
    };
    let workflow = SavingRow {
        key: "workflow".into(),
        label: "ワークフロー（自動化）".into(),
        completed: wf_ok,
        minutes: settings.workflow_minutes,
        hours: hours(wf_ok, settings.workflow_minutes),
    };
    let total_hours = savings.iter().map(|r| r.hours).sum::<f64>() + workflow.hours;
    let yen_saved = settings
        .hourly_wage_jpy
        .map(|w| (total_hours * f64::from(w)).round() as i64);

    let to_rows = |rows: Vec<(String, i64, i64)>, label: &dyn Fn(&str) -> String| {
        rows.into_iter()
            .map(|(key, up, down)| SatisfactionRow {
                label: label(&key),
                key,
                up,
                down,
                rate: ratio(up, up + down),
            })
            .collect::<Vec<_>>()
    };
    let satisfaction = if settings.classify_enabled {
        to_rows(
            feedback_by(
                db,
                s,
                "ti.category",
                "JOIN thread_insight ti ON ti.tenant_id = f.tenant_id AND ti.thread_id = f.thread_id",
            )
            .await?,
            &|k| category_label(k).to_string(),
        )
    } else {
        Vec::new()
    };
    let by_model = to_rows(
        feedback_by(db, s, "COALESCE(f.model, '（不明）')", "").await?,
        &str::to_string,
    );
    let (up, down) = super::overview::ratings(db, s).await?;
    let reasons = reasons(db, s).await?;
    let (skills_created, workflows_created) = created_artifacts(db, s).await?;
    let cost_jpy = jpy_per_usd_centi
        .map(|centi| (cost as f64 / 1_000_000.0 * f64::from(centi) / 100.0).round() as i64);
    let return_ratio = match (yen_saved, cost_jpy) {
        (Some(y), Some(c)) if c > 0 => Some(y as f64 / c as f64),
        _ => None,
    };
    Ok(Impact {
        classify_enabled: settings.classify_enabled,
        hourly_wage_jpy: settings.hourly_wage_jpy,
        workflow_minutes: settings.workflow_minutes,
        savings,
        workflow,
        total_hours,
        yen_saved,
        satisfaction,
        by_model,
        ratings: up + down,
        satisfaction_overall: ratio(up, up + down),
        reasons,
        outputs: Outputs {
            documents,
            skills_created,
            workflows_created,
        },
        quality: Quality {
            runs,
            failed_rate: ratio(failed, runs),
            cancelled_rate: ratio(cancelled, runs),
            down_rate: ratio(down, up + down),
        },
        cost_usd_micros: cost,
        cost_jpy,
        return_ratio,
    })
}

/// 組織全体の利用料（利用量画面と同じ会計・llm_usage）。部署で絞っていないときに使う。
/// ロールアップは在籍者の行しか持たないので、分類器や予約起動のワークフローの分が落ちる。
async fn org_cost(db: &PgPool, s: &Scope) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT COALESCE(sum(cost_usd_micros), 0)::bigint FROM llm_usage \
         WHERE tenant_id = $1 AND org = $2 \
           AND created_at >= ($3::date)::timestamp AT TIME ZONE 'Asia/Tokyo' \
           AND created_at < ($4::date + 1)::timestamp AT TIME ZONE 'Asia/Tokyo'",
    )
    .bind(&s.tenant_id)
    .bind(&s.org)
    .bind(s.from)
    .bind(s.to)
    .fetch_one(db)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hours_and_labels() {
        assert!((hours(120, 15) - 30.0).abs() < 1e-9);
        assert_eq!(reason_label("how_to"), "やり方がわからなかった");
        assert_eq!(reason_label("none"), "理由なし");
    }
}

//! 概要（#751）: 利用者・利用率・定着率・推定削減時間・満足度・週ごとの推移・ファネル・部署・用途。

use chrono::{Duration, NaiveDate};
use serde::Serialize;
use sqlx::PgPool;
use utoipa::ToSchema;

use super::{
    active_users, check_scope, departments, dept_label, guarded, population, ratio, ReportError,
    Scope, DEPT_FILTER,
};
use crate::insights::settings::InsightSettings;

/// 週ごとの利用者（その週に使った日数で区分）。
#[derive(Debug, Serialize, ToSchema)]
pub struct WeekBucket {
    /// 週の初日（月曜・日本時間）。
    pub week_start: NaiveDate,
    /// 4 日以上使った人。
    pub heavy: i64,
    /// 2〜3 日。
    pub regular: i64,
    /// 1 日。
    pub light: i64,
}

/// 浸透のファネル（期間の終わりを基準にした人数）。
#[derive(Debug, Serialize, ToSchema)]
pub struct Funnel {
    /// 対象者（ディレクトリの在籍者）。
    pub population: i64,
    /// 一度でも使った（集計の残っている範囲）。
    pub ever: i64,
    /// 直近 30 日に使った。
    pub last_30_days: i64,
    /// 直近 4 週のうち 3 週以上使った。
    pub weekly: i64,
    /// 直近 28 日に 16 日以上使った（ヘビー）。
    pub heavy: i64,
}

/// 部署の利用率（少人数は伏せる）。
#[derive(Debug, Serialize, ToSchema)]
pub struct DeptRate {
    pub role_id: String,
    pub label: String,
    pub members: i64,
    /// 期間内に使った人 ÷ 部署の人数（5 人未満は `null`）。
    pub rate: Option<f64>,
}

/// 用途の割合。
#[derive(Debug, Serialize, ToSchema)]
pub struct UseShare {
    pub key: String,
    pub label: String,
    pub share: f64,
}

/// 概要。
#[derive(Debug, Serialize, ToSchema)]
pub struct Overview {
    pub population: i64,
    pub active_users: i64,
    pub active_users_prev: i64,
    /// 利用率（期間内に使った人 ÷ 対象者）。
    pub adoption_rate: Option<f64>,
    pub adoption_rate_prev: Option<f64>,
    /// 直近 7 日 / 直近 30 日に使った人（期間の終わり基準）。
    pub wau: i64,
    pub mau: i64,
    /// 定着率（WAU ÷ MAU）。
    pub stickiness: Option<f64>,
    /// 満足度（👍 ÷ 評価数）と評価数。
    pub satisfaction: Option<f64>,
    pub ratings: i64,
    pub satisfaction_prev: Option<f64>,
    /// 推定削減時間（時間）と金額換算（円・時給が設定されていれば）。
    pub hours_saved: f64,
    pub yen_saved: Option<i64>,
    pub weekly: Vec<WeekBucket>,
    pub funnel: Funnel,
    pub departments: Vec<DeptRate>,
    /// 用途の上位（分類がオフなら空）。
    pub top_uses: Vec<UseShare>,
    pub classify_enabled: bool,
    /// 集計値から規則で出す気づき（短い文）。
    pub highlights: Vec<String>,
}

async fn weekly(db: &PgPool, s: &Scope) -> Result<Vec<WeekBucket>, sqlx::Error> {
    let rows: Vec<(NaiveDate, i64, i64, i64)> = sqlx::query_as(&format!(
        "WITH d AS ( \
           SELECT date_trunc('week', day)::date AS wk, user_id, count(DISTINCT day) AS days \
           FROM ai_activity_daily \
           WHERE tenant_id = $1 AND org = $2 AND day BETWEEN $3 AND $4 AND {DEPT_FILTER} \
           GROUP BY 1, 2) \
         SELECT wk, count(*) FILTER (WHERE days >= 4), \
                count(*) FILTER (WHERE days BETWEEN 2 AND 3), count(*) FILTER (WHERE days = 1) \
         FROM d GROUP BY wk ORDER BY wk"
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
        .map(|(week_start, heavy, regular, light)| WeekBucket {
            week_start,
            heavy,
            regular,
            light,
        })
        .collect())
}

/// 期間の終わりを基準にした人数（`ever` は終わりまでの全期間）。
pub(crate) async fn funnel_counts(
    db: &PgPool,
    s: &Scope,
    population: i64,
) -> Result<Funnel, sqlx::Error> {
    let (ever, last_30_days, weekly, heavy): (i64, i64, i64, i64) = sqlx::query_as(&format!(
        "WITH u AS ( \
           SELECT user_id, \
                  bool_or(day > $4 - 30) AS m, \
                  count(DISTINCT day) FILTER (WHERE day > $4 - 28) AS d28, \
                  count(DISTINCT (($4 - day) / 7)) FILTER (WHERE day > $4 - 28) AS w4 \
           FROM ai_activity_daily \
           WHERE tenant_id = $1 AND org = $2 AND day <= $4 AND $3::date IS NOT NULL \
             AND {DEPT_FILTER} \
           GROUP BY user_id) \
         SELECT count(*), count(*) FILTER (WHERE m), count(*) FILTER (WHERE w4 >= 3), \
                count(*) FILTER (WHERE d28 >= 16) \
         FROM u"
    ))
    .bind(&s.tenant_id)
    .bind(&s.org)
    .bind(s.from)
    .bind(s.to)
    .bind(s.dept.as_deref())
    .fetch_one(db)
    .await?;
    Ok(Funnel {
        population,
        ever,
        last_30_days,
        weekly,
        heavy,
    })
}

/// 評価（👍 / 👎）の数。
pub(crate) async fn ratings(db: &PgPool, s: &Scope) -> Result<(i64, i64), sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT count(*) FILTER (WHERE rating = 1), count(*) FILTER (WHERE rating = -1) \
         FROM message_feedback \
         WHERE tenant_id = $1 AND org = $2 \
           AND created_at >= ($3::date)::timestamp AT TIME ZONE 'Asia/Tokyo' \
           AND created_at < ($4::date + 1)::timestamp AT TIME ZONE 'Asia/Tokyo' \
           AND {DEPT_FILTER}"
    ))
    .bind(&s.tenant_id)
    .bind(&s.org)
    .bind(s.from)
    .bind(s.to)
    .bind(s.dept.as_deref())
    .fetch_one(db)
    .await
}

async fn dept_rates(db: &PgPool, s: &Scope) -> Result<Vec<(DeptRate, Option<f64>)>, sqlx::Error> {
    let actives = |scope: Scope| async move {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT r.role_id, count(DISTINCT a.user_id) \
             FROM directory_user_role r \
             JOIN ai_activity_daily a ON a.tenant_id = r.tenant_id AND a.org = r.org \
              AND a.user_id = r.user_id AND a.day BETWEEN $3 AND $4 \
             WHERE r.tenant_id = $1 AND r.org = $2 AND r.source = 'group' GROUP BY 1",
        )
        .bind(&scope.tenant_id)
        .bind(&scope.org)
        .bind(scope.from)
        .bind(scope.to)
        .fetch_all(db)
        .await?;
        Ok::<_, sqlx::Error>(rows)
    };
    let now = actives(s.clone()).await?;
    let prev = actives(s.previous()).await?;
    let find = |rows: &[(String, i64)], id: &str| {
        rows.iter().find(|(r, _)| r == id).map_or(0, |(_, n)| *n)
    };
    Ok(departments(db, &s.tenant_id, &s.org)
        .await?
        .into_iter()
        .map(|(role_id, display, members)| {
            let rate = ratio(find(&now, &role_id), members).and_then(|r| guarded(members, r));
            let prev_rate = ratio(find(&prev, &role_id), members).and_then(|r| guarded(members, r));
            (
                DeptRate {
                    label: dept_label(&role_id, &display),
                    role_id,
                    members,
                    rate,
                },
                prev_rate,
            )
        })
        .collect())
}

/// 規則で出す気づき（数字の裏付けがあるものだけ）。
#[allow(clippy::cast_possible_truncation)]
fn highlights(
    depts: &[(DeptRate, Option<f64>)],
    dormant: i64,
    weak_use: Option<(String, f64)>,
) -> Vec<String> {
    let mut out = Vec::new();
    let pt = |r: f64| (r * 100.0).round() as i64;
    if let Some((d, now, prev)) = depts
        .iter()
        .filter_map(|(d, p)| Some((d, d.rate?, (*p)?)))
        .max_by_key(|(_, now, prev)| (pt(*now) - pt(*prev)).abs())
    {
        let diff = pt(now) - pt(prev);
        if diff.abs() >= 10 {
            out.push(format!(
                "{} の利用率が前期比 {}{} pt（{}%）。",
                d.label,
                if diff > 0 { "+" } else { "−" },
                diff.abs(),
                pt(now)
            ));
        }
    }
    if let Some(d) = depts
        .iter()
        .filter_map(|(d, _)| Some((d, d.rate?)))
        .filter(|(_, r)| *r < 0.3)
        .min_by_key(|(_, r)| pt(*r))
        .map(|(d, _)| d)
    {
        out.push(format!(
            "{} の利用率が {}%（{} 人中）。研修や使い方の共有の候補です。",
            d.label,
            pt(d.rate.unwrap_or(0.0)),
            d.members
        ));
    }
    if let Some((label, rate)) = weak_use {
        out.push(format!("「{label}」の満足度が {}% と低めです。", pt(rate)));
    }
    if dormant > 0 {
        out.push(format!(
            "一度使ったあと 30 日以上使っていない人が {dormant} 人います。"
        ));
    }
    out
}

/// 概要を組み立てる。
pub async fn overview(
    db: &PgPool,
    s: &Scope,
    settings: &InsightSettings,
    jpy_per_usd_centi: Option<i32>,
) -> Result<Overview, ReportError> {
    check_scope(db, s).await?;
    let prev = s.previous();
    let population = population(db, s).await?;
    let active = active_users(db, s).await?;
    let active_prev = active_users(db, &prev).await?;
    let week = Scope {
        from: s.to - Duration::days(6),
        ..s.clone()
    };
    let month = Scope {
        from: s.to - Duration::days(29),
        ..s.clone()
    };
    let wau = active_users(db, &week).await?;
    let mau = active_users(db, &month).await?;
    let (up, down) = ratings(db, s).await?;
    let (up_p, down_p) = ratings(db, &prev).await?;
    let funnel = funnel_counts(db, s, population).await?;
    let impact = super::impact::impact(db, s, settings, jpy_per_usd_centi).await?;
    let usecases = super::usecases::category_counts(db, s).await?;
    let total_threads: i64 = usecases.iter().map(|c| c.1).sum();
    let top_uses = usecases
        .iter()
        .take(5)
        .filter_map(|(key, threads, _)| {
            Some(UseShare {
                key: key.clone(),
                label: crate::insights::category_label(key).to_string(),
                share: ratio(*threads, total_threads)?,
            })
        })
        .collect();
    let depts = dept_rates(db, s).await?;
    let weak_use = impact
        .satisfaction
        .iter()
        .filter(|r| r.up + r.down >= 20)
        .filter_map(|r| Some((r.label.clone(), r.rate?)))
        .filter(|(_, rate)| *rate < 0.75)
        .min_by(|a, b| a.1.total_cmp(&b.1));
    let dormant = funnel.ever - funnel.last_30_days;
    let highlights = highlights(&depts, dormant, weak_use);
    let mut departments: Vec<DeptRate> = depts.into_iter().map(|(d, _)| d).collect();
    departments.sort_by(|a, b| {
        b.rate
            .unwrap_or(-1.0)
            .total_cmp(&a.rate.unwrap_or(-1.0))
            .then_with(|| a.label.cmp(&b.label))
    });
    Ok(Overview {
        population,
        active_users: active,
        active_users_prev: active_prev,
        adoption_rate: ratio(active, population),
        adoption_rate_prev: ratio(active_prev, population),
        wau,
        mau,
        stickiness: ratio(wau, mau),
        satisfaction: ratio(up, up + down),
        ratings: up + down,
        satisfaction_prev: ratio(up_p, up_p + down_p),
        hours_saved: impact.total_hours,
        yen_saved: impact.yen_saved,
        weekly: weekly(db, s).await?,
        funnel,
        departments,
        top_uses,
        classify_enabled: settings.classify_enabled,
        highlights,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dept(label: &str, members: i64, rate: Option<f64>) -> DeptRate {
        DeptRate {
            role_id: label.into(),
            label: label.into(),
            members,
            rate,
        }
    }

    #[test]
    fn highlights_use_only_supported_numbers() {
        let depts = vec![
            (dept("営業部", 20, Some(0.8)), Some(0.6)),
            (dept("法務部", 10, Some(0.2)), Some(0.2)),
            (dept("広報室", 3, None), None),
        ];
        let h = highlights(&depts, 4, Some(("データ分析・表計算".into(), 0.68)));
        assert_eq!(h.len(), 4);
        assert!(h[0].contains("営業部") && h[0].contains("+20 pt"));
        assert!(h[1].contains("法務部") && h[1].contains("20%"));
        assert!(h[2].contains("68%"));
        assert!(h[3].contains("4 人"));
        // 変化が小さく、低い部署も無く、休眠もいなければ何も言わない。
        let calm = vec![(dept("営業部", 20, Some(0.8)), Some(0.78))];
        assert!(highlights(&calm, 0, None).is_empty());
    }
}

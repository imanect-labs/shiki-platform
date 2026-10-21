//! 組織の分析設定（`ai_insight_settings`・#751）。行が無ければ既定。

use std::collections::BTreeMap;

use authz::AuthContext;
use sqlx::PgPool;

use super::CATEGORIES;

/// 組織の分析設定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsightSettings {
    /// 会話の用途分類（本文を AI に渡す処理なので既定オフ）。
    pub classify_enabled: bool,
    /// 個人単位の表示（既定オフ）。
    pub individual_view: bool,
    /// 金額換算の時給（円）。`None` は換算しない。
    pub hourly_wage_jpy: Option<i32>,
    /// 用途ごとの「人がやると何分か」（設定値・無いカテゴリは既定値）。
    pub minutes_per_task: BTreeMap<String, i32>,
    /// ワークフロー 1 回の成功で浮く分数。
    pub workflow_minutes: i32,
}

impl Default for InsightSettings {
    fn default() -> Self {
        InsightSettings {
            classify_enabled: false,
            individual_view: false,
            hourly_wage_jpy: None,
            minutes_per_task: BTreeMap::new(),
            workflow_minutes: 3,
        }
    }
}

impl InsightSettings {
    /// 用途の前提時間（設定が無ければ既定値）。
    pub fn minutes_for(&self, category: &str) -> i32 {
        self.minutes_per_task
            .get(category)
            .copied()
            .unwrap_or_else(|| {
                CATEGORIES
                    .iter()
                    .find(|(k, _, _)| *k == category)
                    .map_or(5, |(_, _, m)| *m)
            })
    }
}

/// 設定の更新内容（`None` の項目は変えない）。
#[derive(Debug, Clone, Default)]
pub struct InsightSettingsPatch {
    pub classify_enabled: Option<bool>,
    pub individual_view: Option<bool>,
    /// `Some(None)` で換算をやめる。
    pub hourly_wage_jpy: Option<Option<i32>>,
    /// 指定したカテゴリだけ上書きする。
    pub minutes_per_task: BTreeMap<String, i32>,
    pub workflow_minutes: Option<i32>,
}

/// 検証エラー（API は 400 に写す）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SettingsError {
    #[error("不明な用途です: {0}")]
    UnknownCategory(String),
    #[error("前提時間は 0〜600 分で指定してください")]
    InvalidMinutes,
    #[error("時給は 1〜100,000 円で指定してください")]
    InvalidWage,
}

/// 更新後の設定を作って検証する（純粋関数）。
pub fn apply_patch(
    current: &InsightSettings,
    patch: &InsightSettingsPatch,
) -> Result<InsightSettings, SettingsError> {
    let mut next = current.clone();
    if let Some(v) = patch.classify_enabled {
        next.classify_enabled = v;
    }
    if let Some(v) = patch.individual_view {
        next.individual_view = v;
    }
    if let Some(w) = patch.hourly_wage_jpy {
        if w.is_some_and(|w| !(1..=100_000).contains(&w)) {
            return Err(SettingsError::InvalidWage);
        }
        next.hourly_wage_jpy = w;
    }
    for (k, m) in &patch.minutes_per_task {
        if !CATEGORIES.iter().any(|(c, _, _)| c == k) {
            return Err(SettingsError::UnknownCategory(k.clone()));
        }
        if !(0..=600).contains(m) {
            return Err(SettingsError::InvalidMinutes);
        }
        next.minutes_per_task.insert(k.clone(), *m);
    }
    if let Some(m) = patch.workflow_minutes {
        if !(0..=600).contains(&m) {
            return Err(SettingsError::InvalidMinutes);
        }
        next.workflow_minutes = m;
    }
    Ok(next)
}

type Row = (bool, bool, Option<i32>, serde_json::Value, i32);

/// 組織の設定を読む（行が無ければ既定）。
pub async fn load(db: &PgPool, tenant_id: &str, org: &str) -> Result<InsightSettings, sqlx::Error> {
    let row: Option<Row> = sqlx::query_as(
        "SELECT classify_enabled, individual_view, hourly_wage_jpy, minutes_per_task, \
                workflow_minutes \
         FROM ai_insight_settings WHERE tenant_id = $1 AND org = $2",
    )
    .bind(tenant_id)
    .bind(org)
    .fetch_optional(db)
    .await?;
    Ok(
        row.map_or_else(InsightSettings::default, |(c, i, w, m, wf)| {
            InsightSettings {
                classify_enabled: c,
                individual_view: i,
                hourly_wage_jpy: w,
                minutes_per_task: serde_json::from_value(m).unwrap_or_default(),
                workflow_minutes: wf,
            }
        }),
    )
}

/// 読む → 検証 → 保存を 1 トランザクションで行う（同時更新で変更が消えないよう行ロックで直列化）。
pub async fn update(
    db: &PgPool,
    ctx: &AuthContext,
    patch: &InsightSettingsPatch,
) -> Result<Result<InsightSettings, SettingsError>, sqlx::Error> {
    let mut tx = db.begin().await?;
    sqlx::query(
        "INSERT INTO ai_insight_settings (tenant_id, org) VALUES ($1, $2) \
         ON CONFLICT (tenant_id, org) DO NOTHING",
    )
    .bind(&ctx.tenant_id)
    .bind(&ctx.org)
    .execute(&mut *tx)
    .await?;
    let (classify, individual, wage, minutes, workflow): Row = sqlx::query_as(
        "SELECT classify_enabled, individual_view, hourly_wage_jpy, minutes_per_task, \
                workflow_minutes \
         FROM ai_insight_settings WHERE tenant_id = $1 AND org = $2 FOR UPDATE",
    )
    .bind(&ctx.tenant_id)
    .bind(&ctx.org)
    .fetch_one(&mut *tx)
    .await?;
    let current = InsightSettings {
        classify_enabled: classify,
        individual_view: individual,
        hourly_wage_jpy: wage,
        minutes_per_task: serde_json::from_value(minutes).unwrap_or_default(),
        workflow_minutes: workflow,
    };
    let next = match apply_patch(&current, patch) {
        Ok(n) => n,
        Err(e) => return Ok(Err(e)),
    };
    sqlx::query(
        "UPDATE ai_insight_settings SET classify_enabled = $3, individual_view = $4, \
                hourly_wage_jpy = $5, minutes_per_task = $6, workflow_minutes = $7, \
                updated_by = $8, updated_at = now() \
         WHERE tenant_id = $1 AND org = $2",
    )
    .bind(&ctx.tenant_id)
    .bind(&ctx.org)
    .bind(next.classify_enabled)
    .bind(next.individual_view)
    .bind(next.hourly_wage_jpy)
    .bind(serde_json::to_value(&next.minutes_per_task).unwrap_or_default())
    .bind(next.workflow_minutes)
    .bind(&ctx.principal.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Ok(next))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minutes_fall_back_to_category_defaults() {
        let mut s = InsightSettings::default();
        assert_eq!(s.minutes_for("writing"), 15);
        s.minutes_per_task.insert("writing".into(), 30);
        assert_eq!(s.minutes_for("writing"), 30);
        assert_eq!(s.minutes_for("ghost"), 5);
    }

    #[test]
    fn patch_validates_ranges_and_categories() {
        let base = InsightSettings::default();
        let bad = InsightSettingsPatch {
            minutes_per_task: [("ghost".to_string(), 3)].into(),
            ..Default::default()
        };
        assert_eq!(
            apply_patch(&base, &bad),
            Err(SettingsError::UnknownCategory("ghost".into()))
        );
        let bad = InsightSettingsPatch {
            hourly_wage_jpy: Some(Some(0)),
            ..Default::default()
        };
        assert_eq!(apply_patch(&base, &bad), Err(SettingsError::InvalidWage));
        let ok = InsightSettingsPatch {
            classify_enabled: Some(true),
            hourly_wage_jpy: Some(Some(2500)),
            minutes_per_task: [("summarize".to_string(), 20)].into(),
            workflow_minutes: Some(5),
            ..Default::default()
        };
        let next = apply_patch(&base, &ok).unwrap();
        assert!(next.classify_enabled && !next.individual_view);
        assert_eq!(next.hourly_wage_jpy, Some(2500));
        assert_eq!(next.minutes_for("summarize"), 20);
        assert_eq!(next.workflow_minutes, 5);
        // 換算をやめる。
        let off = InsightSettingsPatch {
            hourly_wage_jpy: Some(None),
            ..Default::default()
        };
        assert_eq!(apply_patch(&next, &off).unwrap().hourly_wage_jpy, None);
    }
}

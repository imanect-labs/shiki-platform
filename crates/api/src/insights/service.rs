//! AI 活用分析の窓口（#751）。管理 API はここだけを通す。
//!
//! `AppState` に生の `PgPool` を出さない方針（`state::ReadinessProbe`）に合わせ、集計は
//! このサービスに閉じる。範囲（tenant_id ＋ org）は**常に `AuthContext` から**取り、
//! リクエストからは受け取らない（アンビエント権限の禁止）。管理者であることの確認は
//! ルート表のポリシー（`AccessPolicy::OrgAdmin`）が済ませている。

use authz::AuthContext;
use chrono::NaiveDate;
use sqlx::PgPool;

use super::report::{self, ReportError, Scope};
use super::settings::{self, InsightSettings, InsightSettingsPatch, SettingsError};

/// 集計の窓口。
pub struct InsightsService {
    db: PgPool,
}

/// 期間と部署の指定。
#[derive(Debug, Clone)]
pub struct Range {
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub dept: Option<String>,
}

impl InsightsService {
    pub fn new(db: PgPool) -> Self {
        InsightsService { db }
    }

    /// 次の定期実行で、この組織の集計を遡ってやり直させる（新しい在籍者の過去の利用を入れる）。
    /// best-effort（失敗しても次の登録・手動の再集計で埋まる）。
    pub async fn request_backfill(&self, tenant_id: &str, org: &str) {
        if let Err(e) = super::rollup::request_backfill(&self.db, tenant_id, org).await {
            tracing::warn!(error = %e, "分析の遡り集計の予約に失敗");
        }
    }

    fn scope(ctx: &AuthContext, r: &Range) -> Scope {
        Scope {
            tenant_id: ctx.tenant_id.clone(),
            org: ctx.org.clone(),
            from: r.from,
            to: r.to,
            dept: r.dept.clone().filter(|d| !d.is_empty()),
        }
    }

    pub async fn settings(&self, ctx: &AuthContext) -> Result<InsightSettings, sqlx::Error> {
        settings::load(&self.db, &ctx.tenant_id, &ctx.org).await
    }

    pub async fn update_settings(
        &self,
        ctx: &AuthContext,
        patch: &InsightSettingsPatch,
    ) -> Result<Result<InsightSettings, SettingsError>, sqlx::Error> {
        settings::update(&self.db, ctx, patch).await
    }

    pub async fn overview(
        &self,
        ctx: &AuthContext,
        r: &Range,
        jpy_per_usd_centi: Option<i32>,
    ) -> Result<report::overview::Overview, ReportError> {
        let s = self.settings(ctx).await?;
        report::overview::overview(&self.db, &Self::scope(ctx, r), &s, jpy_per_usd_centi).await
    }

    pub async fn adoption(
        &self,
        ctx: &AuthContext,
        r: &Range,
    ) -> Result<report::adoption::Adoption, ReportError> {
        let s = self.settings(ctx).await?;
        report::adoption::adoption(&self.db, &Self::scope(ctx, r), s.individual_view).await
    }

    /// 個人別。設定がオフなら `Ok(None)`（呼び出し側が 403 にする）。
    pub async fn people(
        &self,
        ctx: &AuthContext,
        r: &Range,
    ) -> Result<Option<Vec<report::adoption::PersonRow>>, ReportError> {
        if !self.settings(ctx).await?.individual_view {
            return Ok(None);
        }
        report::adoption::people(&self.db, &Self::scope(ctx, r))
            .await
            .map(Some)
    }

    pub async fn use_cases(
        &self,
        ctx: &AuthContext,
        r: &Range,
    ) -> Result<report::usecases::UseCases, ReportError> {
        let s = self.settings(ctx).await?;
        report::usecases::use_cases(&self.db, &Self::scope(ctx, r), &s).await
    }

    pub async fn impact(
        &self,
        ctx: &AuthContext,
        r: &Range,
        jpy_per_usd_centi: Option<i32>,
    ) -> Result<report::impact::Impact, ReportError> {
        let s = self.settings(ctx).await?;
        report::impact::impact(&self.db, &Self::scope(ctx, r), &s, jpy_per_usd_centi).await
    }

    /// 部署の一覧（絞り込みの選択肢・少人数の部署は選べない印つき）。
    pub async fn departments(
        &self,
        ctx: &AuthContext,
    ) -> Result<Vec<(String, String, i64)>, sqlx::Error> {
        let rows = report::departments(&self.db, &ctx.tenant_id, &ctx.org).await?;
        Ok(rows
            .into_iter()
            .map(|(id, display, members)| {
                let label = report::dept_label(&id, &display);
                (id, label, members)
            })
            .collect())
    }
}

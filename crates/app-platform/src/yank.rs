//! コードアプリの公開の取り下げ（yank・D-203・GAP-69b）。
//!
//! 取り下げると、新しいインストールが止まり（[`InstallService::install`] は取り下げた版を 409 にする）、組織の公開
//! アプリの一覧（[`crate::Registry::list_installable`]）から外れる。登録の行は消さない（公開した版を書き換えない・
//! migration 0038 の unique と、skill_installation の外部キーを守る）。入れたアプリは動き続ける（ゲートウェイは
//! registry_entry を読まない）。意味は crates.io の yank と同じで、動いているものを止めるのはアンインストール。
//!
//! 取り下げられるのは、アプリ（mini_app_code のアーティファクト）の持ち主と、登録した組織の管理者
//! （`organization#admin`）。今ある relation を確かめるだけで、OpenFGA のモデルは変えない。skill の登録は扱わない
//! （D-203）。

use std::collections::HashMap;

use authz::{AuthContext, CheckTuple, Consistency, Relation};
use storage::audit::{record_on, AuditAction, AuditEntry, Chain, Decision};
use uuid::Uuid;

use crate::{map_db, AppPlatformError, InstallService, RegistryEntry};

/// 取り下げの口が扱う登録の種類（コードアプリだけ・skill は扱わない・D-203）。
const KIND: &str = "mini_app_code";

impl InstallService {
    /// 呼ぶ人の組織が公開したコードアプリの登録を取り下げる（D-203・GAP-69b）。
    ///
    /// 1. 登録を (テナント・組織・id・`mini_app_code`) で引く。ほかの組織の行と skill の行は NotFound（404）。
    /// 2. アプリの持ち主か、組織の管理者かを確かめる（剥奪の直後に通さないよう `HigherConsistency`）。どちらでも
    ///    なければ `app.yank` の deny を監査に残して Forbidden（403）。
    /// 3. 1 つの Tx で、まだ取り下げていなければ取り下げ、変えたときだけ同じ Tx で `app.yank` を監査のチェーンに
    ///    つなぐ（対象は公開の監査と同じ `miniapp` のアプリの id・metadata は name と version）。
    ///
    /// この呼び出しで取り下げたら `true`、もう取り下げていたら `false`（どちらも成功・2 回目は監査を残さない）。
    pub async fn yank(
        &self,
        ctx: &AuthContext,
        entry_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<bool, AppPlatformError> {
        let entry = self
            .registry
            .find_in_org(ctx, KIND, entry_id)
            .await?
            .ok_or(AppPlatformError::NotFound)?;
        if !self.may_yank(ctx, entry.artifact_id).await? {
            self.audit_deny(ctx, entry.artifact_id, AuditAction::AppYank, trace_id)
                .await;
            return Err(AppPlatformError::Forbidden);
        }
        let mut tx = self.registry.begin().await?;
        let changed = self.registry.yank_on(&mut tx, ctx, KIND, entry.id).await?;
        if changed {
            record_on(
                &mut tx,
                ctx,
                AuditEntry {
                    action: AuditAction::AppYank,
                    object_type: "miniapp",
                    object_id: &entry.artifact_id.to_string(),
                    decision: Decision::Allow,
                    trace_id,
                    metadata: serde_json::json!({
                        "name": entry.name,
                        "version": entry.version,
                    }),
                },
                Chain::Yes,
            )
            .await
            .map_err(|e| AppPlatformError::Internal(format!("audit: {e}")))?;
        }
        tx.commit().await.map_err(map_db)?;
        Ok(changed)
    }

    /// 一覧の各項目を、呼ぶ人が取り下げられるか（`entries` と同じ順・画面の出し分けだけ）。
    ///
    /// 組織の管理者なら 1 回の check で全部 `true`。そうでなければ、アプリの持ち主かを batch_check でまとめて引く
    /// （同じアプリの版は 1 問にまとめる）。コードアプリ以外の項目は `false`。表示のための判定なので
    /// `MinimizeLatency` で引き、実際の取り下げは [`Self::yank`] が確かめ直す。
    pub async fn yankable(
        &self,
        ctx: &AuthContext,
        entries: &[RegistryEntry],
    ) -> Result<Vec<bool>, AppPlatformError> {
        let is_code = |e: &RegistryEntry| e.artifact_kind == KIND;
        if !entries.iter().any(is_code) {
            return Ok(vec![false; entries.len()]);
        }
        if self.is_org_admin(ctx, Consistency::MinimizeLatency).await? {
            return Ok(entries.iter().map(is_code).collect());
        }
        let mut apps: Vec<Uuid> = entries
            .iter()
            .filter(|e| is_code(e))
            .map(|e| e.artifact_id)
            .collect();
        apps.sort_unstable();
        apps.dedup();
        let items: Vec<CheckTuple> = apps
            .iter()
            .map(|id| CheckTuple {
                subject: ctx.subject(),
                relation: Relation::Owner,
                object: ctx.ns().artifact(&id.to_string()),
            })
            .collect();
        let owned: HashMap<Uuid, bool> = apps
            .into_iter()
            .zip(
                self.authz
                    .batch_check(&items, Consistency::MinimizeLatency)
                    .await
                    .map_err(authz_error)?,
            )
            .collect();
        Ok(entries
            .iter()
            .map(|e| is_code(e) && owned.get(&e.artifact_id).copied().unwrap_or(false))
            .collect())
    }

    /// 取り下げてよいか: アプリの持ち主か、組織の管理者か（[`Self::yank`] の判定・`HigherConsistency`）。
    async fn may_yank(&self, ctx: &AuthContext, app_id: Uuid) -> Result<bool, AppPlatformError> {
        let owner = self
            .authz
            .check(
                &ctx.subject(),
                Relation::Owner,
                &ctx.ns().artifact(&app_id.to_string()),
                Consistency::HigherConsistency,
            )
            .await
            .map_err(authz_error)?;
        if owner {
            return Ok(true);
        }
        self.is_org_admin(ctx, Consistency::HigherConsistency).await
    }

    /// 呼ぶ人が自分の組織（`ctx.org()`）の管理者か。
    async fn is_org_admin(
        &self,
        ctx: &AuthContext,
        consistency: Consistency,
    ) -> Result<bool, AppPlatformError> {
        self.authz
            .check(
                &ctx.subject(),
                Relation::Admin,
                &ctx.ns().organization(ctx.org()),
                consistency,
            )
            .await
            .map_err(authz_error)
    }
}

#[allow(clippy::needless_pass_by_value)]
fn authz_error(e: authz::AuthzError) -> AppPlatformError {
    AppPlatformError::Internal(format!("authz: {e}"))
}

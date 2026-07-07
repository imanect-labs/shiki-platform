//! 実行主体・委譲モデル（Task 10.4a・engine.md §6/§10・FR-12 最重要・confused-deputy 防御）。
//!
//! 委譲 = 対象オブジェクトへの通常 relation タプル（subject = workflow プリンシパル）＋
//! `workflow_delegation` 行での台帳管理（human 承認済み・engine.md §6.6）。
//!
//! - 有効化（[`enable`](DelegationStore::enable)）: 有効化者が自分の権限範囲内から (object, relation) を
//!   明示委譲する。範囲外が 1 つでも混じれば**全体拒否**（fail-closed・部分委譲しない）。
//! - run 開始時チェック（[`check_run_start`](DelegationStore::check_run_start)）: 3 条件を満たさなければ
//!   run を開始せず registration を `suspended_reconsent` へ（黙って動き続けない）。
//! - 棚卸し（[`inventory`](DelegationStore::inventory)）: 委譲者の失権を検知しタプル撤去＋停止（二段目）。

use std::sync::Arc;

use authz::{AuthContext, AuthzClient, Consistency, FgaObject, Namespace, Relation};
use sqlx::PgPool;
use uuid::Uuid;

/// 委譲 1 件の付与要求（有効化者が選んだ (object, relation)）。
#[derive(Debug, Clone)]
pub struct GrantRequest {
    /// 委譲する declared_scope（例: `storage.read`）。
    pub scope: String,
    /// 対象 FGA オブジェクト（`Namespace` 由来・例: `folder:<tenant>|<id>`）。
    pub object: FgaObject,
    /// 付与する relation（viewer/editor/can_use 等）。
    pub relation: Relation,
}

/// run 開始可否の判定結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunAdmission {
    /// 開始可。
    Ok,
    /// 委譲不成立で開始不可（registration は suspended_reconsent 済み）。
    DelegationInvalid(String),
}

/// 委譲操作のエラー。
#[derive(Debug, thiserror::Error)]
pub enum DelegationError {
    #[error("有効化者の権限範囲外の委譲が含まれます: {0}")]
    OutOfScope(String),
    #[error("registration が見つかりません")]
    NotRegistered,
    #[error("認可エラー: {0}")]
    Authz(String),
    #[error("内部エラー: {0}")]
    Internal(String),
}

#[allow(clippy::needless_pass_by_value)]
fn map_db(e: sqlx::Error) -> DelegationError {
    DelegationError::Internal(format!("db: {e}"))
}

/// 委譲・登録の管理（authz クライアント＋DB）。
#[derive(Clone)]
pub struct DelegationStore {
    db: PgPool,
    authz: Arc<dyn AuthzClient>,
}

impl DelegationStore {
    pub fn new(db: PgPool, authz: Arc<dyn AuthzClient>) -> Self {
        DelegationStore { db, authz }
    }

    /// ワークフローを有効化し委譲を付与する（engine.md §10.1・全体 fail-closed）。
    ///
    /// 手順: ①各 grant について有効化者が当該 relation を**現に持つか**を check（1 つでも欠ければ
    /// 全体拒否）→ ②workflow プリンシパルへ FGA タプル書込＋delegation 行記録＋registration 更新。
    pub async fn enable(
        &self,
        enabler: &AuthContext,
        workflow_id: Uuid,
        version: i64,
        declared_scopes: &[String],
        grants: &[GrantRequest],
    ) -> Result<(), DelegationError> {
        let tenant = &enabler.tenant_id;
        // ① 範囲検証: 有効化者が全 grant の relation を持つか（1 つでも欠ければ全体拒否）。
        for g in grants {
            let ok = self
                .authz
                .check(
                    &enabler.subject(),
                    g.relation,
                    &g.object,
                    Consistency::HigherConsistency,
                )
                .await
                .map_err(|e| DelegationError::Authz(e.to_string()))?;
            if !ok {
                return Err(DelegationError::OutOfScope(format!(
                    "{} on {}",
                    g.relation.as_str(),
                    g.object.as_str()
                )));
            }
        }

        // ② 付与。FGA タプルを**先に**書き（失敗したら DB は未コミットのまま return＝all-or-nothing）、
        //    その後 DB を単一 TX でコミットする。コミット済み registration は必ずタプルを伴う。
        let wf_subject = enabler.ns().workflow_principal(&workflow_id.to_string());
        for g in grants {
            self.authz
                .write_tuple(&wf_subject, g.relation, &g.object)
                .await
                .map_err(|e| DelegationError::Authz(e.to_string()))?;
        }

        // 再有効化で外れた委譲（新 grant 集合に無い既存 active 行）を撤去する。旧タプルを残すと
        // ユーザーが同意から外したオブジェクトへワークフローが到達し続ける（P1）。
        let new_objs: std::collections::BTreeSet<&str> =
            grants.iter().map(|g| g.object.as_str()).collect();
        let stale = self.active_delegations(tenant, workflow_id).await?;
        for d in &stale {
            if new_objs.contains(d.object_ref.as_str()) {
                continue;
            }
            if let Some(relation) = Relation::parse(&d.relation) {
                let obj = FgaObject::from_qualified(&d.object_ref);
                self.authz
                    .delete_tuple(&wf_subject, relation, &obj)
                    .await
                    .map_err(|e| DelegationError::Authz(e.to_string()))?;
            }
        }

        let mut tx = self.db.begin().await.map_err(map_db)?;
        sqlx::query(
            "INSERT INTO workflow_registration \
             (tenant_id, workflow_id, org, status, enabled_version, consented_scopes, enabled_by) \
             VALUES ($1, $2, $3, 'enabled', $4, $5, $6) \
             ON CONFLICT (tenant_id, workflow_id) DO UPDATE SET \
               status = 'enabled', enabled_version = $4, consented_scopes = $5, \
               enabled_by = $6, updated_at = now()",
        )
        .bind(tenant)
        .bind(workflow_id)
        .bind(&enabler.org)
        .bind(version)
        .bind(declared_scopes)
        .bind(&enabler.principal.id)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;

        // 外れた委譲行を revoke。
        sqlx::query(
            "UPDATE workflow_delegation SET revoked_at = now() \
             WHERE tenant_id = $1 AND workflow_id = $2 AND revoked_at IS NULL \
               AND NOT (object_ref = ANY($3))",
        )
        .bind(tenant)
        .bind(workflow_id)
        .bind(new_objs.iter().copied().collect::<Vec<&str>>())
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;

        for g in grants {
            sqlx::query(
                "INSERT INTO workflow_delegation \
                 (tenant_id, workflow_id, delegator, scope, object_ref, relation) \
                 VALUES ($1, $2, $3, $4, $5, $6) \
                 ON CONFLICT (tenant_id, workflow_id, delegator, scope, object_ref) \
                 DO UPDATE SET relation = $6, revoked_at = NULL, granted_at = now()",
            )
            .bind(tenant)
            .bind(workflow_id)
            .bind(&enabler.principal.id)
            .bind(&g.scope)
            .bind(g.object.as_str())
            .bind(g.relation.as_str())
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
        }
        tx.commit().await.map_err(map_db)?;
        Ok(())
    }

    /// run 開始時の委譲チェック（engine.md §6.2・fail-closed）。
    ///
    /// 3 条件（registration enabled・委譲有効・declared ⊆ consented）を満たさなければ
    /// registration を `suspended_reconsent` にして `DelegationInvalid` を返す。
    pub async fn check_run_start(
        &self,
        tenant_id: &str,
        workflow_id: Uuid,
        declared_scopes: &[String],
    ) -> Result<RunAdmission, DelegationError> {
        // 1. registration が enabled か＋consented_scopes を得る。
        let row: Option<(String, Vec<String>)> = sqlx::query_as(
            "SELECT status, consented_scopes FROM workflow_registration \
             WHERE tenant_id = $1 AND workflow_id = $2",
        )
        .bind(tenant_id)
        .bind(workflow_id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        let Some((status, consented)) = row else {
            return Ok(RunAdmission::DelegationInvalid("未登録".into()));
        };
        if status != "enabled" {
            return Ok(RunAdmission::DelegationInvalid(format!("status={status}")));
        }
        // 3. declared_scopes ⊆ consented_scopes。
        if let Some(missing) = declared_scopes.iter().find(|s| !consented.contains(s)) {
            self.suspend(tenant_id, workflow_id).await?;
            return Ok(RunAdmission::DelegationInvalid(format!(
                "scope {missing} が未同意"
            )));
        }
        // 2. 委譲有効: 各委譲について委譲者が今も relation を持つか（PIT-34 の最低線）。
        let delegations = self.active_delegations(tenant_id, workflow_id).await?;
        for d in &delegations {
            let subject = Namespace::for_tenant(tenant_id).user(&d.delegator);
            let obj = FgaObject::from_qualified(&d.object_ref);
            let relation = Relation::parse(&d.relation)
                .ok_or_else(|| DelegationError::Internal(format!("bad relation {}", d.relation)))?;
            let ok = self
                .authz
                .check(&subject, relation, &obj, Consistency::HigherConsistency)
                .await
                .map_err(|e| DelegationError::Authz(e.to_string()))?;
            if !ok {
                // 失権を検知 → 停止（次回実行は開始されず再同意要求）。
                self.suspend(tenant_id, workflow_id).await?;
                return Ok(RunAdmission::DelegationInvalid(format!(
                    "委譲者 {} が {} on {} を失権",
                    d.delegator,
                    relation.as_str(),
                    d.object_ref
                )));
            }
        }
        Ok(RunAdmission::Ok)
    }

    /// 棚卸しジョブ（engine.md §6.3・二段目）: 委譲者失権を検知しタプル撤去＋停止。
    ///
    /// 失権を検出したワークフロー id を返す（呼び出し側が実行中 run のキャンセルを起動する）。
    pub async fn inventory(&self, tenant_id: &str) -> Result<Vec<Uuid>, DelegationError> {
        let delegations = self.all_active_delegations(tenant_id).await?;
        let mut revoked_workflows: Vec<Uuid> = Vec::new();
        for d in &delegations {
            let subject = Namespace::for_tenant(tenant_id).user(&d.delegator);
            let obj = FgaObject::from_qualified(&d.object_ref);
            let Some(relation) = Relation::parse(&d.relation) else {
                continue;
            };
            let ok = self
                .authz
                .check(&subject, relation, &obj, Consistency::HigherConsistency)
                .await
                .map_err(|e| DelegationError::Authz(e.to_string()))?;
            if !ok {
                // 失権: workflow プリンシパルの FGA タプルを撤去。
                let wf_subject =
                    Namespace::for_tenant(tenant_id).workflow_principal(&d.workflow_id.to_string());
                // タプル撤去が失敗したら**行を revoke しない**（次回 inventory で再試行）。ここで
                // 握り潰して revoke すると、live なままのタプルが以後の inventory から漏れ、
                // fail-closed 撤去が破れる（P1）。撤去成功まで suspend だけは先に効かせる。
                if let Err(e) = self.authz.delete_tuple(&wf_subject, relation, &obj).await {
                    self.suspend(tenant_id, d.workflow_id).await?;
                    tracing::warn!(error = %e, workflow = %d.workflow_id, "委譲タプル撤去に失敗（次回再試行）");
                    if !revoked_workflows.contains(&d.workflow_id) {
                        revoked_workflows.push(d.workflow_id);
                    }
                    continue;
                }
                // delegation 行を revoke・registration を suspend。
                sqlx::query(
                    "UPDATE workflow_delegation SET revoked_at = now() \
                     WHERE tenant_id = $1 AND workflow_id = $2 AND delegator = $3 \
                       AND scope = $4 AND object_ref = $5",
                )
                .bind(tenant_id)
                .bind(d.workflow_id)
                .bind(&d.delegator)
                .bind(&d.scope)
                .bind(&d.object_ref)
                .execute(&self.db)
                .await
                .map_err(map_db)?;
                self.suspend(tenant_id, d.workflow_id).await?;
                if !revoked_workflows.contains(&d.workflow_id) {
                    revoked_workflows.push(d.workflow_id);
                }
            }
        }
        Ok(revoked_workflows)
    }

    /// registration を `suspended_reconsent` にする（失権検知時）。
    async fn suspend(&self, tenant_id: &str, workflow_id: Uuid) -> Result<(), DelegationError> {
        sqlx::query(
            "UPDATE workflow_registration SET status = 'suspended_reconsent', updated_at = now() \
             WHERE tenant_id = $1 AND workflow_id = $2",
        )
        .bind(tenant_id)
        .bind(workflow_id)
        .execute(&self.db)
        .await
        .map_err(map_db)?;
        Ok(())
    }

    async fn active_delegations(
        &self,
        tenant_id: &str,
        workflow_id: Uuid,
    ) -> Result<Vec<DelegationRow>, DelegationError> {
        sqlx::query_as::<_, DelegationRow>(
            "SELECT workflow_id, delegator, scope, object_ref, relation FROM workflow_delegation \
             WHERE tenant_id = $1 AND workflow_id = $2 AND revoked_at IS NULL",
        )
        .bind(tenant_id)
        .bind(workflow_id)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)
    }

    async fn all_active_delegations(
        &self,
        tenant_id: &str,
    ) -> Result<Vec<DelegationRow>, DelegationError> {
        sqlx::query_as::<_, DelegationRow>(
            "SELECT workflow_id, delegator, scope, object_ref, relation FROM workflow_delegation \
             WHERE tenant_id = $1 AND revoked_at IS NULL",
        )
        .bind(tenant_id)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct DelegationRow {
    workflow_id: Uuid,
    delegator: String,
    scope: String,
    object_ref: String,
    relation: String,
}

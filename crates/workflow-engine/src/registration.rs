//! 有効化・同意・トリガ実体化（Task 10.4a 残・engine.md §10・ir.md §9）。
//!
//! [`RegistrationService`] が「enable = 単一論理操作」を提供する:
//! FGA タプル（[`DelegationStore`] 経由・TX 外先行）→ **単一 DB TX** で registration 更新＋
//! IR `triggers[]` からの `workflow_trigger` 実体化。enable コミット済みなのにトリガ未実体化、
//! という中間状態を作らない。
//!
//! バージョン切替規則（ir.md §9・fail-closed）:
//! - **scope 拡大**（新版 declared ⊄ 現 consented）は grants 必須。無ければ
//!   [`EnableError::ScopeExpansion`]（API 層が 409 ＋ missing_scopes で返す）。
//! - **縮小/同一のみ**は grants 省略で軽量切替（既存委譲を維持し enabled_version とトリガのみ更新）。

use sqlx::PgPool;
use uuid::Uuid;

use authz::AuthContext;

use crate::delegation::{DelegationError, DelegationStore, GrantRequest};
use crate::ir::{Trigger, WorkflowIr};

/// 有効化操作のエラー。
#[derive(Debug, thiserror::Error)]
pub enum EnableError {
    /// scope 拡大なのに grants が無い（再同意が必要・API は 409）。
    #[error("scope が拡大しています（再同意が必要）: {missing:?}")]
    ScopeExpansion { missing: Vec<String> },
    #[error(transparent)]
    Delegation(#[from] DelegationError),
    #[error("内部エラー: {0}")]
    Internal(String),
}

#[allow(clippy::needless_pass_by_value)]
fn map_db(e: sqlx::Error) -> EnableError {
    EnableError::Internal(format!("db: {e}"))
}

/// 委譲 1 件の表示ビュー。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DelegationView {
    pub delegator: String,
    pub scope: String,
    pub object_ref: String,
    pub relation: String,
    pub granted_at: chrono::DateTime<chrono::Utc>,
}

/// registration の表示ビュー（UI の有効化状態・再同意バナー用）。
#[derive(Debug, Clone)]
pub struct RegistrationView {
    /// enabled / disabled / suspended_reconsent / none（未登録）。
    pub status: String,
    pub enabled_version: Option<i64>,
    pub consented_scopes: Vec<String>,
    pub enabled_by: Option<String>,
    pub delegations: Vec<DelegationView>,
}

/// 同意画面の提案 grant（IR の静的分析から列挙・最終選択は有効化者）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuggestedGrant {
    /// 対応する declared_scope。
    pub scope: String,
    /// 対象の種類（folder / file / secret / workflow）。
    pub object_kind: String,
    /// IR から確定できた対象 id（リテラル参照時のみ）。
    pub object_id: Option<String>,
    /// 対象の参照名（secret の name・workflow.start の name 等・API 層で id 解決）。
    pub object_name: Option<String>,
    /// 付与する relation（viewer / editor / can_use）。
    pub relation: String,
    /// 提案の根拠（`node:<id>` / `trigger:<index>`）。
    pub source: String,
    /// 対象をユーザーが選ぶ必要があるか（rag の検索範囲・非リテラル参照）。
    pub needs_user_pick: bool,
}

/// 有効化・無効化・同意計画の単一入口。
#[derive(Clone)]
pub struct RegistrationService {
    db: PgPool,
    delegation: DelegationStore,
}

impl RegistrationService {
    pub fn new(db: PgPool, delegation: DelegationStore) -> Self {
        RegistrationService { db, delegation }
    }

    /// registration の現況（未登録は status = "none"）。
    #[allow(clippy::type_complexity)]
    pub async fn view(
        &self,
        tenant_id: &str,
        workflow_id: Uuid,
    ) -> Result<RegistrationView, EnableError> {
        let row: Option<(String, Option<i64>, Vec<String>, Option<String>)> = sqlx::query_as(
            "SELECT status, enabled_version, consented_scopes, enabled_by \
             FROM workflow_registration WHERE tenant_id = $1 AND workflow_id = $2",
        )
        .bind(tenant_id)
        .bind(workflow_id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        let Some((status, enabled_version, consented_scopes, enabled_by)) = row else {
            return Ok(RegistrationView {
                status: "none".into(),
                enabled_version: None,
                consented_scopes: Vec::new(),
                enabled_by: None,
                delegations: Vec::new(),
            });
        };
        let delegations: Vec<DelegationView> = sqlx::query_as(
            "SELECT delegator, scope, object_ref, relation, granted_at FROM workflow_delegation \
             WHERE tenant_id = $1 AND workflow_id = $2 AND revoked_at IS NULL \
             ORDER BY scope, object_ref",
        )
        .bind(tenant_id)
        .bind(workflow_id)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(RegistrationView {
            status,
            enabled_version,
            consented_scopes,
            enabled_by,
            delegations,
        })
    }

    /// ワークフローを version 単位で有効化する（単一論理操作・engine.md §10.1）。
    ///
    /// - grants あり（または未登録）: 委譲の全面再同意（[`DelegationStore::enable`] 相当）＋
    ///   トリガ実体化を単一 TX で行う。
    /// - grants なし＋登録済み: **軽量切替**。新版 declared ⊆ 現 consented のときのみ
    ///   既存委譲を維持して enabled_version とトリガだけ更新する（拡大は `ScopeExpansion`）。
    pub async fn enable(
        &self,
        enabler: &AuthContext,
        workflow_id: Uuid,
        version: i64,
        ir: &WorkflowIr,
        grants: &[GrantRequest],
    ) -> Result<(), EnableError> {
        let tenant = &enabler.tenant_id;
        let current = self.view(tenant, workflow_id).await?;
        let registered = current.status != "none";

        if grants.is_empty() && registered {
            // 軽量切替: 拡大が無いことを検証（fail-closed・ir.md §9）。
            let missing: Vec<String> = ir
                .declared_scopes
                .iter()
                .filter(|s| !current.consented_scopes.contains(s))
                .cloned()
                .collect();
            if !missing.is_empty() {
                return Err(EnableError::ScopeExpansion { missing });
            }
            let mut tx = self.db.begin().await.map_err(map_db)?;
            sqlx::query(
                "UPDATE workflow_registration SET status = 'enabled', enabled_version = $3, \
                 updated_at = now() WHERE tenant_id = $1 AND workflow_id = $2",
            )
            .bind(tenant)
            .bind(workflow_id)
            .bind(version)
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
            materialize_triggers(&mut tx, tenant, workflow_id, version, ir).await?;
            tx.commit().await.map_err(map_db)?;
            return Ok(());
        }

        // 全面（再）同意: FGA（TX 外先行）＋ registration/delegation ＋ トリガ実体化を単一 TX で。
        let mut tx = self.db.begin().await.map_err(map_db)?;
        self.delegation
            .enable_within(
                &mut tx,
                enabler,
                workflow_id,
                version,
                &ir.declared_scopes,
                grants,
            )
            .await?;
        materialize_triggers(&mut tx, tenant, workflow_id, version, ir).await?;
        tx.commit().await.map_err(map_db)?;
        Ok(())
    }

    /// 無効化（トリガ停止＋status=disabled・単一 TX）。
    ///
    /// 委譲タプルは撤去しない（再有効化で再同意なく戻せる。run 開始は status で fail-closed に
    /// 止まるため権限は行使されない）。完全な委譲撤去は grants を空にした再有効化→無効化で行う。
    pub async fn disable(&self, tenant_id: &str, workflow_id: Uuid) -> Result<(), EnableError> {
        let mut tx = self.db.begin().await.map_err(map_db)?;
        sqlx::query(
            "UPDATE workflow_registration SET status = 'disabled', updated_at = now() \
             WHERE tenant_id = $1 AND workflow_id = $2",
        )
        .bind(tenant_id)
        .bind(workflow_id)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        sqlx::query(
            "UPDATE workflow_trigger SET enabled = false \
             WHERE tenant_id = $1 AND workflow_id = $2",
        )
        .bind(tenant_id)
        .bind(workflow_id)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        tx.commit().await.map_err(map_db)?;
        Ok(())
    }

    /// 同意画面の提案 grants を IR から静的に列挙する（純関数・最終選択は有効化者）。
    #[must_use]
    pub fn consent_plan(ir: &WorkflowIr) -> Vec<SuggestedGrant> {
        let mut out: Vec<SuggestedGrant> = Vec::new();
        let mut push = |g: SuggestedGrant| {
            if !out.iter().any(|e| {
                e.scope == g.scope
                    && e.object_kind == g.object_kind
                    && e.object_id == g.object_id
                    && e.object_name == g.object_name
                    && e.relation == g.relation
            }) {
                out.push(g);
            }
        };

        // イベントトリガの folder 束縛: トリガ元コンテキストの読取提案（典型パターン）。
        for (i, t) in ir.triggers.iter().enumerate() {
            if let Trigger::Event(ev) = t {
                if let Some(folder) = ev.scope.get("folder").and_then(|v| v.as_str()) {
                    push(SuggestedGrant {
                        scope: "storage.read".into(),
                        object_kind: "folder".into(),
                        object_id: Some(folder.to_string()),
                        object_name: None,
                        relation: "viewer".into(),
                        source: format!("trigger:{i}"),
                        needs_user_pick: false,
                    });
                }
            }
        }

        for node in &ir.nodes {
            let source = format!("node:{}", node.id);
            let Some(nt) = crate::vocab::NodeType::parse(&node.node_type) else {
                continue;
            };
            match nt {
                crate::vocab::NodeType::StorageRead => {
                    let file = literal_str(&node.params, "file");
                    push(SuggestedGrant {
                        scope: "storage.read".into(),
                        object_kind: "file".into(),
                        object_id: file.clone(),
                        object_name: None,
                        relation: "viewer".into(),
                        source,
                        needs_user_pick: file.is_none(),
                    });
                }
                crate::vocab::NodeType::StorageList => {
                    let folder = literal_str(&node.params, "folder");
                    push(SuggestedGrant {
                        scope: "storage.read".into(),
                        object_kind: "folder".into(),
                        object_id: folder.clone(),
                        object_name: None,
                        relation: "viewer".into(),
                        source,
                        needs_user_pick: folder.is_none(),
                    });
                }
                crate::vocab::NodeType::StorageWrite => {
                    let folder = literal_str(&node.params, "folder");
                    push(SuggestedGrant {
                        scope: "storage.write".into(),
                        object_kind: "folder".into(),
                        object_id: folder.clone(),
                        object_name: None,
                        relation: "editor".into(),
                        source,
                        needs_user_pick: folder.is_none(),
                    });
                }
                crate::vocab::NodeType::RagSearch => {
                    // 検索範囲はユーザーが選ぶ（実効 = 委譲 ∩ pre/post filter）。
                    push(SuggestedGrant {
                        scope: "rag.query".into(),
                        object_kind: "folder".into(),
                        object_id: None,
                        object_name: None,
                        relation: "viewer".into(),
                        source,
                        needs_user_pick: true,
                    });
                }
                crate::vocab::NodeType::HttpRequest => {
                    if let Some(name) = node
                        .params
                        .get("secret")
                        .and_then(|s| s.get("name"))
                        .and_then(|v| v.as_str())
                    {
                        push(SuggestedGrant {
                            scope: "http.egress".into(),
                            object_kind: "secret".into(),
                            object_id: None,
                            object_name: Some(name.to_string()),
                            relation: "can_use".into(),
                            source,
                            needs_user_pick: false,
                        });
                    }
                }
                crate::vocab::NodeType::WorkflowStart => {
                    let name = literal_str(&node.params, "name");
                    push(SuggestedGrant {
                        scope: "workflow.start".into(),
                        object_kind: "workflow".into(),
                        object_id: None,
                        object_name: name.clone(),
                        relation: "viewer".into(),
                        source,
                        needs_user_pick: name.is_none(),
                    });
                }
                _ => {}
            }
        }
        out
    }
}

/// params のフィールドが文字列リテラルならその値（`$from`/`$template` は None）。
fn literal_str(params: &serde_json::Value, key: &str) -> Option<String> {
    match params.get(key) {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

/// IR の triggers[] を `workflow_trigger` 行へ実体化する（既存行は全削除→挿入・呼び出し側 TX）。
///
/// trigger_id は `<workflow_id>:<index>` の決定的 id（再有効化で安定・DELETE 先行で衝突しない）。
async fn materialize_triggers(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: &str,
    workflow_id: Uuid,
    version: i64,
    ir: &WorkflowIr,
) -> Result<(), EnableError> {
    sqlx::query("DELETE FROM workflow_trigger WHERE tenant_id = $1 AND workflow_id = $2")
        .bind(tenant_id)
        .bind(workflow_id)
        .execute(&mut **tx)
        .await
        .map_err(map_db)?;
    for (i, t) in ir.triggers.iter().enumerate() {
        let (kind, source, spec) = match t {
            Trigger::Schedule(sc) => (
                "schedule",
                None,
                serde_json::to_value(sc).map_err(|e| EnableError::Internal(e.to_string()))?,
            ),
            Trigger::Event(ev) => (
                "event",
                Some(ev.source.as_str()),
                serde_json::to_value(ev).map_err(|e| EnableError::Internal(e.to_string()))?,
            ),
            Trigger::Interactive(_) => ("interactive", None, serde_json::json!({})),
        };
        sqlx::query(
            "INSERT INTO workflow_trigger \
             (tenant_id, trigger_id, workflow_id, version, kind, source, spec, enabled) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, true)",
        )
        .bind(tenant_id)
        .bind(format!("{workflow_id}:{i}"))
        .bind(workflow_id)
        .bind(version)
        .bind(kind)
        .bind(source)
        .bind(sqlx::types::Json(spec))
        .execute(&mut **tx)
        .await
        .map_err(map_db)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ir(v: &serde_json::Value) -> WorkflowIr {
        WorkflowIr::from_json(v).expect("test IR")
    }

    #[test]
    fn consent_plan_collects_literal_objects_and_secret_names() {
        let ir = ir(&json!({
            "ir_version": 1, "name": "wf",
            "declared_scopes": ["storage.read", "storage.write", "http.egress"],
            "triggers": [
                { "kind": "event", "source": "storage.write",
                  "scope": { "folder": "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1" } }
            ],
            "nodes": [
                { "id": "rd", "type": "storage.read",
                  "params": { "file": { "$from": "trigger", "path": "/file_id" } } },
                { "id": "wr", "type": "storage.write",
                  "params": { "folder": "11111111-2222-3333-4444-555555555555",
                              "name": "out", "content": "x" } },
                { "id": "post", "type": "http.request",
                  "params": { "url": "https://api.example.com",
                              "secret": { "name": "slack-token" } } }
            ],
            "edges": [{ "from": "rd", "to": "wr" }, { "from": "wr", "to": "post" }]
        }));
        let plan = RegistrationService::consent_plan(&ir);
        // トリガ folder（リテラル確定）。
        assert!(plan.iter().any(|g| g.object_kind == "folder"
            && g.object_id.as_deref() == Some("8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1")
            && !g.needs_user_pick));
        // storage.read は非リテラル → user pick。
        assert!(plan
            .iter()
            .any(|g| g.source == "node:rd" && g.needs_user_pick));
        // storage.write folder リテラル → editor 提案。
        assert!(plan.iter().any(|g| g.source == "node:wr"
            && g.relation == "editor"
            && g.object_id.as_deref() == Some("11111111-2222-3333-4444-555555555555")));
        // secret は参照名で提案（id 解決は API 層）。
        assert!(plan.iter().any(|g| g.object_kind == "secret"
            && g.object_name.as_deref() == Some("slack-token")
            && g.relation == "can_use"));
    }

    #[test]
    fn consent_plan_dedupes_same_object() {
        let ir = ir(&json!({
            "ir_version": 1, "name": "wf",
            "declared_scopes": ["storage.read"],
            "nodes": [
                { "id": "a", "type": "storage.list",
                  "params": { "folder": "11111111-2222-3333-4444-555555555555" } },
                { "id": "b", "type": "storage.list",
                  "params": { "folder": "11111111-2222-3333-4444-555555555555" } }
            ],
            "edges": [{ "from": "a", "to": "b" }]
        }));
        let plan = RegistrationService::consent_plan(&ir);
        assert_eq!(
            plan.len(),
            1,
            "同一 (scope, object, relation) は 1 件: {plan:?}"
        );
    }
}

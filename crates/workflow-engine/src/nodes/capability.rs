//! 能力呼び出しの本体（storage / rag / workflow.start）。node 経路と script の `Shiki.*` 経路で共用。
//!
//! effect_journal（storage.write は in-TX・workflow.start は cross-TX）・監査はここで一点合流する。
//! 個別ノードに認可検査を書かせない（INV-2）。scope ceiling は [`exec`](super::exec) が事前に担保する。

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::capability::{op_digest, JournalDecision};
use crate::control::eval::resolve_value;
use crate::ir::params::{
    RagSearchParams, StorageListParams, StorageReadParams, StorageWriteParams, WorkflowStartParams,
};
use crate::run::NodeContext;

use super::exec::CapabilityNodeExecutor;
use super::ports::{ExecCtx, PortError, StorageWriteReq};
use super::resolver::{as_bytes, as_string, as_u32, as_uuid, ParamResolver};

/// params を typed struct として取り出す（保存済み IR は V1 済み・失敗は permanent 扱い）。
pub(super) fn parse_params<T: serde::de::DeserializeOwned>(raw: &Value) -> Result<T, PortError> {
    crate::ir::params::parse(raw).map_err(|e| PortError::invalid(format!("params が不正: {e}")))
}

/// バイト列の sha256（16 進）。storage.write の op_digest 素材。
fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

impl CapabilityNodeExecutor {
    /// 監査（成功/失敗）を 1 行記録する（meta に secret 平文・本文を載せない）。
    pub(super) fn audit(&self, tenant_id: &str, api: &str, allowed: bool, meta: &Value) {
        self.audit.record(tenant_id, api, allowed, meta);
    }

    /// 外部 API のレート制御（未設定なら常に通過）。false は `rate_limited`（attempt 非消費・retryable）。
    pub(super) async fn rate_check(&self, ec: &ExecCtx, api: &str) -> Result<(), PortError> {
        let Some(rl) = &self.ratelimit else {
            return Ok(());
        };
        let key = format!("{}:{}", ec.tenant_id, api);
        match rl.try_acquire(&key, self.ratelimit_cfg, 1).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(PortError::new(
                "rate_limited",
                "レート上限に達しました",
                true,
            )),
            Err(e) => Err(PortError::unavailable(format!("ratelimit: {e}"))),
        }
    }

    // --- storage ---------------------------------------------------------

    pub(super) async fn node_storage_read(
        &self,
        params: &Value,
        _ctx: &NodeContext,
        ec: &ExecCtx,
        r: &ParamResolver<'_>,
    ) -> Result<Value, PortError> {
        let p: StorageReadParams = parse_params(params)?;
        let raw = resolve_value(&p.file, r)
            .ok_or_else(|| PortError::invalid("storage.read: file が解決できません"))?;
        let file_id = as_uuid(&raw)
            .ok_or_else(|| PortError::invalid("storage.read: file が UUID ではありません"))?;
        let out = self.ports.storage_read(ec, file_id).await?;
        self.audit(
            &ec.tenant_id,
            "storage.read",
            true,
            &json!({ "file_id": file_id.to_string() }),
        );
        Ok(out)
    }

    pub(super) async fn node_storage_write(
        &self,
        params: &Value,
        ctx: &NodeContext,
        ec: &ExecCtx,
        r: &ParamResolver<'_>,
    ) -> Result<Value, PortError> {
        let p: StorageWriteParams = parse_params(params)?;
        let parent_id = p
            .folder
            .as_ref()
            .and_then(|e| resolve_value(e, r))
            .and_then(|v| as_uuid(&v));
        let name = resolve_value(&p.name, r)
            .and_then(|v| as_string(&v))
            .ok_or_else(|| PortError::invalid("storage.write: name が解決できません"))?;
        let bytes = resolve_value(&p.content, r)
            .map(|v| as_bytes(&v))
            .ok_or_else(|| PortError::invalid("storage.write: content が解決できません"))?;
        let content_type = p
            .content_type
            .as_ref()
            .and_then(|e| resolve_value(e, r))
            .and_then(|v| as_string(&v))
            .unwrap_or_else(|| "application/octet-stream".to_string());

        // op_digest は書込の同一性（親/名前/内容ハッシュ）で安定。冪等キーはステップ固定。
        let digest = op_digest(
            "storage.write",
            &json!({
                "parent": parent_id.map(|p| p.to_string()),
                "name": name,
                "content_sha256": sha256_hex(&bytes),
            }),
        );
        let out = self
            .ports
            .storage_write(
                ec,
                StorageWriteReq {
                    parent_id,
                    name: name.clone(),
                    bytes,
                    content_type,
                    idempotency_key: ctx.idempotency_key.clone(),
                    op_digest: digest,
                },
            )
            .await?;
        self.audit(
            &ec.tenant_id,
            "storage.write",
            true,
            &json!({ "name": name, "parent": parent_id.map(|p| p.to_string()) }),
        );
        Ok(out)
    }

    pub(super) async fn node_storage_list(
        &self,
        params: &Value,
        _ctx: &NodeContext,
        ec: &ExecCtx,
        r: &ParamResolver<'_>,
    ) -> Result<Value, PortError> {
        let p: StorageListParams = parse_params(params)?;
        let parent_id = p
            .folder
            .as_ref()
            .and_then(|e| resolve_value(e, r))
            .and_then(|v| as_uuid(&v));
        let out = self.ports.storage_list(ec, parent_id).await?;
        self.audit(
            &ec.tenant_id,
            "storage.list",
            true,
            &json!({ "parent": parent_id.map(|p| p.to_string()) }),
        );
        Ok(out)
    }

    // --- rag -------------------------------------------------------------

    pub(super) async fn node_rag_search(
        &self,
        params: &Value,
        _ctx: &NodeContext,
        ec: &ExecCtx,
        r: &ParamResolver<'_>,
    ) -> Result<Value, PortError> {
        let p: RagSearchParams = parse_params(params)?;
        let query = resolve_value(&p.query, r)
            .and_then(|v| as_string(&v))
            .ok_or_else(|| PortError::invalid("rag.search: query が解決できません"))?;
        let top_k = p
            .top_k
            .as_ref()
            .and_then(|e| resolve_value(e, r))
            .and_then(|v| as_u32(&v));
        let out = self.ports.rag_search(ec, &query, top_k).await?;
        // クエリ本文は監査に載せない（PII 混入回避）。件数のみ。
        let n = out
            .get("results")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        self.audit(&ec.tenant_id, "rag.search", true, &json!({ "results": n }));
        Ok(out)
    }

    // --- workflow.start（cross-TX effect_journal で start-once） ----------

    pub(super) async fn do_workflow_start(
        &self,
        ec: &ExecCtx,
        idempotency_key: &str,
        name: &str,
        input: &Value,
    ) -> Result<Value, PortError> {
        let digest = op_digest("workflow.start", &json!({ "name": name, "input": input }));
        match self
            .journal
            .check(&ec.tenant_id, idempotency_key, &digest)
            .await
            .map_err(|e| PortError::unavailable(format!("journal: {e}")))?
        {
            JournalDecision::Proceed => {
                let out = self.ports.workflow_start(ec, name, input).await?;
                self.journal
                    .record(&ec.tenant_id, idempotency_key, &digest, &out)
                    .await
                    .map_err(|e| PortError::unavailable(format!("journal record: {e}")))?;
                self.audit(
                    &ec.tenant_id,
                    "workflow.start",
                    true,
                    &json!({ "name": name }),
                );
                Ok(out)
            }
            JournalDecision::AlreadyDone(v) => Ok(v),
            JournalDecision::InProgress => Err(PortError::new(
                "effect_in_progress",
                "別ワーカーが起動処理中",
                true,
            )),
            JournalDecision::DigestMismatch => Err(PortError::new(
                "effect_conflict",
                "同一冪等キーで別の起動要求",
                false,
            )),
        }
    }

    pub(super) async fn node_workflow_start(
        &self,
        params: &Value,
        ctx: &NodeContext,
        ec: &ExecCtx,
        r: &ParamResolver<'_>,
    ) -> Result<Value, PortError> {
        let p: WorkflowStartParams = parse_params(params)?;
        let name = resolve_value(&p.name, r)
            .and_then(|v| as_string(&v))
            .ok_or_else(|| PortError::invalid("workflow.start: name が解決できません"))?;
        let input = p
            .input
            .as_ref()
            .and_then(|e| resolve_value(e, r))
            .unwrap_or(Value::Null);
        self.do_workflow_start(ec, &ctx.idempotency_key, &name, &input)
            .await
    }
}

//! 本番 `NodePorts` 実装（executor → 既存チョークポイント結線・Stage A W3）。
//!
//! [`workflow_engine::CapabilityNodeExecutor`] が能力ゲートウェイを通した後に呼ぶポートを、
//! StorageService / SearchService / LlmGateway / Sandbox / SecretStore / RunLauncher へ結線する。
//! `ExecCtx` から実行主体の `AuthContext`（interactive=user / schedule・event=workflow）を組み、
//! 各チョークポイント内の OpenFGA 認可を通す（scope ceiling は executor 側＝二重ゲート）。

use std::sync::Arc;

use async_trait::async_trait;
use authz::{AuthContext, Principal, PrincipalKind};
use futures::StreamExt;
use llm_gateway::{
    GenerateRequest, GenerationRecord, LlmGateway, Message, Role, StreamDelta, Usage,
};
use sandbox_client::{ExecEvent, ExecRequest, Sandbox, SandboxSpec};
use secrets::SecretStore;
use serde_json::{json, Value};
use sqlx::PgPool;
use storage::model::ChildSort;
use storage::StorageService;
use uuid::Uuid;
use workflow_engine::{
    CsvPatchReq, CsvWriteReq, ExecCtx, HttpSendReq, HttpSendResp, LlmInvokeReq, NodePorts,
    PortError, ResolvedSecretView, StorageWriteReq, WorkflowRunLauncher,
};

/// 本番ポート（AppState のチョークポイント参照を保持）。
pub struct ProdNodePorts {
    pub storage: Arc<StorageService>,
    pub search: Option<Arc<rag::SearchService>>,
    pub gateway: Arc<LlmGateway>,
    pub sandbox: Option<Arc<dyn Sandbox>>,
    /// コード実行系（agent_invoke）の隔離ティア（admin ポリシー・design §4.6）。
    pub sandbox_backend: sandbox_client::SandboxBackend,
    pub secrets: Option<Arc<SecretStore>>,
    /// CSV 表データ（csv.query/patch/write・隔離 DuckDB 経由の Task 11P.9）。
    pub tabular: Option<Arc<tabular::TabularService>>,
    pub launcher: WorkflowRunLauncher,
    /// http.request の外部送信クライアント（リダイレクト非追従の別クライアントも内部で使う）。
    pub http: reqwest::Client,
    /// workflow.start の名前解決に使う（artifact 名 → workflow_id）。
    pub db: PgPool,
}

/// `ExecCtx` から実行主体の `AuthContext` を組む（種別で subject を分ける）。
fn auth_ctx(ec: &ExecCtx) -> AuthContext {
    if ec.principal_kind == "workflow" {
        AuthContext::for_workflow(ec.tenant_id.clone(), ec.org.clone(), &ec.principal)
    } else {
        AuthContext::new(
            Principal {
                kind: PrincipalKind::User,
                id: ec.principal.clone(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some(ec.tenant_id.clone()),
            },
            ec.org.clone(),
            ec.tenant_id.clone(),
        )
    }
}

fn map_storage(e: storage::StorageError) -> PortError {
    use storage::StorageError as S;
    match e {
        S::Forbidden => PortError::forbidden("storage 権限なし"),
        S::NotFound => PortError::new("not_found", "対象が見つかりません", false),
        S::Conflict => PortError::new("conflict", "冪等衝突/競合", false),
        S::Invalid(m) => PortError::invalid(m),
        other => PortError::unavailable(format!("storage: {other}")),
    }
}

/// tabular エラー → PortError（認可拒否は fail-closed の forbidden・競合/クォータは非 retryable）。
fn map_tabular(e: tabular::TabularError) -> PortError {
    use tabular::TabularError as T;
    match e {
        T::Forbidden => PortError::forbidden("csv 権限なし"),
        T::Authz(_) => PortError::forbidden("csv 認可失敗"),
        T::Storage(s) => map_storage(s),
        T::NotFound(m) => PortError::new("not_found", format!("csv: {m}"), false),
        T::SqlRejected(m) => PortError::invalid(format!("csv SQL 拒否: {m}")),
        T::InvalidPatch(m) => PortError::invalid(format!("csv パッチ不正: {m}")),
        T::RevConflict { base, current } => PortError::new(
            "conflict",
            format!("csv 競合: base={base}, current={current}"),
            false,
        ),
        T::QuotaExceeded(m) => {
            PortError::new("quota_exceeded", format!("csv クォータ超過: {m}"), false)
        }
        T::Runner(m) => PortError::unavailable(format!("csv runner: {m}")),
        // ユーザー SQL の誤り＝入力不正（非 retryable。再試行しても直らない）。
        T::QueryFailed(m) => PortError::invalid(format!("csv クエリ失敗: {m}")),
        T::Internal(m) => PortError::unavailable(format!("csv internal: {m}")),
    }
}

#[async_trait]
impl NodePorts for ProdNodePorts {
    async fn storage_write(&self, ec: &ExecCtx, req: StorageWriteReq) -> Result<Value, PortError> {
        let ctx = auth_ctx(ec);
        self.storage
            .write_file_internal_idempotent(
                &ctx,
                req.parent_id,
                &req.name,
                &req.bytes,
                &req.content_type,
                &req.idempotency_key,
                &req.op_digest,
                ec.trace_id.as_deref(),
            )
            .await
            .map_err(map_storage)
    }

    async fn storage_read(&self, ec: &ExecCtx, file_id: Uuid) -> Result<Value, PortError> {
        let ctx = auth_ctx(ec);
        let (node, bytes) = self
            .storage
            .read_file_internal(&ctx, file_id, ec.trace_id.as_deref())
            .await
            .map_err(map_storage)?;
        Ok(json!({
            "id": node.id.to_string(),
            "name": node.name,
            "content_type": node.content_type,
            "size": node.size_bytes,
            "text": String::from_utf8_lossy(&bytes),
        }))
    }

    async fn storage_list(
        &self,
        ec: &ExecCtx,
        parent_id: Option<Uuid>,
    ) -> Result<Value, PortError> {
        let ctx = auth_ctx(ec);
        let page = self
            .storage
            .list_children(
                &ctx,
                parent_id,
                ChildSort::default(),
                None,
                100,
                ec.trace_id.as_deref(),
            )
            .await
            .map_err(map_storage)?;
        let items: Vec<Value> = page
            .items
            .iter()
            .map(|n| {
                json!({
                    "id": n.id.to_string(),
                    "name": n.name,
                    "kind": format!("{:?}", n.kind).to_lowercase(),
                    "size": n.size_bytes,
                })
            })
            .collect();
        Ok(json!({ "items": items }))
    }

    async fn rag_search(
        &self,
        ec: &ExecCtx,
        query: &str,
        top_k: Option<u32>,
    ) -> Result<Value, PortError> {
        let ctx = auth_ctx(ec);
        let search = self
            .search
            .as_ref()
            .ok_or_else(|| PortError::forbidden("rag が未構成です"))?;
        let out = search
            .search(
                &ctx,
                query,
                top_k,
                rag::SearchMode::Hybrid,
                None,
                ec.trace_id.as_deref(),
            )
            .await
            .map_err(|e| PortError::unavailable(format!("rag: {e}")))?;
        let results: Vec<Value> = out
            .results
            .iter()
            .map(|r| {
                json!({
                    "file_id": r.file_id.to_string(),
                    "file_name": r.file_name,
                    "content": r.content,
                    "score": r.score,
                })
            })
            .collect();
        Ok(json!({ "results": results }))
    }

    async fn llm_invoke(&self, ec: &ExecCtx, req: LlmInvokeReq) -> Result<Value, PortError> {
        let ctx = auth_ctx(ec);
        let mut greq = GenerateRequest::new(vec![Message::text(Role::User, req.prompt.clone())]);
        greq.model = req.model.clone();
        greq.system = req.system.clone();
        greq.max_tokens = req.max_tokens;
        let mut stream = self
            .gateway
            .stream(greq)
            .await
            .map_err(|e| PortError::unavailable(format!("llm: {e}")))?;
        let mut text = String::new();
        let mut usage = Usage {
            prompt_tokens: 0,
            completion_tokens: 0,
        };
        while let Some(item) = stream.next().await {
            match item.map_err(|e| PortError::unavailable(format!("llm stream: {e}")))? {
                StreamDelta::TextDelta { text: t } => text.push_str(&t),
                StreamDelta::Done { usage: u, .. } => usage = u,
                _ => {}
            }
        }
        let model = req
            .model
            .clone()
            .unwrap_or_else(|| self.gateway.default_model().to_string());
        // 生成記録（trace_id で監査 ↔ Langfuse ↔ OTel を束ねる・本文プレビューは短く）。
        self.gateway
            .record_generation(
                &ctx,
                &GenerationRecord {
                    idempotency_key: req.idempotency_key,
                    model: model.clone(),
                    usage,
                    trace_id: ec.trace_id.clone(),
                    input_preview: preview(&req.prompt),
                    output_preview: preview(&text),
                    app_id: None,
                },
            )
            .await;
        Ok(json!({ "text": text, "model": model }))
    }

    async fn agent_invoke(
        &self,
        ec: &ExecCtx,
        req: workflow_engine::AgentInvokeReq,
    ) -> Result<Value, PortError> {
        let sandbox = self
            .sandbox
            .as_ref()
            .ok_or_else(|| PortError::forbidden("sandbox が未構成です"))?;
        // capability 縮小のみ: egress 全遮断の code_interpreter spec（ノード設定で egress/権限は拡大不能）。
        // 隔離ティアだけは admin ポリシーに従う（wasm→gVisor は縮小方向＝より強い隔離なので不変条件を破らない）。
        let spec = SandboxSpec::code_interpreter(
            self.sandbox_backend,
            ec.tenant_id.clone(),
            ec.org.clone(),
            ec.principal.clone(),
        );
        let handle = sandbox
            .create(spec)
            .await
            .map_err(|e| PortError::unavailable(format!("sandbox create: {e}")))?;
        let exec = sandbox
            .exec(
                &handle,
                ExecRequest::Python {
                    code: req.code,
                    timeout_ms: req.timeout_ms,
                },
            )
            .await;
        // exec の失敗は空成功にせず伝播する（destroy してから返す）。
        let mut stream = match exec {
            Ok(s) => s,
            Err(e) => {
                let _ = sandbox.destroy(&handle).await;
                return Err(PortError::unavailable(format!("sandbox exec: {e}")));
            }
        };
        let mut stdout = String::new();
        let mut exit_code = None;
        while let Some(ev) = stream.next().await {
            match ev {
                Ok(ExecEvent::Stdout(b)) => stdout.push_str(&String::from_utf8_lossy(&b)),
                Ok(ExecEvent::Exited { code }) => exit_code = Some(code),
                Ok(ExecEvent::LimitExceeded { kind, .. }) => {
                    let _ = sandbox.destroy(&handle).await;
                    return Err(PortError::new(
                        "sandbox_limit",
                        format!("サンドボックス上限超過: {kind:?}"),
                        false,
                    ));
                }
                Err(e) => {
                    let _ = sandbox.destroy(&handle).await;
                    return Err(PortError::unavailable(format!("sandbox stream: {e}")));
                }
                _ => {}
            }
        }
        let _ = sandbox.destroy(&handle).await;
        Ok(json!({ "stdout": stdout, "exit_code": exit_code }))
    }

    async fn http_send(&self, _ec: &ExecCtx, req: HttpSendReq) -> Result<HttpSendResp, PortError> {
        // Stage A は常に非追従（executor が redirect を拒否する）。SSRF/内部 rebind を防ぐため
        // auto-follow は使わない（`follow_redirects` が来ても Policy::none）。
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| PortError::unavailable(format!("http client: {e}")))?;
        let method = reqwest::Method::from_bytes(req.method.as_bytes())
            .map_err(|_| PortError::invalid("不正な HTTP メソッド"))?;
        let mut builder = client.request(method, &req.url);
        for (k, v) in &req.headers {
            builder = builder.header(k, v);
        }
        if let Some(body) = req.body {
            builder = builder.body(body);
        }
        if let Some(ms) = req.timeout_ms {
            builder = builder.timeout(std::time::Duration::from_millis(ms));
        }
        let mut resp = builder
            .send()
            .await
            .map_err(|e| PortError::unavailable(format!("http 送信: {e}")))?;
        let status = resp.status().as_u16();
        // 本文は上限まで**ストリーミング読み**する（巨大レスポンスの全量バッファで OOM しない）。
        const MAX_BODY: usize = 8 * 1024 * 1024;
        let mut body = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| PortError::unavailable(format!("http 本文: {e}")))?
        {
            if body.len() + chunk.len() > MAX_BODY {
                body.extend_from_slice(&chunk[..MAX_BODY - body.len()]);
                break;
            }
            body.extend_from_slice(&chunk);
        }
        Ok(HttpSendResp { status, body })
    }

    async fn resolve_secret(
        &self,
        ec: &ExecCtx,
        name: &str,
    ) -> Result<ResolvedSecretView, PortError> {
        let ctx = auth_ctx(ec);
        let secrets = self
            .secrets
            .as_ref()
            .ok_or_else(|| PortError::forbidden("secrets が未構成です"))?;
        // 宛先束縛の host 照合は executor が allowed_hosts で行うため、ここでは None（can_use 認可＋監査は実施）。
        let resolved = secrets
            .resolve(&ctx, name, None, ec.trace_id.as_deref())
            .await
            .map_err(|e| PortError::forbidden(format!("secret 解決: {e}")))?;
        Ok(ResolvedSecretView {
            plaintext: resolved.plaintext,
            allowed_hosts: resolved.binding.hosts().to_vec(),
        })
    }

    async fn workflow_start(
        &self,
        ec: &ExecCtx,
        name: &str,
        input: &Value,
    ) -> Result<Value, PortError> {
        let ctx = auth_ctx(ec);
        // 名前 → workflow_id（artifact 名の一意性・tenant×kind 内）。
        let id: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM artifact \
             WHERE tenant_id = $1 AND kind = 'workflow' AND name = $2 AND deleted_at IS NULL",
        )
        .bind(&ec.tenant_id)
        .bind(name)
        .fetch_optional(&self.db)
        .await
        .map_err(|e| PortError::unavailable(format!("workflow 名前解決: {e}")))?;
        let Some(workflow_id) = id else {
            return Err(PortError::invalid(format!(
                "workflow が存在しません: {name}"
            )));
        };
        // 実行主体の権限で子 run を起動（fire-and-forget）。
        let run_id = self
            .launcher
            .start_interactive(&ctx, workflow_id, input)
            .await
            .map_err(|e| PortError::unavailable(format!("workflow.start: {e}")))?;
        Ok(json!({ "run_id": run_id.map(|r| r.to_string()) }))
    }

    async fn csv_query(&self, ec: &ExecCtx, file_id: Uuid, sql: &str) -> Result<Value, PortError> {
        let ctx = auth_ctx(ec);
        let tabular = self
            .tabular
            .as_ref()
            .ok_or_else(|| PortError::forbidden("tabular が未構成です"))?;
        let resp = tabular
            .query(&ctx, file_id, sql, ec.trace_id.as_deref())
            .await
            .map_err(map_tabular)?;
        let rows: Vec<Value> = resp
            .rows
            .iter()
            .map(|row| Value::Array(row.iter().map(|c| cell_to_json(c.as_deref())).collect()))
            .collect();
        Ok(json!({
            "columns": resp.columns,
            "column_types": resp.column_types,
            "rows": rows,
            "total_rows": resp.total_rows,
            "truncated": resp.truncated,
        }))
    }

    async fn csv_patch(&self, ec: &ExecCtx, req: CsvPatchReq) -> Result<Value, PortError> {
        let ctx = auth_ctx(ec);
        let tabular = self
            .tabular
            .as_ref()
            .ok_or_else(|| PortError::forbidden("tabular が未構成です"))?;
        // ops（JSON 配列）を tabular の PatchOp へ型変換する（不正は permanent の invalid）。
        let ops: Vec<tabular::PatchOp> = serde_json::from_value(req.ops)
            .map_err(|e| PortError::invalid(format!("csv.patch: ops が不正: {e}")))?;
        let applied = tabular
            .patch(
                &ctx,
                req.file_id,
                req.base_rev,
                &ops,
                ec.trace_id.as_deref(),
            )
            .await
            .map_err(map_tabular)?;
        Ok(json!({
            "node_id": applied.node_id.to_string(),
            "version": applied.version,
            "rows": applied.rows,
            "cols": applied.cols,
        }))
    }

    async fn csv_write(&self, ec: &ExecCtx, req: CsvWriteReq) -> Result<Value, PortError> {
        let ctx = auth_ctx(ec);
        let tabular = self
            .tabular
            .as_ref()
            .ok_or_else(|| PortError::forbidden("tabular が未構成です"))?;
        let saved = tabular
            .save_new(
                &ctx,
                req.parent_id,
                &req.name,
                &req.csv_bytes,
                ec.trace_id.as_deref(),
            )
            .await
            .map_err(map_tabular)?;
        Ok(json!({
            "node_id": saved.node_id.to_string(),
            "version": saved.version,
            "name": saved.name,
        }))
    }
}

/// CSV セル（NULL は None）を JSON 値へ（NULL → null・値 → 文字列）。
fn cell_to_json(cell: Option<&str>) -> Value {
    cell.map_or(Value::Null, |s| Value::String(s.to_string()))
}

/// 監査/記録用の短いプレビュー（先頭 200 文字・改行畳み）。
fn preview(s: &str) -> String {
    let flat: String = s.chars().take(200).collect();
    flat.replace('\n', " ")
}

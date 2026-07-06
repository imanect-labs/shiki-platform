//! gRPC サービス実装（sandbox-client 契約）。ゲスト由来入力を `validate` で弾き、出力上限・壁時計
//! デッドラインを強制する。バックエンドは差し替え可能（wasm 実 sidecar / FakeBackend）。

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures::stream::StreamExt;
use sandbox_client::pb;
use sandbox_client::server::SandboxService;
use sandbox_client::{
    ExecEvent, ExecRequest, LimitKind, SandboxError, SandboxLifetime, SandboxSpec,
};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

use crate::backend::Backend;
use crate::config::OrchestratorEnv;
use crate::registry::Registry;
use crate::validate;

/// gRPC サービス。バックエンド＋レジストリ＋ランタイム設定を束ねる。
pub struct SandboxSvc {
    backend: Arc<dyn Backend>,
    registry: Arc<Registry>,
    _env: OrchestratorEnv,
}

impl SandboxSvc {
    pub fn new(backend: Arc<dyn Backend>, registry: Arc<Registry>, env: OrchestratorEnv) -> Self {
        SandboxSvc {
            backend,
            registry,
            _env: env,
        }
    }
}

fn to_status(e: SandboxError) -> Status {
    match e {
        SandboxError::Invalid(m) => Status::invalid_argument(m),
        SandboxError::NotFound(m) => Status::not_found(m),
        SandboxError::Unimplemented(m) => Status::unimplemented(m),
        SandboxError::Unavailable(m) => Status::unavailable(m),
        SandboxError::Internal(m) => Status::internal(m),
    }
}

fn validate_err(e: &validate::ValidateError) -> Status {
    Status::invalid_argument(e.to_string())
}

fn limit_kind_to_pb(kind: LimitKind) -> i32 {
    pb::LimitKind::from(kind) as i32
}

type PbExecStream = Pin<Box<dyn futures::Stream<Item = Result<pb::ExecEvent, Status>> + Send>>;

#[tonic::async_trait]
impl SandboxService for SandboxSvc {
    async fn create(
        &self,
        request: Request<pb::CreateRequest>,
    ) -> Result<Response<pb::CreateResponse>, Status> {
        let spec_pb = request
            .into_inner()
            .spec
            .ok_or_else(|| Status::invalid_argument("spec missing"))?;
        let spec = SandboxSpec::try_from(spec_pb).map_err(to_status)?;
        // PIT-24: 隔離クラスを監査に残し、機微度ポリシ（現状 allow-all）を通す。
        validate::check_isolation(&spec).map_err(|e| validate_err(&e))?;
        tracing::info!(
            target: "sandbox_audit",
            tenant = %spec.tenant_id,
            backend = ?spec.backend,
            isolation = ?spec.backend.isolation_class(),
            "sandbox create"
        );
        let tenant_id = spec.tenant_id.clone();
        let ttl = match spec.lifetime {
            SandboxLifetime::Ephemeral { ttl_ms } if ttl_ms > 0 => Duration::from_millis(ttl_ms),
            SandboxLifetime::Ephemeral { .. } => {
                return Err(Status::invalid_argument("ephemeral ttl must be > 0"))
            }
            SandboxLifetime::Persistent => {
                return Err(Status::unimplemented("persistent sandboxes are post-alpha"))
            }
        };

        let instance = self.backend.create(spec).await.map_err(to_status)?;
        let id = uuid::Uuid::new_v4().to_string();
        self.registry
            .insert(id.clone(), instance, ttl, tenant_id)
            .await;
        Ok(Response::new(pb::CreateResponse { sandbox_id: id }))
    }

    type ExecStream = PbExecStream;

    async fn exec(
        &self,
        request: Request<pb::ExecRequest>,
    ) -> Result<Response<Self::ExecStream>, Status> {
        let req = request.into_inner();
        let instance = self
            .registry
            .get(&req.sandbox_id)
            .await
            .ok_or_else(|| Status::not_found("sandbox not found"))?;

        let sandbox_id = req.sandbox_id.clone();
        let exec = ExecRequest::try_from(req).map_err(to_status)?;
        match &exec {
            ExecRequest::Python { code, .. } => {
                validate::check_code(code).map_err(|e| validate_err(&e))?;
            }
            ExecRequest::Shell { cmd, .. } => {
                validate::check_shell(cmd).map_err(|e| validate_err(&e))?;
            }
        }

        let inner = instance.exec(exec).await.map_err(to_status)?;
        let stream = guarded_exec_stream(inner, Arc::clone(&self.registry), sandbox_id);
        Ok(Response::new(Box::pin(stream)))
    }

    async fn put_file(
        &self,
        request: Request<pb::PutFileRequest>,
    ) -> Result<Response<pb::PutFileResponse>, Status> {
        let req = request.into_inner();
        validate::check_file_size(req.content.len()).map_err(|e| validate_err(&e))?;
        let path = validate::normalize_workspace_path(&req.path).map_err(|e| validate_err(&e))?;
        let instance = self
            .registry
            .get(&req.sandbox_id)
            .await
            .ok_or_else(|| Status::not_found("sandbox not found"))?;
        instance
            .put_file(&path, req.content)
            .await
            .map_err(to_status)?;
        Ok(Response::new(pb::PutFileResponse {}))
    }

    async fn get_file(
        &self,
        request: Request<pb::GetFileRequest>,
    ) -> Result<Response<pb::GetFileResponse>, Status> {
        let req = request.into_inner();
        let path = validate::normalize_workspace_path(&req.path).map_err(|e| validate_err(&e))?;
        let instance = self
            .registry
            .get(&req.sandbox_id)
            .await
            .ok_or_else(|| Status::not_found("sandbox not found"))?;
        let content = instance.get_file(&path).await.map_err(to_status)?;
        validate::check_file_size(content.len()).map_err(|e| validate_err(&e))?;
        Ok(Response::new(pb::GetFileResponse { content }))
    }

    async fn list_dir(
        &self,
        request: Request<pb::ListDirRequest>,
    ) -> Result<Response<pb::ListDirResponse>, Status> {
        let req = request.into_inner();
        let path = validate::normalize_workspace_path(&req.path).map_err(|e| validate_err(&e))?;
        let instance = self
            .registry
            .get(&req.sandbox_id)
            .await
            .ok_or_else(|| Status::not_found("sandbox not found"))?;
        let mut entries = instance.list_dir(&path).await.map_err(to_status)?;
        entries.truncate(validate::MAX_DIR_ENTRIES);
        let entries = entries
            .into_iter()
            .map(|e| pb::DirEntry {
                name: e.name,
                is_dir: e.is_dir,
                size: e.size,
            })
            .collect();
        Ok(Response::new(pb::ListDirResponse { entries }))
    }

    async fn destroy(
        &self,
        request: Request<pb::DestroyRequest>,
    ) -> Result<Response<pb::DestroyResponse>, Status> {
        let req = request.into_inner();
        if let Some(instance) = self.registry.remove(&req.sandbox_id).await {
            instance.destroy().await.map_err(to_status)?;
        }
        Ok(Response::new(pb::DestroyResponse {}))
    }
}

/// exec ストリームに出力上限を強制し、上限超過時は sandbox を破棄する。
fn guarded_exec_stream(
    mut inner: futures::stream::BoxStream<'static, Result<ExecEvent, SandboxError>>,
    registry: Arc<Registry>,
    sandbox_id: String,
) -> PbExecStream {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<pb::ExecEvent, Status>>(64);
    tokio::spawn(async move {
        let mut output_bytes: usize = 0;
        while let Some(item) = inner.next().await {
            let ev = match item {
                Ok(ev) => ev,
                Err(e) => {
                    let _ = tx.send(Err(to_status(e))).await;
                    break;
                }
            };
            // 出力累積が上限を超えたら打ち切り＋破棄。
            if let ExecEvent::Stdout(ref b) | ExecEvent::Stderr(ref b) = ev {
                output_bytes = output_bytes.saturating_add(b.len());
                if output_bytes > validate::MAX_OUTPUT_BYTES {
                    let _ = tx
                        .send(Ok(limit_event(LimitKind::Output, "output limit exceeded")))
                        .await;
                    destroy_now(&registry, &sandbox_id).await;
                    break;
                }
            }
            let done = matches!(ev, ExecEvent::Exited { .. });
            if tx.send(Ok(exec_event_to_pb(ev))).await.is_err() || done {
                break;
            }
        }
    });
    Box::pin(ReceiverStream::new(rx))
}

async fn destroy_now(registry: &Registry, sandbox_id: &str) {
    if let Some(instance) = registry.remove(sandbox_id).await {
        let _ = instance.destroy().await;
    }
}

fn limit_event(kind: LimitKind, detail: &str) -> pb::ExecEvent {
    pb::ExecEvent {
        event: Some(pb::exec_event::Event::LimitExceeded(pb::LimitExceeded {
            kind: limit_kind_to_pb(kind),
            detail: detail.to_string(),
        })),
    }
}

fn exec_event_to_pb(ev: ExecEvent) -> pb::ExecEvent {
    ev.into()
}

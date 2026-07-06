//! `GrpcSandboxClient` — orchestrator への tonic gRPC クライアント（`Sandbox` 実装）。

use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use tonic::transport::Channel;

use crate::error::SandboxError;
use crate::pb;
use crate::pb::sandbox_service_client::SandboxServiceClient;
use crate::spec::{DirEntry, ExecEvent, ExecRequest, Sandbox, SandboxHandle, SandboxSpec};

/// orchestrator gRPC への接続。tonic クライアントは clone が安価（内部で multiplex）。
#[derive(Clone)]
pub struct GrpcSandboxClient {
    inner: SandboxServiceClient<Channel>,
}

impl GrpcSandboxClient {
    /// 遅延接続でエンドポイントに接続する（compose 網内・非公開ポート）。
    pub fn connect_lazy(endpoint: impl Into<String>) -> Result<Self, SandboxError> {
        let channel = Channel::from_shared(endpoint.into())
            .map_err(|e| SandboxError::Invalid(format!("invalid sandbox endpoint: {e}")))?
            .connect_lazy();
        Ok(GrpcSandboxClient {
            inner: SandboxServiceClient::new(channel),
        })
    }

    /// 既存 Channel から構築する（テスト・カスタムトランスポート用）。
    pub fn with_channel(channel: Channel) -> Self {
        GrpcSandboxClient {
            inner: SandboxServiceClient::new(channel),
        }
    }
}

fn status_to_err(s: &tonic::Status) -> SandboxError {
    match s.code() {
        tonic::Code::InvalidArgument => SandboxError::Invalid(s.message().to_string()),
        tonic::Code::NotFound => SandboxError::NotFound(s.message().to_string()),
        tonic::Code::Unimplemented => SandboxError::Unimplemented(s.message().to_string()),
        tonic::Code::Unavailable => SandboxError::Unavailable(s.message().to_string()),
        _ => SandboxError::Internal(s.message().to_string()),
    }
}

#[async_trait]
impl Sandbox for GrpcSandboxClient {
    async fn create(&self, spec: SandboxSpec) -> Result<SandboxHandle, SandboxError> {
        let req = pb::CreateRequest {
            spec: Some(spec.into()),
        };
        let resp = self
            .inner
            .clone()
            .create(req)
            .await
            .map_err(|s| status_to_err(&s))?
            .into_inner();
        Ok(SandboxHandle {
            id: resp.sandbox_id,
        })
    }

    async fn exec(
        &self,
        handle: &SandboxHandle,
        req: ExecRequest,
    ) -> Result<BoxStream<'static, Result<ExecEvent, SandboxError>>, SandboxError> {
        let mut pb_req: pb::ExecRequest = req.into();
        pb_req.sandbox_id = handle.id.clone();
        let stream = self
            .inner
            .clone()
            .exec(pb_req)
            .await
            .map_err(|s| status_to_err(&s))?
            .into_inner();
        let mapped = stream.map(|item| match item {
            Ok(ev) => ExecEvent::try_from(ev),
            Err(s) => Err(status_to_err(&s)),
        });
        Ok(Box::pin(mapped))
    }

    async fn put_file(
        &self,
        handle: &SandboxHandle,
        path: &str,
        bytes: Vec<u8>,
    ) -> Result<(), SandboxError> {
        let req = pb::PutFileRequest {
            sandbox_id: handle.id.clone(),
            path: path.to_string(),
            content: bytes,
        };
        self.inner
            .clone()
            .put_file(req)
            .await
            .map_err(|s| status_to_err(&s))?;
        Ok(())
    }

    async fn get_file(&self, handle: &SandboxHandle, path: &str) -> Result<Vec<u8>, SandboxError> {
        let req = pb::GetFileRequest {
            sandbox_id: handle.id.clone(),
            path: path.to_string(),
        };
        let resp = self
            .inner
            .clone()
            .get_file(req)
            .await
            .map_err(|s| status_to_err(&s))?
            .into_inner();
        Ok(resp.content)
    }

    async fn list_dir(
        &self,
        handle: &SandboxHandle,
        path: &str,
    ) -> Result<Vec<DirEntry>, SandboxError> {
        let req = pb::ListDirRequest {
            sandbox_id: handle.id.clone(),
            path: path.to_string(),
        };
        let resp = self
            .inner
            .clone()
            .list_dir(req)
            .await
            .map_err(|s| status_to_err(&s))?
            .into_inner();
        Ok(resp.entries.into_iter().map(Into::into).collect())
    }

    async fn destroy(&self, handle: &SandboxHandle) -> Result<(), SandboxError> {
        let req = pb::DestroyRequest {
            sandbox_id: handle.id.clone(),
        };
        self.inner
            .clone()
            .destroy(req)
            .await
            .map_err(|s| status_to_err(&s))?;
        Ok(())
    }
}

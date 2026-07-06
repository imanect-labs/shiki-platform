//! wasm バックエンド: per-sandbox の secure-exec-sidecar 子プロセスを spawn し wire プロトコルで駆動する。
//!
//! 手順: spawn → Authenticate → OpenSession → CreateVm(json_config) →（software があれば
//! ConfigureVm でコマンドルートを host_dir マウント）。
//! 実行・ファイル・破棄は `instance` モジュール。ゲスト由来の応答は敵対的として扱う（PIT-23）。

mod instance;

use std::sync::Arc;

use async_trait::async_trait;
use sandbox_client::{SandboxBackend, SandboxError, SandboxSpec};
use secure_exec_client::wire;
use secure_exec_client::SidecarTransport;

use super::{Backend, Instance};
use crate::config::{spec_to_vm_config, OrchestratorEnv};

pub use instance::WasmInstance;

/// wasm バックエンド。create ごとに新しい sidecar 子プロセスを立てる（1 transport=1 session=1 VM）。
pub struct WasmBackend {
    sidecar_bin: Option<String>,
    env: OrchestratorEnv,
}

impl WasmBackend {
    pub fn new(sidecar_bin: Option<String>, env: OrchestratorEnv) -> Self {
        WasmBackend { sidecar_bin, env }
    }
}

/// wire リクエスト 1 往復の上限（hang した sidecar でハンドラを塞がない）。
const WIRE_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(1);

/// wire リクエストを 1 往復する（TransportError→SandboxError・タイムアウト付き）。
pub(crate) async fn request(
    transport: &SidecarTransport,
    ownership: wire::OwnershipScope,
    payload: wire::RequestPayload,
) -> Result<wire::ResponsePayload, SandboxError> {
    match tokio::time::timeout(WIRE_TIMEOUT, transport.request_wire(ownership, payload)).await {
        Ok(Ok(resp)) => Ok(resp),
        Ok(Err(e)) => Err(SandboxError::Unavailable(format!("sidecar transport: {e}"))),
        Err(_) => Err(SandboxError::Unavailable(
            "sidecar request timed out".into(),
        )),
    }
}

fn conn_scope(connection_id: &str) -> wire::OwnershipScope {
    wire::OwnershipScope::ConnectionOwnership(wire::ConnectionOwnership {
        connection_id: connection_id.to_string(),
    })
}

fn session_scope(connection_id: &str, session_id: &str) -> wire::OwnershipScope {
    wire::OwnershipScope::SessionOwnership(wire::SessionOwnership {
        connection_id: connection_id.to_string(),
        session_id: session_id.to_string(),
    })
}

pub(crate) fn vm_scope(connection_id: &str, session_id: &str, vm_id: &str) -> wire::OwnershipScope {
    wire::OwnershipScope::VmOwnership(wire::VmOwnership {
        connection_id: connection_id.to_string(),
        session_id: session_id.to_string(),
        vm_id: vm_id.to_string(),
    })
}

fn unexpected(what: &str, got: &wire::ResponsePayload) -> SandboxError {
    SandboxError::Internal(format!("unexpected sidecar response for {what}: {got:?}"))
}

#[async_trait]
impl Backend for WasmBackend {
    async fn create(&self, spec: SandboxSpec) -> Result<Arc<dyn Instance>, SandboxError> {
        if spec.backend != SandboxBackend::Wasm {
            return Err(SandboxError::Unimplemented(
                "only the wasm backend is available in alpha".into(),
            ));
        }
        if spec.mounts_allowed {
            return Err(SandboxError::Unimplemented(
                "storage mounts are post-alpha".into(),
            ));
        }
        // 0. software（ゲストコマンド）を先に解決する（不正名・未同梱は spawn 前に fail-fast）。
        //    コマンドルートの host_dir マウント記述子を得る（None なら要求なし）。
        let command_mount = crate::software::resolve_command_mount(
            self.env.commands_dir.as_deref(),
            &spec.software,
        )?;

        // 1. sidecar 子プロセスを spawn（kill_on_drop）。
        let transport = SidecarTransport::spawn(self.sidecar_bin.clone())
            .await
            .map_err(|e| SandboxError::Unavailable(format!("spawn sidecar: {e}")))?;

        // 2. Authenticate（stdio は信頼済み・任意トークン）。
        let auth = request(
            &transport,
            conn_scope("shiki"),
            wire::RequestPayload::AuthenticateRequest(wire::AuthenticateRequest {
                client_name: "shiki-orchestrator".to_string(),
                auth_token: "shiki".to_string(),
                protocol_version: wire::PROTOCOL_VERSION,
                bridge_version: secure_exec_bridge::bridge_contract().version,
            }),
        )
        .await?;
        let (connection_id, max_frame) = match auth {
            wire::ResponsePayload::AuthenticatedResponse(r) => (r.connection_id, r.max_frame_bytes),
            other => return Err(unexpected("authenticate", &other)),
        };
        transport.set_max_frame_bytes(max_frame as usize);

        // 3. OpenSession。
        let opened = request(
            &transport,
            conn_scope(&connection_id),
            wire::RequestPayload::OpenSessionRequest(wire::OpenSessionRequest {
                placement: wire::SidecarPlacement::SidecarPlacementShared(
                    wire::SidecarPlacementShared { pool: None },
                ),
                metadata: std::collections::HashMap::new(),
            }),
        )
        .await?;
        let session_id = match opened {
            wire::ResponsePayload::SessionOpenedResponse(r) => r.session_id,
            other => return Err(unexpected("open session", &other)),
        };

        // 4. CreateVm（spec→CreateVmConfig を JSON で埋める）。
        let vm_config = spec_to_vm_config(&spec, &self.env);
        let created = request(
            &transport,
            session_scope(&connection_id, &session_id),
            wire::RequestPayload::CreateVmRequest(wire::CreateVmRequest::json_config(
                wire::GuestRuntimeKind::WebAssembly,
                vm_config,
            )),
        )
        .await?;
        let vm_id = match created {
            wire::ResponsePayload::VmCreatedResponse(r) => r.vm_id,
            other => return Err(unexpected("create vm", &other)),
        };

        // 5. ゲストコマンドがあれば ConfigureVm でコマンドルートを host_dir マウントする
        //    （`/__secure_exec/commands/0` が `$PATH` に載る）。permissions は None＝CreateVm の
        //    egress ポリシを維持（緩めない）。tar 投影（packages）では kernel 管理 stdio が働かず
        //    出力が surface しないため、この host_dir 経路を使う（#109 の調査結果）。
        if let Some(mount) = command_mount {
            let configured = request(
                &transport,
                vm_scope(&connection_id, &session_id, &vm_id),
                wire::RequestPayload::ConfigureVmRequest(wire::ConfigureVmRequest {
                    mounts: vec![mount],
                    software: Vec::new(),
                    permissions: None,
                    module_access_cwd: None,
                    instructions: Vec::new(),
                    projected_modules: Vec::new(),
                    command_permissions: std::collections::HashMap::new(),
                    loopback_exempt_ports: Vec::new(),
                    packages: Vec::new(),
                    packages_mount_at: String::new(),
                    bootstrap_commands: Vec::new(),
                    tool_shim_commands: Vec::new(),
                }),
            )
            .await?;
            match configured {
                wire::ResponsePayload::VmConfiguredResponse(_) => {}
                other => return Err(unexpected("configure vm (commands)", &other)),
            }
        }

        Ok(Arc::new(WasmInstance::new(
            transport,
            connection_id,
            session_id,
            vm_id,
        )))
    }
}

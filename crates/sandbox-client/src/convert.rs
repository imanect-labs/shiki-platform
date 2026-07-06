//! ドメイン型 ⇄ proto の変換。
//!
//! ドメイン→proto は全フィールドを **struct 分解**して書く。Rust 型にフィールドを足すと分解が
//! コンパイルエラーになり proto 更新を強制する（Rust→proto が真実）。proto→ドメインは未指定 enum を
//! 弾くため `TryFrom`。round-trip 恒等性は proptest で担保（tests/roundtrip.rs）。

use crate::error::SandboxError;
use crate::pb;
use crate::spec::{
    DirEntry, Egress, EgressRule, ExecEvent, ExecRequest, LimitKind, SandboxBackend,
    SandboxLifetime, SandboxLimits, SandboxSpec,
};

// ---- ドメイン → proto（infallible・全フィールド分解） ----

impl From<SandboxBackend> for pb::Backend {
    fn from(b: SandboxBackend) -> Self {
        match b {
            SandboxBackend::Wasm => pb::Backend::Wasm,
            SandboxBackend::Gvisor => pb::Backend::Gvisor,
            SandboxBackend::Firecracker => pb::Backend::Firecracker,
        }
    }
}

impl From<EgressRule> for pb::EgressRule {
    fn from(r: EgressRule) -> Self {
        let EgressRule { host_pattern, port } = r;
        pb::EgressRule {
            host_pattern,
            port: u32::from(port),
        }
    }
}

impl From<Egress> for pb::Egress {
    fn from(e: Egress) -> Self {
        let Egress {
            static_allow,
            dynamic_allow,
            deny_overlay,
            secret_attach,
        } = e;
        pb::Egress {
            static_allow: static_allow.into_iter().map(Into::into).collect(),
            dynamic_allow: dynamic_allow.into_iter().map(Into::into).collect(),
            deny_overlay: deny_overlay.into_iter().map(Into::into).collect(),
            secret_attach,
        }
    }
}

impl From<SandboxLimits> for pb::Limits {
    fn from(l: SandboxLimits) -> Self {
        let SandboxLimits {
            wall_clock_ms,
            exec_timeout_ms,
            memory_mb,
            max_processes,
            max_fs_bytes,
            max_open_fds,
            max_output_bytes,
        } = l;
        pb::Limits {
            wall_clock_ms,
            exec_timeout_ms,
            memory_mb,
            max_processes,
            max_fs_bytes,
            max_open_fds,
            max_output_bytes,
        }
    }
}

impl From<SandboxSpec> for pb::Spec {
    fn from(s: SandboxSpec) -> Self {
        let SandboxSpec {
            backend,
            tenant_id,
            org,
            principal,
            limits,
            egress,
            software,
            mounts_allowed,
            lifetime,
        } = s;
        // Ephemeral{ttl_ms} → ttl_ms、Persistent → 0（orchestrator が alpha で拒否）。
        let ttl_ms = match lifetime {
            SandboxLifetime::Ephemeral { ttl_ms } => ttl_ms,
            SandboxLifetime::Persistent => 0,
        };
        pb::Spec {
            backend: pb::Backend::from(backend) as i32,
            tenant_id,
            org,
            principal,
            limits: Some(limits.into()),
            egress: Some(egress.into()),
            software,
            mounts_allowed,
            ttl_ms,
        }
    }
}

impl From<ExecRequest> for pb::ExecRequest {
    fn from(r: ExecRequest) -> Self {
        let (kind, payload, timeout_ms) = match r {
            ExecRequest::Python { code, timeout_ms } => (pb::ExecKind::Python, code, timeout_ms),
            ExecRequest::Shell { cmd, timeout_ms } => (pb::ExecKind::Shell, cmd, timeout_ms),
        };
        pb::ExecRequest {
            sandbox_id: String::new(), // 呼び出し側が埋める
            kind: kind as i32,
            payload,
            timeout_ms: timeout_ms.unwrap_or(0),
        }
    }
}

impl From<LimitKind> for pb::LimitKind {
    fn from(k: LimitKind) -> Self {
        match k {
            LimitKind::WallClock => pb::LimitKind::WallClock,
            LimitKind::Memory => pb::LimitKind::Memory,
            LimitKind::Output => pb::LimitKind::Output,
            LimitKind::Process => pb::LimitKind::Process,
            LimitKind::Filesystem => pb::LimitKind::Filesystem,
        }
    }
}

impl From<DirEntry> for pb::DirEntry {
    fn from(e: DirEntry) -> Self {
        let DirEntry { name, is_dir, size } = e;
        pb::DirEntry { name, is_dir, size }
    }
}

// ---- proto → ドメイン（未指定 enum を弾く TryFrom） ----

fn invalid(msg: impl Into<String>) -> SandboxError {
    SandboxError::Invalid(msg.into())
}

impl TryFrom<pb::Backend> for SandboxBackend {
    type Error = SandboxError;
    fn try_from(b: pb::Backend) -> Result<Self, Self::Error> {
        match b {
            pb::Backend::Wasm => Ok(SandboxBackend::Wasm),
            pb::Backend::Gvisor => Ok(SandboxBackend::Gvisor),
            pb::Backend::Firecracker => Ok(SandboxBackend::Firecracker),
            pb::Backend::Unspecified => Err(invalid("backend unspecified")),
        }
    }
}

impl From<pb::EgressRule> for EgressRule {
    fn from(r: pb::EgressRule) -> Self {
        let pb::EgressRule { host_pattern, port } = r;
        EgressRule {
            host_pattern,
            // proto は u32。ポートは 0..=65535 に丸める（範囲外は 0=全ポート扱い）。
            port: u16::try_from(port).unwrap_or(0),
        }
    }
}

impl From<pb::Egress> for Egress {
    fn from(e: pb::Egress) -> Self {
        let pb::Egress {
            static_allow,
            dynamic_allow,
            deny_overlay,
            secret_attach,
        } = e;
        Egress {
            static_allow: static_allow.into_iter().map(Into::into).collect(),
            dynamic_allow: dynamic_allow.into_iter().map(Into::into).collect(),
            deny_overlay: deny_overlay.into_iter().map(Into::into).collect(),
            secret_attach,
        }
    }
}

impl From<pb::Limits> for SandboxLimits {
    fn from(l: pb::Limits) -> Self {
        let pb::Limits {
            wall_clock_ms,
            exec_timeout_ms,
            memory_mb,
            max_processes,
            max_fs_bytes,
            max_open_fds,
            max_output_bytes,
        } = l;
        SandboxLimits {
            wall_clock_ms,
            exec_timeout_ms,
            memory_mb,
            max_processes,
            max_fs_bytes,
            max_open_fds,
            max_output_bytes,
        }
    }
}

impl TryFrom<pb::Spec> for SandboxSpec {
    type Error = SandboxError;
    fn try_from(s: pb::Spec) -> Result<Self, Self::Error> {
        let pb::Spec {
            backend,
            tenant_id,
            org,
            principal,
            limits,
            egress,
            software,
            mounts_allowed,
            ttl_ms,
        } = s;
        let backend = SandboxBackend::try_from(
            pb::Backend::try_from(backend).unwrap_or(pb::Backend::Unspecified),
        )?;
        Ok(SandboxSpec {
            backend,
            tenant_id,
            org,
            principal,
            limits: limits
                .map(Into::into)
                .ok_or_else(|| invalid("limits missing"))?,
            egress: egress.map(Into::into).unwrap_or_default(),
            software,
            mounts_allowed,
            lifetime: SandboxLifetime::Ephemeral { ttl_ms },
        })
    }
}

impl TryFrom<pb::ExecRequest> for ExecRequest {
    type Error = SandboxError;
    fn try_from(r: pb::ExecRequest) -> Result<Self, Self::Error> {
        let pb::ExecRequest {
            sandbox_id: _,
            kind,
            payload,
            timeout_ms,
        } = r;
        let timeout = (timeout_ms != 0).then_some(timeout_ms);
        match pb::ExecKind::try_from(kind).unwrap_or(pb::ExecKind::Unspecified) {
            pb::ExecKind::Python => Ok(ExecRequest::Python {
                code: payload,
                timeout_ms: timeout,
            }),
            pb::ExecKind::Shell => Ok(ExecRequest::Shell {
                cmd: payload,
                timeout_ms: timeout,
            }),
            pb::ExecKind::Unspecified => Err(invalid("exec kind unspecified")),
        }
    }
}

impl TryFrom<pb::LimitKind> for LimitKind {
    type Error = SandboxError;
    fn try_from(k: pb::LimitKind) -> Result<Self, Self::Error> {
        match k {
            pb::LimitKind::WallClock => Ok(LimitKind::WallClock),
            pb::LimitKind::Memory => Ok(LimitKind::Memory),
            pb::LimitKind::Output => Ok(LimitKind::Output),
            pb::LimitKind::Process => Ok(LimitKind::Process),
            pb::LimitKind::Filesystem => Ok(LimitKind::Filesystem),
            pb::LimitKind::Unspecified => Err(invalid("limit kind unspecified")),
        }
    }
}

impl From<pb::DirEntry> for DirEntry {
    fn from(e: pb::DirEntry) -> Self {
        let pb::DirEntry { name, is_dir, size } = e;
        DirEntry { name, is_dir, size }
    }
}

impl TryFrom<pb::ExecEvent> for ExecEvent {
    type Error = SandboxError;
    fn try_from(e: pb::ExecEvent) -> Result<Self, Self::Error> {
        let event = e.event.ok_or_else(|| invalid("exec event empty"))?;
        Ok(match event {
            pb::exec_event::Event::Output(o) => {
                let pb::OutputChunk { channel, chunk } = o;
                match pb::ExecChannel::try_from(channel).unwrap_or(pb::ExecChannel::Unspecified) {
                    pb::ExecChannel::Stdout => ExecEvent::Stdout(chunk),
                    pb::ExecChannel::Stderr => ExecEvent::Stderr(chunk),
                    pb::ExecChannel::Unspecified => return Err(invalid("channel unspecified")),
                }
            }
            pb::exec_event::Event::Exited(x) => ExecEvent::Exited { code: x.code },
            pb::exec_event::Event::LimitExceeded(l) => {
                let pb::LimitExceeded { kind, detail } = l;
                let kind = LimitKind::try_from(
                    pb::LimitKind::try_from(kind).unwrap_or(pb::LimitKind::Unspecified),
                )?;
                ExecEvent::LimitExceeded { kind, detail }
            }
        })
    }
}

/// ドメイン ExecEvent → proto（orchestrator の server 側で使う）。
impl From<ExecEvent> for pb::ExecEvent {
    fn from(e: ExecEvent) -> Self {
        let event = match e {
            ExecEvent::Stdout(chunk) => pb::exec_event::Event::Output(pb::OutputChunk {
                channel: pb::ExecChannel::Stdout as i32,
                chunk,
            }),
            ExecEvent::Stderr(chunk) => pb::exec_event::Event::Output(pb::OutputChunk {
                channel: pb::ExecChannel::Stderr as i32,
                chunk,
            }),
            ExecEvent::Exited { code } => pb::exec_event::Event::Exited(pb::ProcessExited { code }),
            ExecEvent::LimitExceeded { kind, detail } => {
                pb::exec_event::Event::LimitExceeded(pb::LimitExceeded {
                    kind: pb::LimitKind::from(kind) as i32,
                    detail,
                })
            }
        };
        pb::ExecEvent { event: Some(event) }
    }
}

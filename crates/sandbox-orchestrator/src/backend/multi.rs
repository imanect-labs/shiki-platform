//! `MultiBackend`: `spec.backend` でティア（wasm / gVisor / Firecracker）へ振り分ける。
//!
//! 未構成のティアが要求されたら理由付き `Unimplemented` を返す（黙って弱いティアに落とさない）。

use std::sync::Arc;

use async_trait::async_trait;
use sandbox_client::{SandboxBackend, SandboxError, SandboxSpec};

use super::{Backend, Instance};

/// 3 ティアのディスパッチャ。gVisor/Firecracker は構成済みのときだけ `Some`。
pub struct MultiBackend {
    wasm: Arc<dyn Backend>,
    gvisor: Option<Arc<dyn Backend>>,
    firecracker: Option<Arc<dyn Backend>>,
}

impl MultiBackend {
    #[must_use]
    pub fn new(
        wasm: Arc<dyn Backend>,
        gvisor: Option<Arc<dyn Backend>>,
        firecracker: Option<Arc<dyn Backend>>,
    ) -> Self {
        MultiBackend {
            wasm,
            gvisor,
            firecracker,
        }
    }
}

#[async_trait]
impl Backend for MultiBackend {
    async fn create(&self, spec: SandboxSpec) -> Result<Arc<dyn Instance>, SandboxError> {
        match spec.backend {
            SandboxBackend::Wasm => self.wasm.create(spec).await,
            SandboxBackend::Gvisor => match &self.gvisor {
                Some(b) => b.create(spec).await,
                None => Err(SandboxError::Unimplemented(
                    "gvisor backend not configured (set SANDBOX__GVISOR__ENABLED=1 with RUNSC_BIN/ROOTFS_DIR)".into(),
                )),
            },
            SandboxBackend::Firecracker => match &self.firecracker {
                Some(b) => b.create(spec).await,
                None => Err(SandboxError::Unimplemented(
                    "firecracker backend not configured (set SANDBOX__FIRECRACKER__ENABLED=1 with BIN/KERNEL/ROOTFS)".into(),
                )),
            },
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::backend::fake::FakeBackend;
    use sandbox_client::SandboxSpec;

    fn spec(backend: SandboxBackend) -> SandboxSpec {
        let mut s = SandboxSpec::code_interpreter("t".into(), "o".into(), "u:1".into());
        s.backend = backend;
        s
    }

    #[tokio::test]
    async fn routes_wasm_to_configured_backend() {
        let mb = MultiBackend::new(Arc::new(FakeBackend::new()), None, None);
        assert!(mb.create(spec(SandboxBackend::Wasm)).await.is_ok());
    }

    #[tokio::test]
    async fn unconfigured_tiers_are_unimplemented() {
        let mb = MultiBackend::new(Arc::new(FakeBackend::new()), None, None);
        assert!(matches!(
            mb.create(spec(SandboxBackend::Gvisor)).await,
            Err(SandboxError::Unimplemented(_))
        ));
        assert!(matches!(
            mb.create(spec(SandboxBackend::Firecracker)).await,
            Err(SandboxError::Unimplemented(_))
        ));
    }

    #[tokio::test]
    async fn configured_gvisor_is_dispatched() {
        let mb = MultiBackend::new(
            Arc::new(FakeBackend::new()),
            Some(Arc::new(FakeBackend::new())),
            None,
        );
        assert!(mb.create(spec(SandboxBackend::Gvisor)).await.is_ok());
    }
}

//! proto round-trip 恒等テスト（Rust→proto→Rust が値を保つ）。
//! convert.rs の全フィールド分解が壊れていないことを担保する。

use proptest::prelude::*;
use sandbox_client::pb;
use sandbox_client::{
    Egress, EgressRule, ExecRequest, SandboxBackend, SandboxLifetime, SandboxLimits, SandboxSpec,
};

fn backend() -> impl Strategy<Value = SandboxBackend> {
    prop_oneof![
        Just(SandboxBackend::Wasm),
        Just(SandboxBackend::Gvisor),
        Just(SandboxBackend::Firecracker),
    ]
}

fn egress_rule() -> impl Strategy<Value = EgressRule> {
    ("[a-z.*]{1,20}", any::<u16>())
        .prop_map(|(host_pattern, port)| EgressRule { host_pattern, port })
}

fn egress() -> impl Strategy<Value = Egress> {
    (
        prop::collection::vec(egress_rule(), 0..4),
        prop::collection::vec(egress_rule(), 0..4),
        prop::collection::vec(egress_rule(), 0..4),
        any::<bool>(),
    )
        .prop_map(
            |(static_allow, dynamic_allow, deny_overlay, secret_attach)| Egress {
                static_allow,
                dynamic_allow,
                deny_overlay,
                secret_attach,
            },
        )
}

fn limits() -> impl Strategy<Value = SandboxLimits> {
    (
        any::<u64>(),
        any::<u64>(),
        any::<u64>(),
        any::<u32>(),
        any::<u64>(),
        any::<u32>(),
        any::<u64>(),
    )
        .prop_map(
            |(
                wall_clock_ms,
                exec_timeout_ms,
                memory_mb,
                max_processes,
                max_fs_bytes,
                max_open_fds,
                max_output_bytes,
            )| SandboxLimits {
                wall_clock_ms,
                exec_timeout_ms,
                memory_mb,
                max_processes,
                max_fs_bytes,
                max_open_fds,
                max_output_bytes,
            },
        )
}

fn spec() -> impl Strategy<Value = SandboxSpec> {
    (
        backend(),
        "[a-z0-9]{1,12}",
        "[a-z0-9]{1,12}",
        "[a-z0-9]{1,12}",
        limits(),
        egress(),
        prop::collection::vec("[a-z]{1,10}", 0..5),
        any::<bool>(),
        any::<u64>(),
    )
        .prop_map(
            |(
                backend,
                tenant_id,
                org,
                principal,
                limits,
                egress,
                software,
                mounts_allowed,
                ttl_ms,
            )| {
                SandboxSpec {
                    backend,
                    tenant_id,
                    org,
                    principal,
                    limits,
                    egress,
                    software,
                    mounts_allowed,
                    lifetime: SandboxLifetime::Ephemeral { ttl_ms },
                }
            },
        )
}

proptest! {
    #[test]
    fn spec_roundtrip(s in spec()) {
        let wire: pb::Spec = s.clone().into();
        let back = SandboxSpec::try_from(wire).expect("spec should decode");
        prop_assert_eq!(s, back);
    }

    #[test]
    fn exec_request_roundtrip(code in "[ -~]{0,50}", is_python in any::<bool>(), timeout in any::<u64>()) {
        let req = if is_python {
            ExecRequest::Python { code: code.clone(), timeout_ms: (timeout != 0).then_some(timeout) }
        } else {
            ExecRequest::Shell { cmd: code.clone(), timeout_ms: (timeout != 0).then_some(timeout) }
        };
        let wire: pb::ExecRequest = req.clone().into();
        let back = ExecRequest::try_from(wire).expect("exec req should decode");
        prop_assert_eq!(req, back);
    }
}

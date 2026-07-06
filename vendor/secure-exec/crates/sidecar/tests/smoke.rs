use secure_exec_sidecar::scaffold;
use secure_exec_sidecar::wire::{DEFAULT_MAX_FRAME_BYTES, PROTOCOL_NAME, PROTOCOL_VERSION};
use secure_exec_sidecar::NativeSidecarConfig;

#[test]
fn native_sidecar_scaffold_tracks_kernel_and_execution_dependencies() {
    let scaffold = scaffold();

    assert_eq!(scaffold.package_name, "secure-exec-sidecar");
    assert_eq!(scaffold.binary_name, "secure-exec-sidecar");
    assert_eq!(scaffold.kernel_package, "secure-exec-kernel");
    assert_eq!(scaffold.execution_package, "secure-exec-execution");
    assert_eq!(scaffold.protocol_name, PROTOCOL_NAME);
    assert_eq!(scaffold.protocol_version, PROTOCOL_VERSION);
    assert_eq!(scaffold.max_frame_bytes, DEFAULT_MAX_FRAME_BYTES);
    assert_eq!(
        NativeSidecarConfig::default().sidecar_id,
        "secure-exec-sidecar"
    );
}

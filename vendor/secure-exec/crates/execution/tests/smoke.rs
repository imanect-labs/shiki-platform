use secure_exec_execution::{scaffold, GuestRuntime};

#[test]
fn execution_scaffold_is_native_and_depends_on_kernel() {
    let scaffold = scaffold();

    assert_eq!(scaffold.package_name, "secure-exec-execution");
    assert_eq!(scaffold.kernel_package, "secure-exec-kernel");
    assert_eq!(scaffold.target, "native");
    assert_eq!(
        scaffold.planned_guest_runtimes,
        [GuestRuntime::JavaScript, GuestRuntime::WebAssembly]
    );
}

//! 最小 WASI スタブ（`wasm32-wasip1` ゲストが要求する安全なサブセットのみ提供）。
//!
//! QuickJS ゲストは random/clock/環境変数/stdio を参照する。これらは **fs/net を含まない**
//! 安全な機能のみ実装し、`path_open` / `sock_*` / `fd_prestat_*` は**提供しない**
//! （ゲスト wasm がそもそも import していない＝ファイル/ソケットを開く手段が構造的に無い）。
//! これが「wasm 内から外界（fs/net）に到達できない」（script.md §4・受け入れ条件）の担保。

use wasmtime::{Caller, Linker};

use crate::engine::HostState;

/// WASI errno: success。
const ERRNO_SUCCESS: i32 = 0;
/// WASI errno: bad file descriptor（fs 無しの read/seek 等）。
const ERRNO_BADF: i32 = 8;

/// 必要最小の preview1 関数を linker へ登録する（fs/net 系は登録しない）。
pub(crate) fn add_to_linker(linker: &mut Linker<HostState>) -> Result<(), wasmtime::Error> {
    let m = "wasi_snapshot_preview1";

    // 乱数: HostState の LCG から疑似乱数を書き込む（OS エントロピーへは触れない）。
    linker.func_wrap(
        m,
        "random_get",
        |mut caller: Caller<'_, HostState>, buf: u32, len: u32| -> i32 {
            // guest が渡す len（最大 4GB）で host RAM を確保させない（DoS 対策）。1 回の要求上限を設ける。
            const MAX_RANDOM_BYTES: u32 = 64 * 1024;
            if len > MAX_RANDOM_BYTES {
                return 28; // EINVAL 相当。
            }
            let mut bytes = vec![0u8; len as usize];
            {
                let st = caller.data_mut();
                for b in &mut bytes {
                    // PCG/LCG 定数（Knuth MMIX）。
                    st.rng = st
                        .rng
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1_442_695_040_888_963_407);
                    *b = (st.rng >> 33) as u8;
                }
            }
            write_bytes(&mut caller, buf, &bytes)
        },
    )?;

    // 時刻: 単調増加カウンタ（ns）。実時計に触れず決定論も要求しない（script.md Q5）。
    linker.func_wrap(
        m,
        "clock_time_get",
        |mut caller: Caller<'_, HostState>, _id: i32, _prec: i64, out: u32| -> i32 {
            let now = {
                let st = caller.data_mut();
                st.clock = st.clock.wrapping_add(1_000_000);
                st.clock
            };
            write_bytes(&mut caller, out, &now.to_le_bytes())
        },
    )?;

    // 環境変数: 空（0 個）。
    linker.func_wrap(
        m,
        "environ_sizes_get",
        |mut caller: Caller<'_, HostState>, count: u32, size: u32| -> i32 {
            let r1 = write_bytes(&mut caller, count, &0u32.to_le_bytes());
            if r1 != ERRNO_SUCCESS {
                return r1;
            }
            write_bytes(&mut caller, size, &0u32.to_le_bytes())
        },
    )?;
    linker.func_wrap(
        m,
        "environ_get",
        |_caller: Caller<'_, HostState>, _e: u32, _b: u32| -> i32 { ERRNO_SUCCESS },
    )?;

    // stdio: fd_write は stdout/stderr のみ「書けた」ことにする（実出力は破棄・ログは Shiki.log）。
    linker.func_wrap(
        m,
        "fd_write",
        |mut caller: Caller<'_, HostState>,
         fd: i32,
         iovs: u32,
         iovs_len: u32,
         nwritten: u32|
         -> i32 {
            if fd != 1 && fd != 2 {
                return ERRNO_BADF;
            }
            let mut total: u32 = 0;
            for i in 0..iovs_len {
                let iov = iovs + i * 8;
                let mut buf = [0u8; 8];
                if read_bytes(&mut caller, iov, &mut buf) != ERRNO_SUCCESS {
                    return ERRNO_BADF;
                }
                let len = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
                total = total.saturating_add(len);
            }
            write_bytes(&mut caller, nwritten, &total.to_le_bytes())
        },
    )?;

    // fs 系は「開けない/読めない」を返すだけ（preopen が無いので実ファイルは存在しない）。
    linker.func_wrap(
        m,
        "fd_read",
        |_c: Caller<'_, HostState>, _fd: i32, _iovs: u32, _len: u32, _nread: u32| -> i32 {
            ERRNO_BADF
        },
    )?;
    linker.func_wrap(
        m,
        "fd_seek",
        |_c: Caller<'_, HostState>, _fd: i32, _off: i64, _whence: i32, _out: u32| -> i32 {
            ERRNO_BADF
        },
    )?;
    linker.func_wrap(
        m,
        "fd_close",
        |_c: Caller<'_, HostState>, _fd: i32| -> i32 { ERRNO_BADF },
    )?;
    linker.func_wrap(
        m,
        "fd_fdstat_get",
        |_c: Caller<'_, HostState>, _fd: i32, _out: u32| -> i32 { ERRNO_BADF },
    )?;

    // proc_exit: 実行を trap で終わらせる（ゲストの異常終了）。
    linker.func_wrap(
        m,
        "proc_exit",
        |_c: Caller<'_, HostState>, code: i32| -> Result<(), wasmtime::Error> {
            Err(wasmtime::Error::msg(format!("guest proc_exit({code})")))
        },
    )?;

    Ok(())
}

fn guest_memory(caller: &mut Caller<'_, HostState>) -> Option<wasmtime::Memory> {
    caller
        .get_export("memory")
        .and_then(wasmtime::Extern::into_memory)
}

fn write_bytes(caller: &mut Caller<'_, HostState>, ptr: u32, bytes: &[u8]) -> i32 {
    match guest_memory(caller) {
        Some(mem) => match mem.write(&mut *caller, ptr as usize, bytes) {
            Ok(()) => ERRNO_SUCCESS,
            Err(_) => ERRNO_BADF,
        },
        None => ERRNO_BADF,
    }
}

fn read_bytes(caller: &mut Caller<'_, HostState>, ptr: u32, out: &mut [u8]) -> i32 {
    match guest_memory(caller) {
        Some(mem) => match mem.read(&mut *caller, ptr as usize, out) {
            Ok(()) => ERRNO_SUCCESS,
            Err(_) => ERRNO_BADF,
        },
        None => ERRNO_BADF,
    }
}

#[cfg(test)]
mod tests {
    use wasmtime::{Engine, Instance, Linker, Module, Store, TypedFunc};

    use super::*;
    use crate::engine::HostState;
    use crate::host::HostResponse;

    /// WASI スタブ関数を import して呼び出すだけの薄いテスト用ゲスト（memory を export）。
    const HARNESS_WAT: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "random_get" (func $random_get (param i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "clock_time_get" (func $clock (param i32 i64 i32) (result i32)))
      (import "wasi_snapshot_preview1" "environ_sizes_get" (func $env_sizes (param i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "environ_get" (func $env_get (param i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "fd_read" (func $fd_read (param i32 i32 i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "fd_seek" (func $fd_seek (param i32 i64 i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "fd_close" (func $fd_close (param i32) (result i32)))
      (import "wasi_snapshot_preview1" "fd_fdstat_get" (func $fd_fdstat_get (param i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "proc_exit" (func $proc_exit (param i32)))
      (memory (export "memory") 1)
      (func (export "call_random") (param i32 i32) (result i32)
        (call $random_get (local.get 0) (local.get 1)))
      (func (export "call_clock") (param i32) (result i32)
        (call $clock (i32.const 0) (i64.const 0) (local.get 0)))
      (func (export "call_env_sizes") (param i32 i32) (result i32)
        (call $env_sizes (local.get 0) (local.get 1)))
      (func (export "call_env_get") (param i32 i32) (result i32)
        (call $env_get (local.get 0) (local.get 1)))
      (func (export "call_fd_write") (param i32 i32 i32 i32) (result i32)
        (call $fd_write (local.get 0) (local.get 1) (local.get 2) (local.get 3)))
      (func (export "call_fd_read") (param i32 i32 i32 i32) (result i32)
        (call $fd_read (local.get 0) (local.get 1) (local.get 2) (local.get 3)))
      (func (export "call_fd_seek") (param i32 i64 i32 i32) (result i32)
        (call $fd_seek (local.get 0) (local.get 1) (local.get 2) (local.get 3)))
      (func (export "call_fd_close") (param i32) (result i32)
        (call $fd_close (local.get 0)))
      (func (export "call_fd_fdstat") (param i32 i32) (result i32)
        (call $fd_fdstat_get (local.get 0) (local.get 1)))
      (func (export "call_proc_exit") (param i32)
        (call $proc_exit (local.get 0)))
    )
    "#;

    struct Harness {
        store: Store<HostState>,
        instance: Instance,
    }

    impl Harness {
        fn new() -> Self {
            let engine = Engine::default();
            let mut linker = Linker::new(&engine);
            add_to_linker(&mut linker).expect("add wasi stub");
            let module = Module::new(&engine, HARNESS_WAT).expect("wat module");
            let host_fn =
                Box::new(|_c: &crate::host::HostCall| HostResponse::Ok(serde_json::Value::Null));
            let mut store = Store::new(&engine, HostState::new_for_test(host_fn));
            let instance = linker
                .instantiate(&mut store, &module)
                .expect("instantiate");
            Harness { store, instance }
        }

        fn func2(&mut self, name: &str) -> TypedFunc<(i32, i32), i32> {
            self.instance
                .get_typed_func::<(i32, i32), i32>(&mut self.store, name)
                .expect("typed func2")
        }

        fn read(&mut self, ptr: usize, out: &mut [u8]) {
            let mem = self
                .instance
                .get_memory(&mut self.store, "memory")
                .expect("memory");
            mem.read(&self.store, ptr, out).expect("read");
        }

        fn write(&mut self, ptr: usize, bytes: &[u8]) {
            let mem = self
                .instance
                .get_memory(&mut self.store, "memory")
                .expect("memory");
            mem.write(&mut self.store, ptr, bytes).expect("write");
        }
    }

    #[test]
    fn clock_is_monotonic_and_writes_le_bytes() {
        let mut h = Harness::new();
        let clock = h
            .instance
            .get_typed_func::<i32, i32>(&mut h.store, "call_clock")
            .expect("clock");
        let rc = clock.call(&mut h.store, 0).expect("call");
        assert_eq!(rc, ERRNO_SUCCESS);
        let mut buf = [0u8; 8];
        h.read(0, &mut buf);
        assert_eq!(u64::from_le_bytes(buf), 1_000_000);
        // 2 回目は +1_000_000 単調増加。
        clock.call(&mut h.store, 8).expect("call2");
        h.read(8, &mut buf);
        assert_eq!(u64::from_le_bytes(buf), 2_000_000);
    }

    #[test]
    fn clock_write_out_of_bounds_returns_badf() {
        let mut h = Harness::new();
        let clock = h
            .instance
            .get_typed_func::<i32, i32>(&mut h.store, "call_clock")
            .expect("clock");
        // 1 ページ (64KB) の外へ書こうとすると write_bytes が失敗し BADF。
        let rc = clock.call(&mut h.store, 10_000_000).expect("call");
        assert_eq!(rc, ERRNO_BADF);
    }

    #[test]
    fn random_fills_and_rejects_oversized() {
        let mut h = Harness::new();
        let random = h.func2("call_random");
        let rc = random.call(&mut h.store, (16, 32)).expect("call");
        assert_eq!(rc, ERRNO_SUCCESS);
        // 64KB 超の 1 回要求は EINVAL(28) で拒否（DoS 対策）。
        let rc_big = random.call(&mut h.store, (16, 70_000)).expect("call big");
        assert_eq!(rc_big, 28);
    }

    #[test]
    fn environ_reports_empty() {
        let mut h = Harness::new();
        let env_sizes = h.func2("call_env_sizes");
        let rc = env_sizes.call(&mut h.store, (100, 108)).expect("call");
        assert_eq!(rc, ERRNO_SUCCESS);
        let mut count = [0u8; 4];
        let mut size = [0u8; 4];
        h.read(100, &mut count);
        h.read(108, &mut size);
        assert_eq!(u32::from_le_bytes(count), 0);
        assert_eq!(u32::from_le_bytes(size), 0);

        let env_get = h.func2("call_env_get");
        assert_eq!(
            env_get.call(&mut h.store, (0, 0)).expect("env_get"),
            ERRNO_SUCCESS
        );
    }

    #[test]
    fn fd_write_sums_iov_lengths_for_stdio() {
        let mut h = Harness::new();
        // iov[0] = (buf_ptr=300, buf_len=5) を 200 番地へ置く。
        let mut iov = [0u8; 8];
        iov[0..4].copy_from_slice(&300u32.to_le_bytes());
        iov[4..8].copy_from_slice(&5u32.to_le_bytes());
        h.write(200, &iov);

        let fd_write = h
            .instance
            .get_typed_func::<(i32, i32, i32, i32), i32>(&mut h.store, "call_fd_write")
            .expect("fd_write");
        // fd=1 (stdout)・iovs=200・iovs_len=1・nwritten=400。
        let rc = fd_write.call(&mut h.store, (1, 200, 1, 400)).expect("call");
        assert_eq!(rc, ERRNO_SUCCESS);
        let mut nwritten = [0u8; 4];
        h.read(400, &mut nwritten);
        assert_eq!(u32::from_le_bytes(nwritten), 5);
    }

    #[test]
    fn fd_write_rejects_non_stdio_fd() {
        let mut h = Harness::new();
        let fd_write = h
            .instance
            .get_typed_func::<(i32, i32, i32, i32), i32>(&mut h.store, "call_fd_write")
            .expect("fd_write");
        // fd=5（stdout/stderr 以外）は BADF。
        let rc = fd_write.call(&mut h.store, (5, 200, 1, 400)).expect("call");
        assert_eq!(rc, ERRNO_BADF);
    }

    #[test]
    fn fd_write_iov_out_of_bounds_returns_badf() {
        let mut h = Harness::new();
        let fd_write = h
            .instance
            .get_typed_func::<(i32, i32, i32, i32), i32>(&mut h.store, "call_fd_write")
            .expect("fd_write");
        // iovs ポインタがメモリ外 → iov の読取（read_bytes）が失敗し BADF。
        let rc = fd_write
            .call(&mut h.store, (1, 10_000_000, 1, 400))
            .expect("call");
        assert_eq!(rc, ERRNO_BADF);
    }

    #[test]
    fn fs_ops_are_unavailable() {
        let mut h = Harness::new();
        // fd_read / fd_seek / fd_close / fd_fdstat_get は preopen が無く常に BADF。
        let fd_read = h
            .instance
            .get_typed_func::<(i32, i32, i32, i32), i32>(&mut h.store, "call_fd_read")
            .expect("fd_read");
        assert_eq!(
            fd_read.call(&mut h.store, (0, 0, 0, 0)).expect("read"),
            ERRNO_BADF
        );

        let fd_seek = h
            .instance
            .get_typed_func::<(i32, i64, i32, i32), i32>(&mut h.store, "call_fd_seek")
            .expect("fd_seek");
        assert_eq!(
            fd_seek.call(&mut h.store, (0, 0, 0, 0)).expect("seek"),
            ERRNO_BADF
        );

        let fd_close = h
            .instance
            .get_typed_func::<i32, i32>(&mut h.store, "call_fd_close")
            .expect("fd_close");
        assert_eq!(fd_close.call(&mut h.store, 0).expect("close"), ERRNO_BADF);

        let fd_fdstat = h.func2("call_fd_fdstat");
        assert_eq!(
            fd_fdstat.call(&mut h.store, (0, 0)).expect("fdstat"),
            ERRNO_BADF
        );
    }

    #[test]
    fn proc_exit_traps() {
        let mut h = Harness::new();
        let proc_exit = h
            .instance
            .get_typed_func::<i32, ()>(&mut h.store, "call_proc_exit")
            .expect("proc_exit");
        // proc_exit は trap で実行を終わらせる（ゲストの異常終了）。
        assert!(proc_exit.call(&mut h.store, 3).is_err());
    }
}

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

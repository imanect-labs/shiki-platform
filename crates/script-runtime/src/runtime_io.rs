//! script-runtime の線形メモリ受け渡し・トラップ分類・結果エンベロープ処理。
//! [`engine`](crate::engine) の実装詳細を切り出したもの（500 行ゲート対応）。

use serde_json::Value;
use wasmtime::{Caller, Instance, Memory, Store};

use crate::engine::{ExecOutcome, HostState, Termination};

// --- 線形メモリ ヘルパ（Store 版と Caller 版） ---

pub(crate) fn guest_alloc(
    store: &mut Store<HostState>,
    instance: &Instance,
    len: u32,
) -> Result<u32, wasmtime::Error> {
    let alloc = instance.get_typed_func::<u32, u32>(&mut *store, "alloc")?;
    alloc.call(&mut *store, len)
}

pub(crate) fn guest_dealloc(
    store: &mut Store<HostState>,
    instance: &Instance,
    ptr: u32,
    len: u32,
) -> Result<(), wasmtime::Error> {
    let dealloc = instance.get_typed_func::<(u32, u32), ()>(&mut *store, "dealloc")?;
    dealloc.call(&mut *store, (ptr, len))
}

pub(crate) fn write_mem(
    store: &mut Store<HostState>,
    memory: &Memory,
    ptr: u32,
    bytes: &[u8],
) -> Result<(), wasmtime::Error> {
    memory
        .write(store, ptr as usize, bytes)
        .map_err(|e| wasmtime::Error::msg(format!("mem write: {e}")))
}

pub(crate) fn read_mem(
    store: &mut Store<HostState>,
    memory: &Memory,
    ptr: u32,
    len: u32,
) -> Result<Vec<u8>, wasmtime::Error> {
    let mut buf = vec![0u8; len as usize];
    memory
        .read(store, ptr as usize, &mut buf)
        .map_err(|e| wasmtime::Error::msg(format!("mem read: {e}")))?;
    Ok(buf)
}

pub(crate) fn read_mem_caller(
    caller: &mut Caller<'_, HostState>,
    memory: &Memory,
    ptr: u32,
    len: u32,
) -> Result<Vec<u8>, wasmtime::Error> {
    if len as usize > crate::frames::MAX_ARGS_BYTES {
        return Err(wasmtime::Error::msg("host call too large"));
    }
    let mut buf = vec![0u8; len as usize];
    memory
        .read(&mut *caller, ptr as usize, &mut buf)
        .map_err(|e| wasmtime::Error::msg(format!("mem read: {e}")))?;
    Ok(buf)
}

/// 応答をゲスト alloc で確保した領域へ書き、packed ptr/len を返す（0 は失敗）。
pub(crate) fn write_response(
    caller: &mut Caller<'_, HostState>,
    memory: &Memory,
    bytes: &[u8],
) -> u64 {
    let Some(alloc) = caller
        .get_export("alloc")
        .and_then(wasmtime::Extern::into_func)
    else {
        return 0;
    };
    let Ok(alloc) = alloc.typed::<u32, u32>(&*caller) else {
        return 0;
    };
    let Ok(ptr) = alloc.call(&mut *caller, bytes.len() as u32) else {
        return 0;
    };
    if memory.write(&mut *caller, ptr as usize, bytes).is_err() {
        return 0;
    }
    (u64::from(ptr) << 32) | (bytes.len() as u64)
}

pub(crate) fn unpack(packed: u64) -> (u32, u32) {
    (
        ((packed >> 32) & 0xffff_ffff) as u32,
        (packed & 0xffff_ffff) as u32,
    )
}

/// トラップを中断理由へ分類する（fuel/epoch/memory/frame/trap）。
pub(crate) fn classify_trap(store: &mut Store<HostState>, e: &wasmtime::Error) -> ExecOutcome {
    // フレーム違反が記録されていれば最優先（実行破棄・INV-4）。
    if let Some(v) = store.data().frame_violation.clone() {
        return ExecOutcome::terminated(Termination::FrameViolation(v), store);
    }
    let msg = format!("{e}");
    let termination = if let Some(trap) = e.downcast_ref::<wasmtime::Trap>() {
        match trap {
            wasmtime::Trap::OutOfFuel => Termination::Fuel,
            wasmtime::Trap::Interrupt => Termination::Epoch,
            _ => Termination::Trap(msg.clone()),
        }
    } else if msg.contains("out of fuel") || msg.contains("fuel") {
        Termination::Fuel
    } else if msg.contains("epoch") || msg.contains("interrupt") {
        Termination::Epoch
    } else if msg.contains("memory") || msg.contains("grow") {
        Termination::Memory
    } else {
        Termination::Trap(msg)
    };
    ExecOutcome::terminated(termination, store)
}

impl ExecOutcome {
    pub(crate) fn trap(msg: String) -> Self {
        ExecOutcome {
            ok: false,
            value: None,
            error: Some((msg.clone(), "internal".into(), false)),
            termination: Termination::Trap(msg),
            logs: Vec::new(),
        }
    }

    pub(crate) fn terminated(termination: Termination, store: &mut Store<HostState>) -> Self {
        let logs = std::mem::take(&mut store.data_mut().logs);
        // error code は中断種別に対応させる（フレーム違反は "frame_violation"・envelope 経路と一致させる）。
        let (msg, code) = match &termination {
            Termination::Fuel => ("fuel exhausted".to_string(), "resource"),
            Termination::Epoch => ("time limit exceeded".to_string(), "resource"),
            Termination::Memory => ("memory limit exceeded".to_string(), "resource"),
            Termination::FrameViolation(v) => (format!("frame violation: {v}"), "frame_violation"),
            Termination::Cancelled => ("cancelled".to_string(), "cancelled"),
            Termination::Trap(m) => (m.clone(), "internal"),
            Termination::Completed => ("completed".to_string(), "internal"),
        };
        ExecOutcome {
            ok: false,
            value: None,
            error: Some((msg, code.to_string(), false)),
            termination,
            logs,
        }
    }

    pub(crate) fn from_envelope(envelope: &str, store: &mut Store<HostState>) -> Self {
        let logs = std::mem::take(&mut store.data_mut().logs);
        // フレーム違反（未知 API・最大呼出超過等）が記録されていたら、ゲストがエラーを握り潰して
        // ok=true を返してきても**成功として受理しない**（侵害されたフレームを成功扱いにしない・PIT-35）。
        if let Some(violation) = store.data().frame_violation.clone() {
            return ExecOutcome {
                ok: false,
                value: None,
                error: Some((
                    format!("frame violation: {violation}"),
                    "frame_violation".to_string(),
                    false,
                )),
                termination: Termination::FrameViolation(violation),
                logs,
            };
        }
        let parsed: Value = serde_json::from_str(envelope).unwrap_or(Value::Null);
        let ok = parsed.get("ok").and_then(Value::as_bool).unwrap_or(false);
        if ok {
            ExecOutcome {
                ok: true,
                value: Some(parsed.get("value").cloned().unwrap_or(Value::Null)),
                error: None,
                termination: Termination::Completed,
                logs,
            }
        } else {
            let err = parsed.get("error");
            let message = err
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("script failed")
                .to_string();
            let code = err
                .and_then(|e| e.get("code"))
                .and_then(Value::as_str)
                .unwrap_or("internal")
                .to_string();
            let retryable = err
                .and_then(|e| e.get("retryable"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            ExecOutcome {
                ok: false,
                value: None,
                error: Some((message, code, retryable)),
                termination: Termination::Completed,
                logs,
            }
        }
    }
}

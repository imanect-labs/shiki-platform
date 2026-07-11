//! wasmtime 上で QuickJS ゲスト wasm を駆動する実行エンジン（script.md §4）。
//!
//! 二層隔離: ゲスト JS の脱出バグも wasm メモリ空間に閉じ、wasmtime の fuel（CPU）・
//! メモリ上限・epoch interruption がそのままリソース制限になる。WASI は与えない
//! （fs/net へ到達できない・受け入れ条件「外界不達」の前提）。
//!
//! **1 実行 = 1 Store/Instance 使い捨て**（テナント間で状態を共有しない）。`Module` の
//! コンパイルはプロセス起動時 1 回（[`ScriptEngine::new`]）で、以降の実行はプリウォーム済み。
//!
//! ゲスト ABI（guest/src/lib.rs と合意）:
//! - export `alloc(len)->ptr` / `dealloc(ptr,len)` / `exec(js_ptr,js_len,in_ptr,in_len)->u64`
//! - import `shiki.hostcall(req_ptr,req_len)->u64`（同期能力呼び出し・深さ 1）

use std::time::Duration;

use serde_json::Value;
use wasmtime::{Caller, Config, Engine, Instance, Linker, Module, Store};

use crate::frames::{FrameError, FrameValidator};
use crate::host::{HostCall, HostResponse};
use crate::runtime_io::{
    classify_trap, guest_alloc, guest_dealloc, read_mem, read_mem_caller, unpack, write_mem,
    write_response,
};

/// 埋め込みゲスト wasm（scripts/build-qjs-guest.sh で再現ビルドし vendor したもの）。
pub const GUEST_WASM: &[u8] = include_bytes!("../assets/shiki_qjs_guest.wasm");

/// 実行リソース上限（ノード設定は縮小のみ・script.md §4.3）。
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// CPU 予算（fuel・枯渇で強制中断）。
    pub fuel: u64,
    /// 線形メモリ上限（バイト）。
    pub memory_bytes: usize,
    /// wall-clock 上限（epoch interruption）。
    pub epoch_deadline: Duration,
    /// ホスト呼び出し回数上限（フレーム往復）。
    pub max_host_calls: u64,
}

impl Default for Limits {
    /// script.md §4.3 の初期値（memory 128MB・wall-clock 30s・host calls 1000）。
    /// fuel は「単純ループ 1 秒相当」を較正する暫定値（運用で調整）。
    fn default() -> Self {
        Limits {
            fuel: 2_000_000_000,
            memory_bytes: 128 * 1024 * 1024,
            epoch_deadline: Duration::from_secs(30),
            max_host_calls: 1000,
        }
    }
}

/// 中断理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Termination {
    /// 正常終了（結果あり）。
    Completed,
    /// fuel 枯渇。
    Fuel,
    /// メモリ上限。
    Memory,
    /// wall-clock 超過（epoch）。
    Epoch,
    /// フレーム違反（敵対的フレーム・INV-4）。
    FrameViolation(String),
    /// ホスト側キャンセル。
    Cancelled,
    /// 内部エラー（トラップ等）。
    Trap(String),
}

/// 実行結果。
#[derive(Debug, Clone)]
pub struct ExecOutcome {
    /// 成否。
    pub ok: bool,
    /// ok=true のとき main 戻り値。
    pub value: Option<Value>,
    /// ok=false のときのメッセージ／コード。
    pub error: Option<(String, String, bool)>,
    /// 中断理由。
    pub termination: Termination,
    /// ゲストの Shiki.log 出力（行）。
    pub logs: Vec<String>,
}

/// ホスト呼び出しを同期処理するコールバック（実行スレッド内でブロッキング実行される）。
///
/// gRPC サーバはこれを「ワーカーへ HostCall を送り HostCallResult を待つ」実装で埋める。
/// wasmtime の `func_wrap` が `Send + 'static` を要求するため owned box を受ける。
pub type HostFn = Box<dyn FnMut(&HostCall) -> HostResponse + Send>;

/// epoch ティック間隔。共有エンジンの epoch を一定間隔で単調増加させ、各 Store は自分の
/// deadline を「ティック数」で設定する。これにより 1 実行の deadline 到来が他実行を巻き込まない。
const EPOCH_TICK: Duration = Duration::from_millis(1);

/// deadline（実時間）を epoch ティック数へ換算する（最低 1 ティック）。
fn deadline_ticks(deadline: Duration) -> u64 {
    let ticks = deadline.as_millis() / EPOCH_TICK.as_millis().max(1);
    u64::try_from(ticks).unwrap_or(u64::MAX).max(1)
}

/// エンジン寿命の epoch ティッカ（ドロップ時に停止）。
struct EpochTicker {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for EpochTicker {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// プリウォーム済みの実行エンジン（`Module` を保持・複数実行を並行処理可）。
pub struct ScriptEngine {
    engine: Engine,
    module: Module,
    // エンジンと同じ寿命の単調 epoch ティッカ（並行実行の deadline を隔離する）。
    _ticker: EpochTicker,
}

impl ScriptEngine {
    /// 埋め込みゲスト wasm をコンパイルしてエンジンを作る（プロセス起動時 1 回）。
    pub fn new() -> Result<Self, String> {
        let mut config = Config::new();
        config.consume_fuel(true);
        config.epoch_interruption(true);
        let engine = Engine::new(&config).map_err(|e| format!("engine: {e}"))?;
        let module = Module::new(&engine, GUEST_WASM).map_err(|e| format!("module: {e}"))?;

        // 単一の単調ティッカ: EPOCH_TICK ごとに epoch を +1 する。各 Store は自分の deadline を
        // ティック数で設定するため、あるランの deadline 到来（=その Store のティック超過）が
        // 他ランの Store を早期に interrupt しない（テナント間の巻き込み kill を防ぐ・PIT-35）。
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ticker_engine = engine.clone();
        let ticker_stop = std::sync::Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            while !ticker_stop.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(EPOCH_TICK);
                ticker_engine.increment_epoch();
            }
        });
        Ok(ScriptEngine {
            engine,
            module,
            _ticker: EpochTicker {
                stop,
                handle: Some(handle),
            },
        })
    }

    /// エンジンの epoch を +1 する（テスト補助）。
    pub fn increment_epoch(&self) {
        self.engine.increment_epoch();
    }

    /// クローン可能な epoch ハンドル（テスト補助）。
    pub fn engine_handle(&self) -> Engine {
        self.engine.clone()
    }

    /// スクリプトを 1 回実行する（使い捨て Store/Instance）。
    ///
    /// `exec_id`/`compiled_js`/`input` は検証済み前提の実行入力。`host_fn` は能力呼び出しを
    /// 同期処理する（runtime プロセスでは gRPC 往復、テストでは直呼び）。
    pub fn run(
        &self,
        exec_id: &str,
        compiled_js: &str,
        input_json: &str,
        limits: Limits,
        host_fn: HostFn,
    ) -> ExecOutcome {
        let state = HostState {
            validator: FrameValidator::new(exec_id, limits.max_host_calls),
            logs: Vec::new(),
            frame_violation: None,
            limiter: MemLimiter {
                max_bytes: limits.memory_bytes,
            },
            host_fn,
            // LCG 初期シード（exec_id 由来で実行ごとに変える・OS エントロピー不使用）。
            rng: seed_from(exec_id),
            clock: 0,
        };
        let mut store = Store::new(&self.engine, state);
        store.set_fuel(limits.fuel).ok();
        // epoch deadline を**この実行専用のティック数**で設定する。共有ティッカが EPOCH_TICK ごとに
        // epoch を +1 するので、この Store は自分の deadline 相当のティックが経過して初めて trap する
        // （他ランの deadline 到来に巻き込まれない）。
        let ticks = deadline_ticks(limits.epoch_deadline);
        store.set_epoch_deadline(ticks);
        store.limiter(|s: &mut HostState| &mut s.limiter);

        self.run_inner(&mut store, compiled_js, input_json)
    }

    fn run_inner(
        &self,
        store: &mut Store<HostState>,
        compiled_js: &str,
        input_json: &str,
    ) -> ExecOutcome {
        let mut linker = Linker::new(&self.engine);
        // hostcall import: ゲスト線形メモリからリクエストを読み、Store 内の host_fn で処理し、
        // ゲスト alloc で確保した領域へ応答を書き、packed ptr/len を返す（深さ 1 再入）。
        // クロージャは何も捕捉しない（host_fn は caller.data 経由）＝ Send + 'static。
        if let Err(e) = linker.func_wrap(
            "shiki",
            "hostcall",
            move |mut caller: Caller<'_, HostState>, req_ptr: u32, req_len: u32| -> u64 {
                hostcall_bridge(&mut caller, req_ptr, req_len)
            },
        ) {
            return ExecOutcome::trap(format!("linker: {e}"));
        }
        // 最小 WASI（random/clock/stdio のみ・fs/net 系は登録しない）。
        if let Err(e) = crate::wasi_stub::add_to_linker(&mut linker) {
            return ExecOutcome::trap(format!("wasi linker: {e}"));
        }

        let instance = match linker.instantiate(&mut *store, &self.module) {
            Ok(i) => i,
            Err(e) => return classify_trap(store, &e),
        };
        let result = Self::invoke_exec(store, &instance, compiled_js, input_json);
        match result {
            Ok(envelope) => ExecOutcome::from_envelope(&envelope, store),
            Err(e) => classify_trap(store, &e),
        }
    }

    /// ゲストへ compiled_js/input を書き込み exec を呼ぶ。戻りは結果エンベロープ JSON。
    fn invoke_exec(
        store: &mut Store<HostState>,
        instance: &Instance,
        compiled_js: &str,
        input_json: &str,
    ) -> Result<String, wasmtime::Error> {
        let memory = instance
            .get_memory(&mut *store, "memory")
            .ok_or_else(|| wasmtime::Error::msg("guest memory 不在"))?;
        let js_bytes = compiled_js.as_bytes();
        let in_bytes = input_json.as_bytes();
        let js_ptr = guest_alloc(store, instance, js_bytes.len() as u32)?;
        write_mem(store, &memory, js_ptr, js_bytes)?;
        let in_ptr = guest_alloc(store, instance, in_bytes.len() as u32)?;
        write_mem(store, &memory, in_ptr, in_bytes)?;

        let exec = instance.get_typed_func::<(u32, u32, u32, u32), u64>(&mut *store, "exec")?;
        let packed = exec.call(
            &mut *store,
            (js_ptr, js_bytes.len() as u32, in_ptr, in_bytes.len() as u32),
        )?;
        let (ptr, len) = unpack(packed);
        // 結果サイズ上限（256KB・step-output cap）。ゲストが報告した len をそのまま read すると
        // wasm メモリ上限内でも数 MB を host へ確保させられる。上限超過は読まずに trap する。
        const MAX_RESULT_BYTES: u32 = 256 * 1024;
        if len > MAX_RESULT_BYTES {
            let _ = guest_dealloc(store, instance, ptr, len);
            return Err(wasmtime::Error::msg(format!(
                "result too large: {len} bytes > {MAX_RESULT_BYTES}"
            )));
        }
        let bytes = read_mem(store, &memory, ptr, len)?;
        // 結果領域を解放（ゲスト側 alloc で確保されている）。
        let _ = guest_dealloc(store, instance, ptr, len);
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// Store が持つホスト側状態。
pub(crate) struct HostState {
    validator: FrameValidator,
    pub(crate) logs: Vec<String>,
    pub(crate) frame_violation: Option<String>,
    limiter: MemLimiter,
    /// 能力呼び出しの委譲先（gRPC 往復 or テスト直呼び）。
    host_fn: HostFn,
    /// 最小 WASI スタブ用の LCG 疑似乱数状態（random_get・OS エントロピー不使用）。
    pub(crate) rng: u64,
    /// 最小 WASI スタブ用の単調カウンタ（clock_time_get・実時計不使用）。
    pub(crate) clock: u64,
}

/// exec_id からの LCG 初期シード（決定論は要求しないが実行ごとに変える）。
fn seed_from(exec_id: &str) -> u64 {
    // FNV-1a（64bit）で exec_id をハッシュしてシードにする。
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in exec_id.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h | 1
}

/// wasmtime の ResourceLimiter（メモリ growth を上限で拒否）。
struct MemLimiter {
    max_bytes: usize,
}

impl wasmtime::ResourceLimiter for MemLimiter {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        // ノード設定の上限とモジュール宣言の maximum の厳しい方を採用する。
        if desired > self.max_bytes {
            return Ok(false);
        }
        Ok(maximum.is_none_or(|m| desired <= m))
    }
    fn table_growing(
        &mut self,
        _current: usize,
        _desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(true)
    }
}

/// hostcall import の本体: リクエスト読取 → フレーム検証 → host_fn → 応答書き込み。
fn hostcall_bridge(caller: &mut Caller<'_, HostState>, req_ptr: u32, req_len: u32) -> u64 {
    let Some(memory) = caller
        .get_export("memory")
        .and_then(wasmtime::Extern::into_memory)
    else {
        return 0;
    };
    let Ok(req_bytes) = read_mem_caller(caller, &memory, req_ptr, req_len) else {
        return 0;
    };
    let response = process_frame(caller, &req_bytes);
    let env = response.to_envelope().to_string();
    write_response(caller, &memory, env.as_bytes())
}

/// リクエストフレームをパース・検証し host_fn で処理する。違反は状態に記録し Err を返す。
fn process_frame(caller: &mut Caller<'_, HostState>, req_bytes: &[u8]) -> HostResponse {
    #[derive(serde::Deserialize)]
    struct Frame {
        api: String,
        #[serde(default)]
        args: Value,
    }
    let frame: Frame = if let Ok(f) = serde_json::from_slice(req_bytes) {
        f
    } else {
        record_violation(caller, "invalid host call json");
        return HostResponse::Err {
            message: "invalid frame".into(),
            code: "frame".into(),
            retryable: false,
        };
    };
    // seq はゲストが管理しないため、ホスト側 validator が単調 seq を採番する
    // （フレーム検証: api 閉集合・回数上限・args サイズ）。
    let state = caller.data_mut();
    let next_seq = state.validator_next_seq();
    let call = HostCall {
        exec_id: state.validator_exec_id(),
        seq: next_seq,
        api: frame.api,
        args: frame.args,
    };
    if let Err(e) = state.validator.check(&call) {
        let msg = e.to_string();
        record_violation(caller, &msg);
        return frame_error_response(&e);
    }
    // log は host_fn を呼ばずここで消費する（Shiki.log.*）。
    // 行数 100・1 行 16KB の上限を適用する（wasm メモリ制限の外＝ホスト側 RAM を食い潰させない・DoS 対策）。
    if call.api == "log" {
        const MAX_LOG_LINES: usize = 100;
        const MAX_LOG_BYTES: usize = 16 * 1024;
        let mut line = call
            .args
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if line.len() > MAX_LOG_BYTES {
            // UTF-8 境界へ丸めて truncate（char 境界外での panic を避ける）。
            let cut = (0..=MAX_LOG_BYTES)
                .rev()
                .find(|&i| line.is_char_boundary(i))
                .unwrap_or(0);
            line.truncate(cut);
            line.push_str("…[truncated]");
        }
        let logs = &mut caller.data_mut().logs;
        if logs.len() < MAX_LOG_LINES {
            logs.push(line);
        } else if logs.len() == MAX_LOG_LINES {
            logs.push("…[log truncated: 100 行上限]".to_string());
        }
        return HostResponse::Ok(Value::Null);
    }
    // 委譲先（Store 内 host_fn）へ渡す。単一文で data_mut を借りて即呼ぶ。
    (caller.data_mut().host_fn)(&call)
}

fn record_violation(caller: &mut Caller<'_, HostState>, msg: &str) {
    let state = caller.data_mut();
    if state.frame_violation.is_none() {
        state.frame_violation = Some(msg.to_string());
    }
}

fn frame_error_response(e: &FrameError) -> HostResponse {
    HostResponse::Err {
        message: e.to_string(),
        code: "frame".into(),
        retryable: false,
    }
}

impl HostState {
    fn validator_exec_id(&self) -> String {
        self.validator.exec_id().to_string()
    }
    fn validator_next_seq(&self) -> u64 {
        self.validator.peek_next_seq()
    }
}

#[cfg(test)]
impl HostState {
    /// テスト専用: WASI スタブ / `terminated` エンベロープ経路を Store 越しに直接駆動するための
    /// 最小状態を作る（`host_fn` は WASI テストでは呼ばれない）。
    pub(crate) fn new_for_test(host_fn: HostFn) -> Self {
        HostState {
            validator: FrameValidator::new("test-exec", 1000),
            logs: Vec::new(),
            frame_violation: None,
            limiter: MemLimiter {
                max_bytes: 128 * 1024 * 1024,
            },
            host_fn,
            rng: 0x1234_5678_9abc_def0,
            clock: 0,
        }
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;

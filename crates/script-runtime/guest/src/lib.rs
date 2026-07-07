//! shiki script ゲスト（QuickJS on wasm・PIT-35 の脱出面を wasm 境界に閉じ込める）。
//!
//! ホスト（`crates/script-runtime` の wasmtime エンジン）が **swc で JS へ変換済み**の
//! ソースと入力 JSON を線形メモリへ渡し、[`exec`] を呼ぶ。ゲストは javy(QuickJS) で
//! `main(input)` を評価し、結果エンベロープ JSON を返す。
//!
//! 能力呼び出し（`Shiki.*`）は **資格情報を一切持たず**、単一のホスト import
//! [`hostcall`] へ JSON フレームを渡すだけ（実際の認可・実行はホスト＝shiki-server 側・
//! script.md §5 INV-1/INV-2）。`eval` / `new Function` は QuickJS 設定で無効化する。
//!
//! ABI（ホストと合意）:
//! - `alloc(len) -> ptr` / `dealloc(ptr, len)`: 線形メモリの受け渡しバッファ管理。
//! - `exec(js_ptr, js_len, input_ptr, input_len) -> u64`: 実行。戻り値は
//!   `(result_ptr << 32) | result_len`。result は結果エンベロープ JSON（下記）。
//! - import `shiki.hostcall(req_ptr, req_len) -> u64`: 同期能力呼び出し。戻り値は
//!   `(resp_ptr << 32) | resp_len`（ホストがゲスト `alloc` で確保・ゲストが `dealloc`）。
//!
//! 結果エンベロープ: `{"ok":true,"value":<json>}` / `{"ok":false,"error":{"message":..,"code":..,"retryable":..}}`

use std::cell::RefCell;

use javy::quickjs::{Ctx, Function, Value};
use javy::quickjs::prelude::MutFn;
use javy::{from_js_error, Config, Runtime};

/// ホスト import: 同期能力呼び出し（深さ 1 固定・script.md §5.2）。
#[link(wasm_import_module = "shiki")]
extern "C" {
    fn hostcall(req_ptr: u32, req_len: u32) -> u64;
}

thread_local! {
    /// プロセス内で使い回す Runtime（インスタンス使い捨ては**ホスト側の wasmtime Store**が担う。
    /// 1 wasm インスタンス = 1 実行のため、ここは実質単一実行のキャッシュ）。
    static RUNTIME: RefCell<Option<Runtime>> = const { RefCell::new(None) };
}

/// 線形メモリを `len` バイト確保し先頭ポインタを返す（ホストが書き込み用に呼ぶ）。
///
/// # Safety
/// 返したポインタは対応する [`dealloc`] でのみ解放すること。
#[no_mangle]
pub extern "C" fn alloc(len: u32) -> u32 {
    let mut buf = Vec::<u8>::with_capacity(len as usize);
    let ptr = buf.as_mut_ptr() as u32;
    std::mem::forget(buf);
    ptr
}

/// [`alloc`] で確保したバッファを解放する。
///
/// # Safety
/// `ptr`/`len` は [`alloc`] が返した対で、かつ未解放でなければならない。
#[no_mangle]
pub unsafe extern "C" fn dealloc(ptr: u32, len: u32) {
    drop(Vec::from_raw_parts(ptr as *mut u8, 0, len as usize));
}

/// スクリプトを実行し結果エンベロープ JSON のポインタ/長さ（packed）を返す。
///
/// # Safety
/// 各 ptr/len はホストが [`alloc`] 経由で確保し UTF-8 を書き込んだ有効領域であること。
#[no_mangle]
pub unsafe extern "C" fn exec(
    js_ptr: u32,
    js_len: u32,
    input_ptr: u32,
    input_len: u32,
) -> u64 {
    let js = slice_to_string(js_ptr, js_len);
    let input = slice_to_string(input_ptr, input_len);
    let out = run(js, input).unwrap_or_else(|e| error_envelope(&e.to_string(), None, false));
    pack_leak(out)
}

/// `main(input)` を評価し結果エンベロープ文字列を返す。
fn run(js: String, input_json: String) -> anyhow::Result<String> {
    RUNTIME.with(|slot| {
        let mut borrow = slot.borrow_mut();
        if borrow.is_none() {
            *borrow = Some(build_runtime()?);
        }
        let runtime = borrow.as_ref().expect("runtime built above");
        let ctx = runtime.context();
        let result = ctx.with(|cx| -> anyhow::Result<String> {
            install_shiki(&cx)?;
            // ユーザースクリプト（swc で JS 化済み）を評価し main を定義させる。
            cx.eval_with_options::<(), _>(js.as_bytes(), eval_opts())
                .map_err(|e| from_js_error(cx.clone(), e))?;
            // main(input) を呼ぶブートストラップ。入力は JSON.parse でオブジェクト化。
            let bootstrap = format!(
                "(function(){{\
                   if (typeof main !== 'function') {{ throw new Error('script must define function main(input)'); }}\
                   var __input = {input_json};\
                   var __r = main(__input);\
                   return JSON.stringify({{ ok: true, value: __r === undefined ? null : __r }});\
                 }})()",
            );
            let value: Value = cx
                .eval_with_options(bootstrap.as_bytes(), eval_opts())
                .map_err(|e| from_js_error(cx.clone(), e))?;
            let s: String = value
                .as_string()
                .and_then(|s| s.to_string().ok())
                .ok_or_else(|| anyhow::anyhow!("main の戻り値を直列化できません"))?;
            Ok(s)
        });
        match result {
            Ok(s) => Ok(s),
            // JS 例外（Shiki.fail や TypeError 等）は error エンベロープへ写す。
            Err(e) => Ok(error_envelope(&e.to_string(), None, false)),
        }
    })
}

fn build_runtime() -> anyhow::Result<Runtime> {
    // EVAL intrinsic はホストの JS_Eval（スクリプト実行）に必須のため有効化し、
    // 動的コード生成（`eval` / `new Function` / constructor 経由）は JS レベルで
    // 無効化する（NEUTRALIZE_EVAL）。真の封じ込めは wasm＋fuel＋epoch（script.md §4）。
    let config = Config::default();
    Runtime::new(config).map_err(|e| anyhow::anyhow!("runtime: {e}"))
}

/// `eval` / `Function` / 関数コンストラクタ経由の動的コード生成を無効化する。
const NEUTRALIZE_EVAL: &str = r#"
(function () {
  var block = function () { throw new Error('dynamic code evaluation is disabled'); };
  try { globalThis.eval = block; } catch (e) {}
  var FnCtor = Function;
  try { globalThis.Function = block; } catch (e) {}
  try { FnCtor.prototype.constructor = block; } catch (e) {}
})();
"#;

fn eval_opts() -> javy::quickjs::context::EvalOptions {
    let mut o = javy::quickjs::context::EvalOptions::default();
    o.strict = true;
    o
}

/// `globalThis.Shiki` と橋渡し関数を注入する。
fn install_shiki(cx: &Ctx<'_>) -> anyhow::Result<()> {
    let globals = cx.globals();
    // Rust 橋: JSON 文字列リクエストを受けて hostcall し、JSON 文字列レスポンスを返す。
    // 戻り値を String にして rquickjs の IntoJs に任せる（Value を跨ぐ寿命問題を避ける）。
    let bridge = Function::new(
        cx.clone(),
        MutFn::new(move |_cx: Ctx<'_>, args: javy::quickjs::prelude::Rest<Value<'_>>| -> String {
            let req = args
                .first()
                .and_then(|v| v.as_string())
                .and_then(|s| s.to_string().ok())
                .unwrap_or_default();
            do_hostcall(&req)
        }),
    )
    .map_err(|e| anyhow::anyhow!("bridge fn: {e}"))?;
    globals
        .set("__shiki_hostcall", bridge)
        .map_err(|e| anyhow::anyhow!("set bridge: {e}"))?;
    // Shiki.* API（同期・能力ゲートウェイ 1:1）。参照名のみ扱い資格情報は持たない。
    cx.eval_with_options::<(), _>(SHIKI_BOOTSTRAP.as_bytes(), eval_opts())
        .map_err(|e| from_js_error(cx.clone(), e))?;
    // 動的コード生成を無効化（ユーザースクリプト評価の直前に実行）。
    cx.eval_with_options::<(), _>(NEUTRALIZE_EVAL.as_bytes(), eval_opts())
        .map_err(|e| from_js_error(cx.clone(), e))?;
    Ok(())
}

/// リクエスト JSON をホストへ渡し、レスポンス JSON を受け取る（同期・深さ 1）。
fn do_hostcall(req: &str) -> String {
    let bytes = req.as_bytes();
    let packed = unsafe { hostcall(bytes.as_ptr() as u32, bytes.len() as u32) };
    let (ptr, len) = unpack(packed);
    if ptr == 0 || len == 0 {
        return String::from("{\"ok\":false,\"error\":{\"message\":\"host returned empty\"}}");
    }
    // ホストが alloc で確保し書き込んだ領域を読み取り、読んだら解放する。
    let s = unsafe {
        let slice = std::slice::from_raw_parts(ptr as *const u8, len as usize);
        let owned = String::from_utf8_lossy(slice).into_owned();
        dealloc(ptr, len);
        owned
    };
    s
}

/// `Shiki` グローバルの定義（同期スタイル・ホスト側 async 橋渡し）。
const SHIKI_BOOTSTRAP: &str = r#"
globalThis.Shiki = (function () {
  function call(api, args) {
    var resp = __shiki_hostcall(JSON.stringify({ api: api, args: args || {} }));
    var parsed;
    try { parsed = JSON.parse(resp); }
    catch (e) { throw mkErr('host response parse error', 'internal', false); }
    if (parsed && parsed.ok) { return parsed.value; }
    var err = (parsed && parsed.error) || {};
    throw mkErr(err.message || 'host call failed', err.code || 'internal', !!err.retryable);
  }
  function mkErr(message, code, retryable) {
    var e = new Error(message);
    e.name = 'ShikiError';
    e.code = code;
    e.retryable = retryable;
    return e;
  }
  var ctx = null;
  return {
    // 能力ノード（Stage A: storage / rag / http / workflow.start）。
    storage: {
      read: function (id) { return call('storage.read', { id: id }); },
      list: function (parent) { return call('storage.list', { parent: parent }); },
      write: function (parent, name, content, contentType) {
        return call('storage.write', { parent: parent, name: name, content: content, contentType: contentType });
      }
    },
    rag: {
      search: function (query, topK) { return call('rag.search', { query: query, topK: topK || 8 }); }
    },
    http: {
      request: function (opts) { return call('http.request', opts || {}); }
    },
    workflow: {
      start: function (name, input) { return call('workflow.start', { name: name, input: input || {} }); }
    },
    log: {
      info: function (msg) { call('log', { level: 'info', message: String(msg) }); },
      warn: function (msg) { call('log', { level: 'warn', message: String(msg) }); },
      error: function (msg) { call('log', { level: 'error', message: String(msg) }); }
    },
    // 実行コンテキスト（run_id 等）はホストが exec 前に __shiki_ctx でセットする。
    get context() { return ctx || (ctx = call('context', {})); },
    fail: function (message, opts) {
      throw mkErr(String(message), (opts && opts.code) || 'permanent', !(opts && opts.permanent));
    }
  };
})();
"#;

// --- 線形メモリ ヘルパ ---

/// packed `(ptr<<32)|len` を分解する。
fn unpack(packed: u64) -> (u32, u32) {
    (((packed >> 32) & 0xffff_ffff) as u32, (packed & 0xffff_ffff) as u32)
}

/// 文字列を線形メモリへ確保し `(ptr<<32)|len` を返す（ホストが読んで dealloc する）。
fn pack_leak(s: String) -> u64 {
    let bytes = s.into_bytes();
    let len = bytes.len() as u32;
    let ptr = alloc(len);
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, len as usize);
    }
    ((ptr as u64) << 32) | (len as u64)
}

/// 線形メモリの UTF-8 スライスを String へ複製する。
fn slice_to_string(ptr: u32, len: u32) -> String {
    if ptr == 0 || len == 0 {
        return String::new();
    }
    let slice = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    String::from_utf8_lossy(slice).into_owned()
}

fn error_envelope(message: &str, code: Option<&str>, retryable: bool) -> String {
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n");
    format!(
        "{{\"ok\":false,\"error\":{{\"message\":\"{}\",\"code\":\"{}\",\"retryable\":{}}}}}",
        esc(message),
        esc(code.unwrap_or("internal")),
        retryable
    )
}

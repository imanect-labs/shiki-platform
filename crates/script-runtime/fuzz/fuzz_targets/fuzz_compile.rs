#![no_main]
//! コンパイラ（swc parse/lint/transform）を敵対的入力で fuzz する。
//! compile は任意入力でパニックせず Ok/Err を返すことを担保する（保存時検証 V6 の頑健性）。

use libfuzzer_sys::fuzz_target;
use script_runtime::compile::compile;

fuzz_target!(|data: &[u8]| {
    let s = String::from_utf8_lossy(data);
    let _ = compile(&s);
});

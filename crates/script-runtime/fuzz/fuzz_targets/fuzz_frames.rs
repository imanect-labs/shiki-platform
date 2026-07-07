#![no_main]
//! フレーム検証を敵対的入力で fuzz する（INV-4・PIT-35）。
//! validate_host_call は決してパニックせず、常に Ok/Err を返すことを担保する。

use libfuzzer_sys::fuzz_target;
use script_runtime::frames::{validate_host_call, FrameValidator};

fuzz_target!(|data: &[u8]| {
    let s = String::from_utf8_lossy(data);
    let mut v = FrameValidator::new("e1", 1000);
    // api 名と args_json を入力から分割して検証（パニックしないこと）。
    let (api, args) = s.split_once('\n').unwrap_or(("storage.read", &s));
    let _ = validate_host_call(&mut v, "e1", 1, api, args);
    // exec_id 不一致も試す。
    let _ = validate_host_call(&mut v, "other", 2, api, args);
});

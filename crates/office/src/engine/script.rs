//! kit へ送る固定ソースの組み立て（#725・PIT-68 ⑤）。
//!
//! `crates/office/engine/*.js` をビルド時に埋め込み、`function (req) { … }` 1 個に連結する。
//! LLM の出力はこのソースへ混ざらない（`executescript` の引数 JSON としてだけ渡る）。

use std::sync::LazyLock;

const CORE: &str = include_str!("../../engine/core.js");
const CALC_READ: &str = include_str!("../../engine/calc_read.js");
const CALC_APPLY: &str = include_str!("../../engine/calc_apply.js");
const WRITER_READ: &str = include_str!("../../engine/writer_read.js");
const WRITER_APPLY: &str = include_str!("../../engine/writer_apply.js");

/// 連結済みのエンジン（全 action を含む 1 つの関数式）。
pub static ENGINE_SOURCE: LazyLock<String> = LazyLock::new(|| {
    let modules = [CORE, CALC_READ, CALC_APPLY, WRITER_READ, WRITER_APPLY];
    let mut src = String::with_capacity(modules.iter().map(|m| m.len()).sum::<usize>() + 128);
    src.push_str("function (req) {\n\"use strict\";\n");
    for module in modules {
        src.push_str(module);
        src.push('\n');
    }
    src.push_str("return __shiki_main(req);\n}");
    src
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_is_one_function_expression() {
        assert!(ENGINE_SOURCE.starts_with("function (req) {"));
        assert!(ENGINE_SOURCE
            .trim_end()
            .ends_with("return __shiki_main(req);\n}"));
        // 各 action が登録されている。
        for action in [
            "engine.ping",
            "calc.overview",
            "calc.read_range",
            "calc.apply",
        ] {
            assert!(
                ENGINE_SOURCE.contains(&format!("HANDLERS[\"{action}\"]")),
                "{action}"
            );
        }
    }
}

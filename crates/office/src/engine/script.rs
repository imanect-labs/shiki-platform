//! kit へ送る固定ソースの組み立て（#725・PIT-68 ⑤）。
//!
//! `crates/office/engine/*.js` をビルド時に埋め込み、`function (req) { … }` 1 個に連結する。
//! LLM の出力はこのソースへ混ざらない（`executescript` の引数 JSON としてだけ渡る）。

use std::sync::LazyLock;

const CORE: &str = include_str!("../../engine/core.js");
const CHART: &str = include_str!("../../engine/chart.js");
const CALC_READ: &str = include_str!("../../engine/calc_read.js");
const CALC_APPLY: &str = include_str!("../../engine/calc_apply.js");
const WRITER_READ: &str = include_str!("../../engine/writer_read.js");
const WRITER_APPLY: &str = include_str!("../../engine/writer_apply.js");
const WRITER_TABLE: &str = include_str!("../../engine/writer_table.js");
const WRITER_CHART: &str = include_str!("../../engine/writer_chart.js");

/// 連結済みのエンジン（全 action を含む 1 つの関数式）。
pub static ENGINE_SOURCE: LazyLock<String> = LazyLock::new(|| {
    let modules = [
        CORE,
        CHART,
        CALC_READ,
        CALC_APPLY,
        WRITER_READ,
        WRITER_APPLY,
        WRITER_TABLE,
        WRITER_CHART,
    ];
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

    /// モジュールは 1 つの関数の中へ連結されるので、最上位の名前が重なると kit の中で初めて
    /// 「invalid redefinition of lexical identifier」で落ちる。ここで先に見つける。
    #[test]
    fn modules_do_not_redeclare_top_level_names() {
        let mut seen = std::collections::HashSet::new();
        for line in ENGINE_SOURCE.lines() {
            let Some(rest) = ["const ", "let ", "function "]
                .iter()
                .find_map(|k| line.strip_prefix(k))
            else {
                continue;
            };
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            assert!(
                seen.insert(name.clone()),
                "最上位の名前「{name}」が重複しています"
            );
        }
    }

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

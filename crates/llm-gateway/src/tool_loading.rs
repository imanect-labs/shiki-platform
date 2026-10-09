//! 遅延ツール（[`ToolDef::defer_loading`]）の読み込み状態を履歴から求める純関数群。
//!
//! 読み込み状態は**どこにも保存しない**。履歴中の [`Block::ToolResult::tool_references`] が
//! 唯一の正で、毎リクエストここから導く。チェックポイントから再開しても同じ集合に戻り、
//! 剪定（本文の畳み込み）でも参照は消えない。
//!
//! ネイティブに参照を展開できるプロバイダ（Anthropic）はこれを使わず、全定義を
//! `defer_loading` 付きで送る。展開できないプロバイダ（OpenAI 互換）は
//! [`context_tools`] の結果だけを `tools` に載せる。

use std::collections::HashSet;

use crate::model::{Block, Message, ToolDef};

/// 履歴中で参照された（＝読み込まれた）ツール名を、**初出順・重複なし**で返す。
#[must_use]
pub fn referenced_tools(messages: &[Message]) -> Vec<&str> {
    let mut seen = HashSet::new();
    messages
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|b| match b {
            Block::ToolResult {
                tool_references, ..
            } => Some(tool_references),
            _ => None,
        })
        .flatten()
        .map(String::as_str)
        .filter(|name| seen.insert(*name))
        .collect()
}

/// モデルの文脈に載せるツール定義（非遅延 → 読み込み済みの遅延を初出順）。
///
/// 並びは**追記のみ**で伸びる: 新しく読み込んだツールは末尾に足され、既存の並びは
/// 変わらない（prefix cache を読み込みの 1 回以外で壊さない）。参照先が `tools` に無い
/// 名前は無視する（提示していないツールを参照から復活させない）。
#[must_use]
pub fn context_tools<'a>(tools: &'a [ToolDef], messages: &[Message]) -> Vec<&'a ToolDef> {
    let mut out: Vec<&ToolDef> = tools.iter().filter(|t| !t.defer_loading).collect();
    if out.len() == tools.len() {
        return out;
    }
    for name in referenced_tools(messages) {
        if let Some(t) = tools.iter().find(|t| t.defer_loading && t.name == name) {
            out.push(t);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Role;
    use serde_json::json;

    fn def(name: &str, defer: bool) -> ToolDef {
        let mut d = ToolDef::new(name, "d", json!({"type": "object"}));
        d.defer_loading = defer;
        d
    }

    fn loaded(id: &str, names: &[&str]) -> Message {
        Message {
            role: Role::Tool,
            content: vec![Block::ToolResult {
                tool_use_id: id.into(),
                content: String::new(),
                is_error: false,
                tool_references: names.iter().map(|s| (*s).to_string()).collect(),
            }],
        }
    }

    fn names<'a>(defs: &[&'a ToolDef]) -> Vec<&'a str> {
        defs.iter().map(|d| d.name.as_str()).collect()
    }

    #[test]
    fn without_deferred_tools_everything_is_in_context() {
        let tools = vec![def("a", false), def("b", false)];
        assert_eq!(names(&context_tools(&tools, &[])), ["a", "b"]);
    }

    #[test]
    fn deferred_tools_appear_only_after_reference_in_first_reference_order() {
        let tools = vec![
            def("a", false),
            def("x", true),
            def("y", true),
            def("z", true),
        ];
        assert_eq!(names(&context_tools(&tools, &[])), ["a"]);
        // 定義順（x, y, z）ではなく**読み込み順**（z, x）で末尾に足される。
        let history = vec![loaded("t1", &["z"]), loaded("t2", &["x", "z"])];
        assert_eq!(names(&context_tools(&tools, &history)), ["a", "z", "x"]);
    }

    #[test]
    fn references_to_unknown_or_eager_tools_are_ignored() {
        let tools = vec![def("a", false), def("x", true)];
        let history = vec![loaded("t1", &["ghost", "a", "x"])];
        // ghost は提示していない・a は既に先頭にある（二重に載せない）。
        assert_eq!(names(&context_tools(&tools, &history)), ["a", "x"]);
    }
}

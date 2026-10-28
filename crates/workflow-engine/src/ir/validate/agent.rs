//! AI エージェント（agent.invoke・BUG-6）の保存時検証（行数ゲート対応で分離）。
//!
//! - V3: 道具（`allowed_tools`）は今のステージで選べるもの（`ir.unknown_tool`）で、要るスコープが
//!   declared_scopes にあること（`ir.missing_scope`・保存の入口で補われる）。モデルは llm.invoke と同じ照合。
//! - V3: web.fetch を選んだら、`egress_allowlist`（その宛先）は空にできず（`ir.missing_egress`）、形が正しく
//!   （`ir.bad_host`）、管理者が決める web の天井の部分集合（`ir.host_outside_ceiling`）。web.fetch を選んで
//!   いなければ効かないので照合しない。
//! - V5: 依頼（`instruction`）をリテラルで書くなら文字（`ir.expr_type_error`）。
//! - V7: 手順・時間・トークン・依頼の大きさ・サイトの数の上限（`ir.limit_exceeded`）。
//!
//! エラーは欄に写るよう path（`/params/allowed_tools/<i>` など）を付ける。

use std::collections::BTreeSet;

use super::{Catalog, ValidationError};
use crate::ir::expr::ValueExpr;
use crate::ir::params::AgentInvokeParams;
use crate::ir::WorkflowIr;
use crate::vocab::{
    AgentTool, NodeType, AGENT_DURATION_RANGE_SEC, MAX_AGENT_STEPS, MAX_EGRESS_HOSTS,
    MAX_INSTRUCTION_BYTES,
};

fn agents(ir: &WorkflowIr) -> impl Iterator<Item = (&crate::ir::Node, AgentInvokeParams)> {
    ir.nodes
        .iter()
        .filter(|n| n.node_type == NodeType::AgentInvoke.as_str())
        // 契約に合わないものは V1b が報告する。
        .filter_map(|n| {
            serde_json::from_value::<AgentInvokeParams>(n.params.clone())
                .ok()
                .map(|p| (n, p))
        })
}

/// 外部サイトの書き方として正しいか（ホスト名か `*.` で始まるサブドメインの指定だけ）。
///
/// スキーム・パス・ポート・空白・単独の `*` は拒否する（照合は URL のホスト部の文字列で行う）。
#[must_use]
pub fn valid_host_pattern(pattern: &str) -> bool {
    let host = pattern.strip_prefix("*.").unwrap_or(pattern);
    if host.is_empty() || host.len() > 253 || !host.contains('.') {
        return false;
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    })
}

/// ノードに書いたサイトの指定 `pattern` が、天井の指定 `ceiling` の範囲に収まるか（縮小のみ）。
///
/// ホスト名は天井のどれかに一致すればよい（`*.example.com` は example.com のサブドメインを含む）。
/// `*.sub.example.com` は、天井に同じ指定か、それを含む `*.example.com` があるときだけ収まる。
/// 天井の完全一致のホスト `sub.example.com` は、そのサブドメイン（`*.sub.example.com`）を含まない。
#[must_use]
pub fn pattern_within(pattern: &str, ceiling: &[String]) -> bool {
    ceiling.iter().any(|c| match pattern.strip_prefix("*.") {
        // 天井がワイルドカードのときだけ、ワイルドカードを内側に含められる（完全一致の天井
        // `H` に対して `host_allowed(H, H)` が真になるので、`*.H` を通さないよう先に見る）。
        Some(sub) => {
            let c = c.trim();
            c.eq_ignore_ascii_case(pattern)
                || (c.starts_with("*.") && secrets::host_allowed(c, sub))
        }
        None => secrets::host_allowed(c, pattern),
    })
}

/// V3: 道具・モデル・外部サイト。
pub(super) fn v3(
    ir: &WorkflowIr,
    catalog: &Catalog,
    declared: &BTreeSet<&str>,
    errors: &mut Vec<ValidationError>,
) {
    for (node, p) in agents(ir) {
        let tools = p.allowed_tools.as_deref().unwrap_or_default();
        for (i, tool) in tools.iter().enumerate() {
            let path = format!("/params/allowed_tools/{i}");
            if !tool.available() {
                errors.push(
                    ValidationError::new(
                        "ir.unknown_tool",
                        format!("道具 {} はまだ使えません", tool.as_str()),
                    )
                    .at_node(&node.id)
                    .at_path(path),
                );
                continue;
            }
            let scope = tool.required_scope().as_str();
            if !declared.contains(scope) {
                errors.push(
                    ValidationError::new(
                        "ir.missing_scope",
                        format!(
                            "道具 {} は scope {scope} を要しますが declared_scopes に未宣言です",
                            tool.as_str()
                        ),
                    )
                    .at_node(&node.id)
                    .at_path(path),
                );
            }
        }
        if let Some(e) = super::v3_vocab::check_model(&node.params, catalog) {
            errors.push(e.at_node(&node.id).at_path("/params/model"));
        }
        egress_errors(node, &p, tools, catalog, errors);
    }
}

fn egress_errors(
    node: &crate::ir::Node,
    p: &AgentInvokeParams,
    tools: &[AgentTool],
    catalog: &Catalog,
    errors: &mut Vec<ValidationError>,
) {
    // web.fetch を選んでいなければ効かない（旧 IR の Python 実行の名残りを含めて照合しない）。
    if !tools.contains(&AgentTool::WebFetch) {
        return;
    }
    let at = |e: ValidationError, path: String| e.at_node(&node.id).at_path(path);
    if p.egress_allowlist.is_empty() {
        errors.push(at(
            ValidationError::new(
                "ir.missing_egress",
                "Web ページを読む道具を使うときは、読んでよいサイトを選んでください",
            ),
            "/params/egress_allowlist".into(),
        ));
    }
    if p.egress_allowlist.len() > MAX_EGRESS_HOSTS {
        errors.push(at(
            ValidationError::new(
                "ir.limit_exceeded",
                format!("読んでよいサイトは {MAX_EGRESS_HOSTS} 件までです"),
            ),
            "/params/egress_allowlist".into(),
        ));
    }
    for (i, host) in p.egress_allowlist.iter().enumerate() {
        let path = format!("/params/egress_allowlist/{i}");
        if !valid_host_pattern(host) {
            errors.push(at(
                ValidationError::new(
                    "ir.bad_host",
                    format!(
                        "サイトはホスト名（例: docs.example.com）か *.example.com の形で書いてください: {host}"
                    ),
                ),
                path,
            ));
            continue;
        }
        if let Some(ceiling) = &catalog.web_ceiling {
            if !pattern_within(host, ceiling) {
                errors.push(at(
                    ValidationError::new(
                        "ir.host_outside_ceiling",
                        if ceiling.is_empty() {
                            format!("管理者が外部サイトを許可していません: {host}")
                        } else {
                            format!(
                                "管理者が許可したサイトの外です: {host}（許可されたサイト: {}）",
                                ceiling.join(", ")
                            )
                        },
                    ),
                    path,
                ));
            }
        }
    }
}

/// V5: 依頼をリテラルで書くなら文字（参照・差し込みは実行時に文字かを確かめる）。
pub(super) fn v5(ir: &WorkflowIr, errors: &mut Vec<ValidationError>) {
    for (node, p) in agents(ir) {
        if let ValueExpr::Literal(v) = &p.instruction {
            if !v.is_string() {
                errors.push(
                    ValidationError::new(
                        "ir.expr_type_error",
                        format!("ノード {} の依頼は文字で書いてください", node.id),
                    )
                    .at_node(&node.id)
                    .at_path("/params/instruction"),
                );
            }
        }
    }
}

/// V7: 上限（手順・実行時間・トークン・依頼の大きさ）。
pub(super) fn v7(ir: &WorkflowIr, errors: &mut Vec<ValidationError>) {
    let (min_sec, max_sec) = AGENT_DURATION_RANGE_SEC;
    for (node, p) in agents(ir) {
        let mut push = |path: &str, message: String| {
            errors.push(
                ValidationError::new("ir.limit_exceeded", message)
                    .at_node(&node.id)
                    .at_path(path),
            );
        };
        if let Some(n) = p.max_steps {
            if !(1..=MAX_AGENT_STEPS).contains(&n) {
                push(
                    "/params/max_steps",
                    format!("手順の上限は 1〜{MAX_AGENT_STEPS} 回で指定してください"),
                );
            }
        }
        if p.max_tokens == Some(0) {
            push(
                "/params/max_tokens",
                "トークンの上限は 1 以上で指定してください".into(),
            );
        }
        if let Some(ValueExpr::Literal(v)) = &p.max_duration_sec {
            let ok = v
                .as_u64()
                .is_some_and(|s| (u64::from(min_sec)..=u64::from(max_sec)).contains(&s));
            if !ok {
                // 欄の見出しで言う（ブロックの「時間の上限」の誤りと注記が同じ名前で指す・付録 D の N3.2.3・BUG-96）。
                push(
                    "/params/max_duration_sec",
                    format!(
                        "「AI の作業時間の上限」は {}〜{} 秒の整数にしてください。",
                        super::timeout::grouped(min_sec),
                        super::timeout::grouped(max_sec)
                    ),
                );
            }
        }
        if let ValueExpr::Literal(serde_json::Value::String(s)) = &p.instruction {
            if s.len() > MAX_INSTRUCTION_BYTES {
                push(
                    "/params/instruction",
                    format!(
                        "依頼が長すぎます（{} KB まで）",
                        MAX_INSTRUCTION_BYTES / 1024
                    ),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::super::{validate, Catalog, ValidationError};
    use super::{pattern_within, valid_host_pattern};

    fn agent_ir(params: &Value, scopes: &[&str]) -> Value {
        json!({
            "ir_version": 1, "name": "wf", "declared_scopes": scopes,
            "triggers": [{ "kind": "interactive" }],
            "nodes": [{ "id": "ag", "type": "agent.invoke", "params": params }],
            "edges": []
        })
    }

    fn errors(params: &Value, scopes: &[&str], catalog: &Catalog) -> Vec<ValidationError> {
        validate(&agent_ir(params, scopes), catalog)
            .err()
            .unwrap_or_default()
    }

    /// 旧 IR（依頼・外部サイト・時間だけ）はそのまま通る（ir_version を上げない・D-29）。外部サイトは
    /// web.fetch を選ばない限り効かないので、天井の外でも止めない。
    #[test]
    fn old_ir_still_validates() {
        let catalog = Catalog {
            web_ceiling: Some(vec![]),
            ..Catalog::default()
        };
        for params in [
            json!({ "instruction": "要点をまとめて", "egress_allowlist": [], "max_duration_sec": 300 }),
            json!({ "instruction": "要点をまとめて", "egress_allowlist": ["api.example.com"] }),
        ] {
            let errs = errors(&params, &[], &catalog);
            assert!(errs.is_empty(), "{params}: {errs:#?}");
        }
    }

    /// 未知の道具は V1（選択肢にない値）、予約の道具は V3、宣言の無いスコープは V3（path つき）。
    #[test]
    fn tools_are_checked_with_paths() {
        let errs = errors(
            &json!({ "instruction": "x", "allowed_tools": ["shell"] }),
            &[],
            &Catalog::default(),
        );
        assert!(
            errs.iter().any(|e| e.code == "ir.schema_violation"
                && e.path.as_deref() == Some("/params/allowed_tools/0")),
            "{errs:#?}"
        );
        let errs = errors(
            &json!({ "instruction": "x", "allowed_tools": ["rag.search", "data.query"] }),
            &[],
            &Catalog::default(),
        );
        let at = |code: &str, path: &str| {
            errs.iter()
                .any(|e| e.code == code && e.path.as_deref() == Some(path))
        };
        assert!(
            at("ir.missing_scope", "/params/allowed_tools/0"),
            "{errs:#?}"
        );
        assert!(
            at("ir.unknown_tool", "/params/allowed_tools/1"),
            "{errs:#?}"
        );
        // 書込の道具は語彙に無い（v1 では提示しない・D-29）。
        for write in ["storage.write", "data.record.create", "csv.write"] {
            let errs = errors(
                &json!({ "instruction": "x", "allowed_tools": [write] }),
                &["storage.write", "data.write", "csv.write"],
                &Catalog::default(),
            );
            assert!(
                errs.iter().any(|e| e.code == "ir.schema_violation"),
                "{write}: {errs:#?}"
            );
        }
        let ok = errors(
            &json!({ "instruction": "x", "allowed_tools": ["rag.search", "doc.read", "code.run"] }),
            &["rag.query", "doc.read", "sandbox.exec"],
            &Catalog::default(),
        );
        assert!(ok.is_empty(), "{ok:#?}");
    }

    /// web.fetch の宛先は管理者の天井の部分集合だけ。天井が空なら選べない。
    #[test]
    fn egress_must_stay_inside_the_ceiling() {
        let ceiling = |hosts: &[&str]| Catalog {
            web_ceiling: Some(hosts.iter().map(ToString::to_string).collect()),
            ..Catalog::default()
        };
        let p = |hosts: &[&str]| json!({ "instruction": "x", "allowed_tools": ["web.fetch"], "egress_allowlist": hosts });
        let scopes = ["http.egress"];
        assert!(errors(
            &p(&["docs.example.com"]),
            &scopes,
            &ceiling(&["*.example.com"])
        )
        .is_empty());
        let errs = errors(&p(&["evil.test"]), &scopes, &ceiling(&["*.example.com"]));
        assert!(
            errs.iter().any(|e| e.code == "ir.host_outside_ceiling"
                && e.path.as_deref() == Some("/params/egress_allowlist/0")),
            "{errs:#?}"
        );
        let errs = errors(&p(&["docs.example.com"]), &scopes, &ceiling(&[]));
        assert!(errs.iter().any(|e| e.code == "ir.host_outside_ceiling"));
        // 完全一致の天井に対するサブドメインのワイルドカードは外（保存時にも弾く）。
        let errs = errors(
            &p(&["*.docs.example.com"]),
            &scopes,
            &ceiling(&["docs.example.com"]),
        );
        assert!(
            errs.iter().any(|e| e.code == "ir.host_outside_ceiling"
                && e.path.as_deref() == Some("/params/egress_allowlist/0")),
            "{errs:#?}"
        );
        let errs = errors(&p(&[]), &scopes, &ceiling(&["docs.example.com"]));
        assert!(
            errs.iter().any(|e| e.code == "ir.missing_egress"),
            "{errs:#?}"
        );
        let errs = errors(
            &p(&["https://docs.example.com/x"]),
            &scopes,
            &Catalog::default(),
        );
        assert!(errs.iter().any(|e| e.code == "ir.bad_host"), "{errs:#?}");
    }

    #[test]
    fn host_patterns_and_containment() {
        for ok in ["docs.example.com", "*.example.com", "a-b.example.co.jp"] {
            assert!(valid_host_pattern(ok), "{ok}");
        }
        for bad in [
            "*",
            "example",
            "https://x.example.com",
            "x.example.com:443",
            "x.example.com/a",
            "X.example.com",
            "a b.com",
            "",
        ] {
            assert!(!valid_host_pattern(bad), "{bad}");
        }
        let c = vec!["*.example.com".to_string(), "api.other.test".to_string()];
        assert!(pattern_within("docs.example.com", &c));
        assert!(pattern_within("*.docs.example.com", &c));
        assert!(pattern_within("*.example.com", &c));
        assert!(pattern_within("api.other.test", &c));
        assert!(!pattern_within("example.com", &c));
        assert!(!pattern_within("*.other.test", &c));
        assert!(!pattern_within("evil-example.com", &c));
        // 完全一致の天井は、そのサブドメインのワイルドカードを含まない（PIT-79 ③の縮小のみ）。
        assert!(!pattern_within("*.api.other.test", &c));
        let exact = vec!["docs.example.com".to_string()];
        assert!(pattern_within("docs.example.com", &exact));
        assert!(!pattern_within("*.docs.example.com", &exact));
        assert!(!pattern_within("x.docs.example.com", &exact));
    }

    #[test]
    fn literal_instruction_must_be_text_and_limits_apply() {
        let errs = errors(
            &json!({ "instruction": { "a": 1 } }),
            &[],
            &Catalog::default(),
        );
        assert!(
            errs.iter().any(|e| e.code == "ir.expr_type_error"
                && e.path.as_deref() == Some("/params/instruction")),
            "{errs:#?}"
        );
        let errs = errors(
            &json!({ "instruction": "x", "max_steps": 25, "max_duration_sec": 5, "max_tokens": 0 }),
            &[],
            &Catalog::default(),
        );
        assert!(
            errs.iter()
                .any(|e| e.path.as_deref() == Some("/params/max_duration_sec")
                    && e.message == "「AI の作業時間の上限」は 10〜3,600 秒の整数にしてください。"),
            "{errs:#?}"
        );
        for path in [
            "/params/max_steps",
            "/params/max_duration_sec",
            "/params/max_tokens",
        ] {
            assert!(
                errs.iter()
                    .any(|e| e.code == "ir.limit_exceeded" && e.path.as_deref() == Some(path)),
                "{path}: {errs:#?}"
            );
        }
        // モデルは llm.invoke と同じ照合（組織の許可リスト・path はモデル欄）。
        let catalog = Catalog {
            models: Some(vec!["m1".into()]),
            ..Catalog::default()
        };
        let errs = errors(&json!({ "instruction": "x", "model": "m2" }), &[], &catalog);
        assert!(
            errs.iter().any(|e| e.code == "ir.unknown_model"
                && e.path.as_deref() == Some("/params/model")),
            "{errs:#?}"
        );
    }
}

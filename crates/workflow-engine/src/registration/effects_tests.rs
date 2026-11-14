#![allow(clippy::unwrap_used, clippy::panic, clippy::type_complexity)]

use std::collections::HashMap;

use serde_json::{json, Value};

use super::super::consent::suggested_grants;
use super::super::RegistrationService;
use super::*;
use crate::ir::WorkflowIr;
use crate::vocab::{NodeType, Scope};

const TABLE: &str = "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1";
const FOLDER: &str = "66666666-7777-8888-9999-aaaaaaaaaaaa";
const FILE: &str = "11111111-2222-3333-4444-555555555555";

fn effects(nodes: &Value) -> RunEffects {
    let ir = WorkflowIr::from_json(&json!({
        "ir_version": 1, "name": "wf",
        "declared_scopes": ["data.write", "http.egress"],
        "nodes": nodes, "edges": []
    }))
    .unwrap();
    RegistrationService::run_effects(&ir)
}

fn ir_of(nodes: &Value) -> WorkflowIr {
    WorkflowIr::from_json(&json!({
        "ir_version": 1, "name": "wf", "nodes": nodes, "edges": []
    }))
    .unwrap()
}

/// 書くもの・外への送信・任意の処理・起動先・モデル・お知らせ・宣言スコープが、ブロックごとに挙がる。
#[test]
#[allow(clippy::too_many_lines)] // 種類ごとの期待を 1 つの IR で並べる（どのブロックがどこに挙がるかを一目で見る）。
fn lists_every_kind_of_effect_with_its_block() {
    let e = effects(&json!([
        { "id": "add", "type": "data.record.create", "params": { "table": TABLE, "data": {} } },
        { "id": "save", "type": "storage.write",
          "params": { "folder": FOLDER, "name": "a.txt", "content": "x" } },
        { "id": "save_dyn", "type": "storage.write",
          "params": { "folder": { "$from": "input", "path": "/folder" }, "name": "a", "content": "x" } },
        { "id": "top", "type": "csv.write", "params": { "name": "a", "content": "x" } },
        { "id": "patch", "type": "csv.patch", "params": { "file": FILE, "ops": [] } },
        { "id": "call", "type": "http.request",
          "params": { "url": "https://API.example.com/v1/x", "method": "post" } },
        { "id": "keyed", "type": "http.request",
          "params": { "url": "https://hooks.example.org/a", "secret": { "name": "slack" } } },
        { "id": "broken", "type": "http.request", "params": { "url": "" } },
        { "id": "code", "type": "script.run", "params": { "source": { "inline": "export function main() {}" } } },
        { "id": "kid", "type": "workflow.start", "params": { "name": "child" } },
        { "id": "kid_dyn", "type": "workflow.start",
          "params": { "name": { "$from": "input", "path": "/name" } } },
        { "id": "ask", "type": "llm.invoke", "params": { "model": "m-1", "prompt": "hi" } },
        { "id": "ask_default", "type": "llm.invoke", "params": { "prompt": "hi" } },
        { "id": "agent", "type": "agent.invoke",
          "params": { "instruction": "x",
                      "allowed_tools": ["web.fetch", "rag.search", "web.search", "code.run", "web.fetch"],
                      "egress_allowlist": ["docs.example.com"] } },
        { "id": "decide", "type": "ai.decide",
          "params": { "model": "judge", "state": "x", "questions": [] } },
        { "id": "approve", "type": "human.approval", "params": {} },
        { "id": "read", "type": "data.query", "params": { "table": TABLE } }
    ]));

    // 書くもの（テーブルの名前は入れない・書く先の id と、値の式か省略かを言い分ける）。
    let writes: Vec<(&str, WriteOp, EffectTarget, Option<&str>, bool)> = e
        .writes
        .iter()
        .map(|w| {
            (
                w.node_id.as_str(),
                w.op,
                w.target,
                w.target_id.as_deref(),
                w.decided_at_run,
            )
        })
        .collect();
    assert_eq!(
        writes,
        vec![
            (
                "add",
                WriteOp::Create,
                EffectTarget::Table,
                Some(TABLE),
                false
            ),
            (
                "save",
                WriteOp::Save,
                EffectTarget::Folder,
                Some(FOLDER),
                false
            ),
            ("save_dyn", WriteOp::Save, EffectTarget::Folder, None, true),
            ("top", WriteOp::Save, EffectTarget::Folder, None, false),
            (
                "patch",
                WriteOp::Patch,
                EffectTarget::File,
                Some(FILE),
                false
            ),
        ]
    );
    assert!(e.writes.iter().all(|w| w.target_name.is_none()));
    assert_eq!(e.writes[0].node_type, "data.record.create");

    // 外への送信（シークレットを使わない http.request もホストで挙がる・agent の web の道具）。
    let out: Vec<(&str, OutboundKind, Vec<&str>, bool)> = e
        .outbound
        .iter()
        .map(|o| {
            (
                o.node_id.as_str(),
                o.kind,
                o.hosts.iter().map(String::as_str).collect(),
                o.decided_at_run,
            )
        })
        .collect();
    assert_eq!(
        out,
        vec![
            (
                "call",
                OutboundKind::HttpRequest,
                vec!["api.example.com"],
                false
            ),
            (
                "keyed",
                OutboundKind::HttpRequest,
                vec!["hooks.example.org"],
                false
            ),
            ("broken", OutboundKind::HttpRequest, vec![], true),
            (
                "agent",
                OutboundKind::WebFetch,
                vec!["docs.example.com"],
                false
            ),
            ("agent", OutboundKind::WebSearch, vec![], false),
        ]
    );
    assert_eq!(e.outbound[0].method.as_deref(), Some("POST"));
    assert_eq!(e.outbound[0].secret, None);
    assert_eq!(e.outbound[1].method.as_deref(), Some("GET"));
    assert_eq!(e.outbound[1].secret.as_deref(), Some("slack"));

    // 任意の処理（script.run と agent の code.run）。
    let compute: Vec<(&str, ComputeKind)> = e
        .compute
        .iter()
        .map(|c| (c.node_id.as_str(), c.kind))
        .collect();
    assert_eq!(
        compute,
        vec![
            ("code", ComputeKind::Script),
            ("agent", ComputeKind::AgentCode)
        ]
    );

    // 起動先（リテラルの名前・値の式は実行のときに決まる）。
    let starts: Vec<(&str, Option<&str>)> = e
        .starts
        .iter()
        .map(|s| (s.node_id.as_str(), s.workflow_name.as_deref()))
        .collect();
    assert_eq!(starts, vec![("kid", Some("child")), ("kid_dyn", None)]);

    // モデル（書いていなければ組織の既定 = None）。
    let models: Vec<(&str, &str, Option<&str>)> = e
        .models
        .iter()
        .map(|m| (m.node_id.as_str(), m.node_type.as_str(), m.model.as_deref()))
        .collect();
    assert_eq!(
        models,
        vec![
            ("ask", "llm.invoke", Some("m-1")),
            ("ask_default", "llm.invoke", None),
            ("agent", "agent.invoke", None),
            ("decide", "ai.decide", Some("judge")),
        ]
    );

    // お知らせ（承認の依頼）。読むだけのブロックはどこにも挙がらない。
    assert_eq!(e.notifications.len(), 1);
    assert_eq!(e.notifications[0].node_id, "approve");
    assert!(e.others.is_empty(), "{:?}", e.others);
    assert_eq!(e.declared_scopes, vec!["data.write", "http.egress"]);
}

/// 同意の提案の書く側（委譲で editor を求めるもの）は、どれも要約の書くものに挙がる（単一の定義と食い違わない）。
#[test]
fn every_write_suggestion_is_in_the_writes() {
    let nodes = json!([
        { "id": "c", "type": "data.record.create", "params": { "table": TABLE } },
        { "id": "u", "type": "data.record.update", "params": { "table": TABLE } },
        { "id": "up", "type": "data.record.upsert", "params": { "table": { "$from": "input" } } },
        { "id": "d", "type": "data.record.delete", "params": { "table": TABLE } },
        { "id": "t", "type": "data.transition", "params": { "table": TABLE } },
        { "id": "b", "type": "data.bulk_upsert", "params": { "table": TABLE } },
        { "id": "s", "type": "storage.write", "params": { "folder": FOLDER } },
        { "id": "s2", "type": "storage.write", "params": {} },
        { "id": "cw", "type": "csv.write", "params": { "folder": FOLDER } },
        { "id": "cp", "type": "csv.patch", "params": { "file": FILE } },
        { "id": "q", "type": "data.query", "params": { "table": TABLE } },
        { "id": "r", "type": "storage.read", "params": { "file": FILE } }
    ]);
    let ir = ir_of(&nodes);
    let e = RegistrationService::run_effects(&ir);
    let writes_scopes = [
        Scope::DataWrite.as_str(),
        Scope::StorageWrite.as_str(),
        Scope::CsvWrite.as_str(),
    ];
    let mut checked = 0;
    for g in suggested_grants(&ir) {
        if !writes_scopes.contains(&g.scope.as_str()) {
            continue;
        }
        let node = g.source.strip_prefix("node:").unwrap();
        let hit = e
            .writes
            .iter()
            .find(|w| w.node_id == node)
            .unwrap_or_else(|| panic!("書くものに無い: {g:?}"));
        assert_eq!(hit.target_id, g.object_id, "{g:?}");
        checked += 1;
    }
    assert_eq!(checked, 10);
    // 読むだけのブロックは書くものに入らない。
    assert!(e
        .writes
        .iter()
        .all(|w| w.node_id != "q" && w.node_id != "r"));
}

/// 要求スコープが読むだけでないブロックは、種類を問わずどこかに挙がる（新しいブロックを黙って落とさない）。
#[test]
fn no_block_that_writes_or_sends_is_dropped() {
    for nt in NodeType::ALL {
        let Some(scope) = crate::vocab::required_scope(*nt) else {
            continue;
        };
        let e = effects(&json!([{ "id": "n", "type": nt.as_str(), "params": {} }]));
        let listed = !(e.writes.is_empty()
            && e.outbound.is_empty()
            && e.compute.is_empty()
            && e.starts.is_empty()
            && e.models.is_empty()
            && e.notifications.is_empty()
            && e.others.is_empty());
        assert_eq!(
            listed,
            !reads_only(scope),
            "{} ({})",
            nt.as_str(),
            scope.as_str()
        );
    }
    // 計算のスコープは読むだけではない（予約の sandbox.exec は others に挙がる）。
    let e = effects(&json!([{ "id": "n", "type": "sandbox.exec", "params": {} }]));
    assert_eq!(e.others[0].scope.as_deref(), Some("sandbox.exec"));
    // 語彙に無い種類も読むだけと言い切らない。
    let e = effects(&json!([{ "id": "n", "type": "bogus.node", "params": {} }]));
    assert_eq!(e.others[0].node_type, "bogus.node");
    assert_eq!(e.others[0].scope, None);
}

/// skill.invoke は任意の処理として参照つきで挙がる。
#[test]
fn skills_are_compute() {
    let e = effects(&json!([
        { "id": "sk", "type": "skill.invoke", "params": { "skill": "skill:tidy@1" } }
    ]));
    assert_eq!(e.compute.len(), 1);
    assert_eq!(e.compute[0].kind, ComputeKind::Skill);
    assert_eq!(e.compute[0].skill.as_deref(), Some("skill:tidy@1"));
}

/// 書く先の名前は呼び出し側が入れる（id ごと・見えない id は入れない）。
#[test]
fn target_names_are_filled_by_the_caller() {
    let other = "9c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1";
    let mut e = effects(&json!([
        { "id": "a", "type": "data.record.create", "params": { "table": TABLE } },
        { "id": "b", "type": "data.record.update", "params": { "table": TABLE } },
        { "id": "c", "type": "data.record.delete", "params": { "table": other } },
        { "id": "s", "type": "storage.write", "params": { "folder": FOLDER } }
    ]));
    assert_eq!(e.target_ids(EffectTarget::Table), vec![TABLE, other]);
    assert_eq!(e.target_ids(EffectTarget::Folder), vec![FOLDER]);
    assert!(e.target_ids(EffectTarget::File).is_empty());
    let names: HashMap<String, String> = [(TABLE.to_string(), "受付".to_string())].into();
    e.set_target_names(EffectTarget::Table, &names);
    let got: Vec<Option<&str>> = e.writes.iter().map(|w| w.target_name.as_deref()).collect();
    assert_eq!(got, vec![Some("受付"), Some("受付"), None, None]);
    // 種類が違えば同じ id でも入れない。
    let folders: HashMap<String, String> = [(TABLE.to_string(), "x".to_string())].into();
    e.set_target_names(EffectTarget::Folder, &folders);
    assert_eq!(e.writes[3].target_name, None);
    // 直列化の名前（承認カードが読む）。
    let v = serde_json::to_value(&e).unwrap();
    assert_eq!(v["writes"][0]["op"], "create");
    assert_eq!(v["writes"][0]["target"], "table");
}

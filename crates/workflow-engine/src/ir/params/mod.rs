//! ノード params の typed 契約（codegen が正・ir.md §7）。
//!
//! per-node struct を `deny_unknown_fields` で定義し、**保存時検証 V1 と executor が同一型を
//! 共有する**（契約ドリフトを構造的に排除・Task 10.6「ノード設定パネル契約」）。ts-rs で
//! TS 型へ export し、右パネル UI のフォームはこの型で型検査される（Task 10.12）。
//!
//! 方針:
//! - **実行時に強制されないフィールドは契約に含めない**（UI が効かない制限を約束しない・
//!   fail-closed）。例: `agent.invoke` の `allowed_tools`/`mount_scope`、`http.request` の
//!   `redirect: follow_stripped`、`llm.invoke` の `output_schema` は enforcement 実装時に追加する。
//! - 予約語彙（Stage A 未実装ノード）の params 型は実装フェーズで追加する（ir.md §7.8）。
//! - 検証エラーは `/params/<field>/...` の JSON Pointer 風 path を持ち、dnd がフォーム
//!   フィールドへハイライトを写像できる（ir.md §8 のエラー形式）。

mod ai;
mod control;
mod external;
mod storage;
mod tabular;

pub use ai::{AgentInvokeParams, LlmInvokeParams, SkillInvokeParams};
pub use control::{
    BranchParams, JoinMode, JoinParams, MapItemError, MapParams, SwitchCase, SwitchParams,
    WaitKind, WaitParams, WaitTimeout,
};
pub use external::{
    HttpMethod, HttpRequestParams, HttpSecretRef, RedirectPolicy, ScriptRunParams,
    ScriptSourceSpec, SecretAttach, SecretAttachKind, WorkflowStartParams,
};
pub use storage::{RagSearchParams, StorageListParams, StorageReadParams, StorageWriteParams};
pub use tabular::{CsvPatchParams, CsvQueryParams, CsvWriteParams};

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::vocab::NodeType;

/// typed 契約違反（保存時検証 V1 が `ir.schema_violation` として全件収集する）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamsIssue {
    /// `/params/...` の JSON Pointer 風 path（フォームフィールドへの写像用）。
    pub path: String,
    /// 人向けメッセージ（serde のエラーメッセージそのまま）。
    pub message: String,
}

/// serde_path_to_error の track を `/params/...` path へ変換する。
fn pointer_path(track: &serde_path_to_error::Path) -> String {
    use serde_path_to_error::Segment;
    let mut out = String::from("/params");
    for seg in track {
        match seg {
            Segment::Map { key } => {
                out.push('/');
                out.push_str(key);
            }
            Segment::Seq { index } => {
                out.push('/');
                out.push_str(&index.to_string());
            }
            Segment::Enum { variant } => {
                out.push('/');
                out.push_str(variant);
            }
            Segment::Unknown => out.push_str("/?"),
        }
    }
    out
}

/// params JSON を型 `T` として検査する（値は捨てる・エラーは path 付き）。
fn check_as<T: DeserializeOwned>(raw: &Value) -> Result<(), ParamsIssue> {
    let mut track = serde_path_to_error::Track::new();
    let de = serde_path_to_error::Deserializer::new(raw, &mut track);
    match T::deserialize(de) {
        Ok(_) => Ok(()),
        Err(e) => Err(ParamsIssue {
            path: pointer_path(&track.path()),
            message: e.to_string(),
        }),
    }
}

/// ノード種ごとの typed 契約へ params を照合する（保存時検証 V1 の入口）。
///
/// 予約語彙（`available_stage_a() == false`）は V3 が保存を拒否するため対象外（`Ok`）。
pub fn check_params(nt: NodeType, raw: &Value) -> Result<(), ParamsIssue> {
    match nt {
        NodeType::ControlBranch => check_as::<BranchParams>(raw),
        NodeType::ControlSwitch => check_as::<SwitchParams>(raw),
        NodeType::ControlJoin => check_as::<JoinParams>(raw),
        NodeType::ControlMap => check_as::<MapParams>(raw),
        NodeType::ControlWait => {
            check_as::<WaitParams>(raw)?;
            // kind と供給フィールドの整合（tag 付き enum は deny_unknown_fields 不可のため
            // フラット struct ＋ cross-field 検査で厳密性を保つ）。
            let parsed: WaitParams = parse(raw).map_err(|message| ParamsIssue {
                path: "/params".to_string(),
                message,
            })?;
            parsed.check_cross_fields()
        }
        NodeType::StorageRead => check_as::<StorageReadParams>(raw),
        NodeType::StorageWrite => check_as::<StorageWriteParams>(raw),
        NodeType::StorageList => check_as::<StorageListParams>(raw),
        NodeType::RagSearch => check_as::<RagSearchParams>(raw),
        NodeType::LlmInvoke => check_as::<LlmInvokeParams>(raw),
        NodeType::AgentInvoke => check_as::<AgentInvokeParams>(raw),
        NodeType::HttpRequest => {
            check_as::<HttpRequestParams>(raw)?;
            let parsed: HttpRequestParams = parse(raw).map_err(|message| ParamsIssue {
                path: "/params".to_string(),
                message,
            })?;
            parsed.check_cross_fields()
        }
        NodeType::ScriptRun => {
            check_as::<ScriptRunParams>(raw)?;
            let parsed: ScriptRunParams = parse(raw).map_err(|message| ParamsIssue {
                path: "/params".to_string(),
                message,
            })?;
            parsed.source.check_exactly_one()?;
            // Stage A は inline のみ（実行時 unsupported を保存時に前倒しで弾く・偽装しない）。
            if parsed.source.artifact.is_some() {
                return Err(ParamsIssue {
                    path: "/params/source/artifact".to_string(),
                    message: "script.run の artifact 参照は Stage A では未対応です".to_string(),
                });
            }
            Ok(())
        }
        NodeType::SkillInvoke => check_as::<SkillInvokeParams>(raw),
        NodeType::WorkflowStart => check_as::<WorkflowStartParams>(raw),
        NodeType::CsvQuery => check_as::<CsvQueryParams>(raw),
        NodeType::CsvPatch => check_as::<CsvPatchParams>(raw),
        NodeType::CsvWrite => check_as::<CsvWriteParams>(raw),
        // 予約語彙は V3 が「Stage A では未対応」で拒否する（params 契約は実装フェーズで追加）。
        _ => Ok(()),
    }
}

/// executor 向け: params を typed struct として取り出す（保存済み IR は V1 済みのため
/// 失敗は internal 扱い・呼び出し側が `bad_params` の permanent 失敗にする）。
pub fn parse<T: DeserializeOwned>(raw: &Value) -> Result<T, String> {
    serde_json::from_value(raw.clone()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unknown_field_reports_pointer_path() {
        let err =
            check_params(NodeType::StorageRead, &json!({ "file": "x", "bogus": 1 })).unwrap_err();
        // serde_path_to_error は未知フィールド名まで path に含める。
        assert_eq!(err.path, "/params/bogus");
        assert!(err.message.contains("bogus"), "{}", err.message);
    }

    #[test]
    fn nested_error_has_path() {
        let err = check_params(
            NodeType::HttpRequest,
            &json!({
                "url": "https://api.example.com",
                "secret": { "name": "tok", "attach": { "kind": "cookie" } }
            }),
        )
        .unwrap_err();
        assert_eq!(err.path, "/params/secret/attach/kind");
    }

    #[test]
    fn reserved_vocab_is_not_checked() {
        // 予約語彙は V3 が拒否するため params 契約対象外。
        assert!(check_params(NodeType::DataQuery, &json!({ "whatever": 1 })).is_ok());
    }

    #[test]
    fn roundtrip_all_stage_a_examples() {
        // 全 Stage A ノード種の代表 params が typed 契約を通る（serialize→deserialize 同値）。
        let cases: Vec<(NodeType, serde_json::Value)> = vec![
            (
                NodeType::ControlBranch,
                json!({ "condition": { "cmp": { "left": { "$from": "input", "path": "/n" }, "op": "gt", "right": 3 } } }),
            ),
            (
                NodeType::ControlSwitch,
                json!({ "value": { "$from": "input", "path": "/kind" },
                        "cases": [ { "port": "a", "equals": "A" }, { "port": "b", "equals": 2 } ] }),
            ),
            (NodeType::ControlJoin, json!({})),
            (
                NodeType::ControlMap,
                json!({ "items": { "$from": "input", "path": "/files" },
                        "max_concurrency": 4, "on_item_error": "collect" }),
            ),
            (
                NodeType::ControlWait,
                json!({ "kind": "duration", "duration_sec": 60 }),
            ),
            (
                NodeType::ControlWait,
                json!({ "kind": "event", "source": "storage.write",
                        "scope": { "folder": "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1" },
                        "timeout_sec": 3600, "on_timeout": "continue" }),
            ),
            (
                NodeType::StorageRead,
                json!({ "file": { "$from": "input", "path": "/file_id" } }),
            ),
            (
                NodeType::StorageWrite,
                json!({ "folder": "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1",
                        "name": { "$template": "report-{d}.md", "vars": { "d": { "$from": "input", "path": "/date" } } },
                        "content": { "$from": "nodes.gen.output" }, "content_type": "text/markdown" }),
            ),
            (
                NodeType::StorageList,
                json!({ "folder": "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1" }),
            ),
            (NodeType::StorageList, json!({})),
            (
                NodeType::RagSearch,
                json!({ "query": { "$from": "input", "path": "/q" }, "top_k": 5 }),
            ),
            (
                NodeType::LlmInvoke,
                json!({ "model": "stub-model", "prompt": { "$from": "input" },
                        "system": "あなたは要約器です", "max_tokens": 512 }),
            ),
            (
                NodeType::AgentInvoke,
                json!({ "instruction": { "$from": "input" },
                        "egress_allowlist": ["api.example.com"], "max_duration_sec": 300 }),
            ),
            (
                NodeType::HttpRequest,
                json!({ "method": "POST", "url": "https://api.example.com/v1",
                        "path_suffix": { "$from": "input", "path": "/suffix", "default": "" },
                        "body": { "$from": "nodes.gen.output" },
                        "secret": { "name": "tok", "attach": { "kind": "header", "header": "X-Api-Key" } },
                        "redirect": "deny" }),
            ),
            (
                NodeType::ScriptRun,
                json!({ "source": { "inline": "function main(input) { return input; }" },
                        "input": { "$from": "trigger" } }),
            ),
            (
                NodeType::WorkflowStart,
                json!({ "name": "child-flow", "input": { "a": 1 } }),
            ),
            (
                NodeType::SkillInvoke,
                json!({ "skill": "skill:expense@1", "input": { "$from": "input" } }),
            ),
        ];
        for (nt, raw) in cases {
            assert!(
                check_params(nt, &raw).is_ok(),
                "{}: {raw} が契約を通らない: {:?}",
                nt.as_str(),
                check_params(nt, &raw)
            );
        }
    }

    #[test]
    fn wait_cross_field_mismatch() {
        // kind=duration に event 用フィールドを混ぜると拒否。
        let err = check_params(
            NodeType::ControlWait,
            &json!({ "kind": "duration", "duration_sec": 60, "source": "storage.write" }),
        )
        .unwrap_err();
        assert_eq!(err.path, "/params/source");
        // kind=event は source 必須。
        let err = check_params(NodeType::ControlWait, &json!({ "kind": "event" })).unwrap_err();
        assert_eq!(err.path, "/params/source");
    }

    #[test]
    fn script_source_exactly_one() {
        let err = check_params(NodeType::ScriptRun, &json!({ "source": {} })).unwrap_err();
        assert_eq!(err.path, "/params/source");
        let err = check_params(
            NodeType::ScriptRun,
            &json!({ "source": { "inline": "1", "artifact": "script:x@1" } }),
        )
        .unwrap_err();
        assert_eq!(err.path, "/params/source");
    }

    #[test]
    fn http_url_must_be_literal_string() {
        // url は $from 不可（PIT-36 の型防御・String 型が構造的に拒否）。
        let err = check_params(
            NodeType::HttpRequest,
            &json!({ "url": { "$from": "input", "path": "/url" } }),
        )
        .unwrap_err();
        assert_eq!(err.path, "/params/url");
    }
}

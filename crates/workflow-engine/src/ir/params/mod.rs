//! ノード params の typed 契約（codegen が正・ir.md §7）。
//!
//! per-node struct を `deny_unknown_fields` で定義し、**保存時検証 V1 と executor が同一型を
//! 共有する**（契約ドリフトを構造的に排除・Task 10.6「ノード設定パネル契約」）。ts-rs で
//! TS 型へ export し、右パネル UI のフォームはこの型で型検査される（Task 10.12）。
//!
//! 方針:
//! - **実行時に強制されないフィールドは契約に含めない**（UI が効かない制限を約束しない・
//!   fail-closed）。例: `agent.invoke` の `allowed_tools`/`mount_scope`、`http.request` の
//!   `redirect: follow_stripped` は enforcement 実装時に追加する（`llm.invoke` の `output_schema` は
//!   構造化出力の検証と修復を入れたときに足した・GAP-13）。
//! - 予約語彙（Stage A 未実装ノード）の params 型は実装フェーズで追加する（ir.md §7.8）。
//! - 検証エラーは `/params/<field>/...` の JSON Pointer 風 path を持ち、dnd がフォーム
//!   フィールドへハイライトを写像できる（ir.md §8 のエラー形式）。

mod ai;
mod control;
mod data;
mod decide;
mod external;
mod human;
mod reply;
mod shape;
mod storage;
mod tabular;
mod transform;

pub use ai::{AgentInvokeParams, LlmEffort, LlmInvokeParams, SkillInvokeParams};
pub use control::{
    BranchParams, JoinMode, JoinParams, MapItemError, MapParams, SwitchCase, SwitchParams,
    WaitKind, WaitParams, WaitTimeout, MAX_MAP_ITEMS,
};
pub use data::{
    DataBulkRow, DataBulkUpsertParams, DataQueryParams, DataRecordCreateParams,
    DataRecordDeleteParams, DataRecordUpdateParams, DataRecordUpsertParams, DataTransitionParams,
    MAX_WORKFLOW_BULK_ROWS,
};
pub use decide::{
    AiDecideParams, DecideOption, DecideQuestion, DecideQuestionType, DecideRoute, DecideRouteBy,
    RoutedValue, DECIDE_INSTRUCTIONS_MISSING, DECIDE_THRESHOLD_MISSING, MAX_DECIDE_CONFIDENCE,
    MAX_DECIDE_DESCRIPTION_CHARS, MAX_DECIDE_INSTRUCTIONS_CHARS, MAX_DECIDE_KEY_CHARS,
    MAX_DECIDE_LEVELS, MAX_DECIDE_LEVEL_CHARS, MAX_DECIDE_OPTIONS, MAX_DECIDE_OPTION_LABEL_CHARS,
    MAX_DECIDE_QUESTIONS, MAX_DECIDE_STATE_BYTES, MIN_DECIDE_CONFIDENCE, MIN_DECIDE_LEVELS,
    MIN_DECIDE_OPTIONS, RESERVED_DECIDE_KEYS, UNCERTAIN_PORT,
};
pub use external::{
    HttpMethod, HttpRequestParams, HttpSecretRef, RedirectPolicy, ScriptRunParams,
    ScriptSourceSpec, SecretAttach, SecretAttachKind, WorkflowStartParams,
};
pub use human::{
    ApprovalAssignee, ApprovalChoice, ApprovalComment, ApprovalDecisionKind, ApprovalRecord,
    HumanApprovalParams, MAX_APPROVAL_ASSIGNEES, MAX_APPROVAL_INSTRUCTIONS_CHARS,
    MAX_APPROVAL_LABEL_CHARS, MAX_APPROVAL_SHOW_FIELDS, MAX_APPROVAL_TITLE_CHARS,
};
pub use reply::{
    ReplyColumn, ReplyFileSpec, ReplyFormat, ReplyTone, WorkflowReplyParams, MAX_REPLY_COLUMNS,
    MAX_REPLY_LABEL_CHARS,
};
pub use shape::{compact_ts, params_shape, params_type_name, ParamsShape, COMMON_TYPES};
pub use storage::{
    content_type_from_name, CsvEncoding, NameTakenPolicy, RagSearchParams, StorageListParams,
    StorageReadParams, StorageWriteParams,
};
pub use tabular::{CsvPatchParams, CsvQueryParams, CsvWriteParams};
pub use transform::{
    parse_tz, shift_amount, DateShift, DateUnit, TransformDateParams, WeekStart, DEFAULT_DATE_TZ,
    MAX_DATE_SHIFT,
};

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::ir::patterns::PatternBudget;
use crate::vocab::NodeType;

/// 必須の値の欄の項目が無いときの値（null）。
///
/// 置いたばかりのブロックを保存できるように、業務データ・ファイルを選ぶブロックの必須の値の欄は、項目が無くても契約
/// では誤りにしない（D-83・BUG-151）。空のまま（項目なし・空の文字・null・中身の無い入れ物）なら、保存時の警告に
/// とどめ、実行と自動実行のオンの前に止める（`validate::readiness` の `REQUIRED_VALUES`）ので、実行で使われることはない。
fn unset() -> crate::ir::expr::ValueExpr {
    crate::ir::expr::ValueExpr::Literal(Value::Null)
}

/// typed 契約違反（保存時検証 V1 が `ir.schema_violation` として全件収集する）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamsIssue {
    /// `/params/...` の JSON Pointer 風 path（フォームフィールドへの写像用）。
    pub path: String,
    /// 人向けメッセージ（serde の文言は日本語へ直す・型名や英語の文言を含めない）。
    pub message: String,
}

/// ノードの params の誤りの上限（1 つのブロックあたり・応答を膨らませない）。
const MAX_PARAMS_ISSUES: usize = 10;

/// params JSON を型 `T` として読み、誤りをできるだけまとめて `/params/...` の位置つきで返す（V1b・GAP-35）。
/// 型どおりに読めたら `cross`（項目どうしの検査・最初の 1 件）を掛ける。
fn issues_as<T: DeserializeOwned>(
    raw: &Value,
    cross: impl FnOnce(T) -> Result<(), ParamsIssue>,
) -> Vec<ParamsIssue> {
    let faults = crate::ir::schema_faults::faults::<T>(raw, MAX_PARAMS_ISSUES);
    if !faults.is_empty() {
        return faults
            .into_iter()
            .map(|f| ParamsIssue {
                path: format!("/params{}", crate::ir::schema_faults::pointer(&f.at)),
                message: humanize(&f.raw),
            })
            .collect();
    }
    match parse::<T>(raw) {
        Ok(parsed) => cross(parsed).err().into_iter().collect(),
        Err(message) => vec![ParamsIssue {
            path: "/params".to_string(),
            message,
        }],
    }
}

/// 項目どうしの検査の無い型。
fn issues_of<T: DeserializeOwned>(raw: &Value) -> Vec<ParamsIssue> {
    issues_as::<T>(raw, |_| Ok(()))
}

/// serde の英語の文言を利用者向けの日本語へ（項目名だけを拾い、型名・英語の文言を出さない・検証 N11）。
pub(crate) fn humanize(raw: &str) -> String {
    let msg = raw.split(" at line ").next().unwrap_or(raw);
    let quoted = |s: &str| -> Option<String> {
        let start = s.find('`')? + 1;
        let end = start + s[start..].find('`')?;
        Some(s[start..end].to_string())
    };
    if let Some(rest) = msg.strip_prefix("missing field ") {
        return quoted(rest).map_or_else(
            || "必要な項目がありません".into(),
            |f| format!("項目 '{f}' がありません"),
        );
    }
    if let Some(rest) = msg.strip_prefix("unknown field ") {
        return quoted(rest).map_or_else(
            || "使えない項目があります".into(),
            |f| format!("項目 '{f}' は使えません"),
        );
    }
    if msg.starts_with("unknown variant ") {
        return "選択肢にない値です".into();
    }
    if msg.starts_with("invalid type") {
        return "値の型が正しくありません".into();
    }
    if msg.starts_with("invalid length") {
        return "値の数が正しくありません".into();
    }
    if msg.starts_with("invalid value") {
        return "値が正しくありません".into();
    }
    // 自前の検証（日本語）の文言はそのまま返す。
    if msg.is_ascii() {
        "値が正しくありません".into()
    } else {
        msg.to_string()
    }
}

/// ノード種ごとの typed 契約へ params を照合する（最初の誤りだけ・[`params_issues`] の先頭）。
///
/// 予約語彙（`available_stage_a() == false`）は V3 が保存を拒否するため対象外（`Ok`）。
pub fn check_params(nt: NodeType, raw: &Value) -> Result<(), ParamsIssue> {
    params_issues(nt, raw)
        .into_iter()
        .next()
        .map_or(Ok(()), Err)
}

/// ノード種ごとの typed 契約へ params を照合し、誤りをできるだけまとめて返す（保存時検証 V1b の入口・GAP-35）。
///
/// 型の誤り（deny-unknown・必須・型）は項目ごとに位置つきで集める。型どおりに読めたときだけ項目どうしの検査
/// （kind と供給する項目の整合など）を掛け、その最初の 1 件を返す（型の誤りを直す前の組み合わせを言わない）。
/// 予約語彙（`available_stage_a() == false`）は V3 が保存を拒否するため対象外（空）。
///
/// 出力の形の `pattern` の数は、この params だけで数える。ワークフローの全体で数えるのは保存時検証
/// （[`crate::ir::validate::validate`]・BUG-226）。
pub fn params_issues(nt: NodeType, raw: &Value) -> Vec<ParamsIssue> {
    params_issues_with(nt, raw, &mut PatternBudget::for_save())
}

/// [`params_issues`] の、出力の形の `pattern` の数を `patterns` で数える形（ワークフローの全体の上限・BUG-226）。
pub(crate) fn params_issues_with(
    nt: NodeType,
    raw: &Value,
    patterns: &mut PatternBudget,
) -> Vec<ParamsIssue> {
    match nt {
        NodeType::ControlBranch => issues_of::<BranchParams>(raw),
        NodeType::ControlSwitch => issues_of::<SwitchParams>(raw),
        NodeType::ControlJoin => issues_of::<JoinParams>(raw),
        NodeType::ControlMap => issues_of::<MapParams>(raw),
        // kind と供給フィールドの整合（tag 付き enum は deny_unknown_fields 不可のため
        // フラット struct ＋ cross-field 検査で厳密性を保つ）。
        NodeType::ControlWait => issues_as::<WaitParams>(raw, |p| p.check_cross_fields()),
        NodeType::StorageRead => issues_of::<StorageReadParams>(raw),
        NodeType::StorageWrite => issues_of::<StorageWriteParams>(raw),
        NodeType::StorageList => issues_of::<StorageListParams>(raw),
        NodeType::RagSearch => issues_of::<RagSearchParams>(raw),
        NodeType::LlmInvoke => {
            issues_as::<LlmInvokeParams>(raw, |p| p.check_cross_fields_with(patterns))
        }
        NodeType::AgentInvoke => issues_of::<AgentInvokeParams>(raw),
        NodeType::AiDecide => issues_as::<AiDecideParams>(raw, |p| p.check_cross_fields()),
        NodeType::HttpRequest => issues_as::<HttpRequestParams>(raw, |p| p.check_cross_fields()),
        NodeType::ScriptRun => issues_as::<ScriptRunParams>(raw, |p| {
            p.source.check_exactly_one()?;
            // Stage A は inline のみ（実行時 unsupported を保存時に前倒しで弾く・偽装しない）。
            if p.source.artifact.is_some() {
                return Err(ParamsIssue {
                    path: "/params/source/artifact".to_string(),
                    message: "script.run の artifact 参照は Stage A では未対応です".to_string(),
                });
            }
            Ok(())
        }),
        NodeType::SkillInvoke => issues_of::<SkillInvokeParams>(raw),
        NodeType::WorkflowStart => issues_of::<WorkflowStartParams>(raw),
        NodeType::CsvQuery => issues_as::<CsvQueryParams>(raw, |p| p.check_cross_fields()),
        NodeType::CsvPatch => issues_of::<CsvPatchParams>(raw),
        NodeType::CsvWrite => issues_of::<CsvWriteParams>(raw),
        NodeType::DataQuery => issues_as::<DataQueryParams>(raw, |p| p.check_cross_fields()),
        NodeType::DataRecordCreate => issues_of::<DataRecordCreateParams>(raw),
        NodeType::DataRecordUpdate => {
            issues_as::<DataRecordUpdateParams>(raw, |p| p.check_cross_fields())
        }
        NodeType::DataRecordUpsert => {
            issues_as::<DataRecordUpsertParams>(raw, |p| p.check_cross_fields())
        }
        NodeType::DataRecordDelete => issues_of::<DataRecordDeleteParams>(raw),
        NodeType::DataTransition => issues_of::<DataTransitionParams>(raw),
        NodeType::DataBulkUpsert => issues_of::<DataBulkUpsertParams>(raw),
        NodeType::WorkflowReply => {
            issues_as::<WorkflowReplyParams>(raw, |p| p.check_cross_fields())
        }
        NodeType::HumanApproval => {
            issues_as::<HumanApprovalParams>(raw, |p| p.check_cross_fields())
        }
        NodeType::TransformDate => {
            issues_as::<TransformDateParams>(raw, |p| p.check_cross_fields())
        }
        // 予約語彙は V3 が「Stage A では未対応」で拒否する（params 契約は実装フェーズで追加）。
        _ => Vec::new(),
    }
}

/// executor 向け: params を typed struct として取り出す（保存済み IR は V1 済みのため
/// 失敗は internal 扱い・呼び出し側が `bad_params` の permanent 失敗にする）。
pub fn parse<T: DeserializeOwned>(raw: &Value) -> Result<T, String> {
    serde_json::from_value(raw.clone()).map_err(|e| humanize(&e.to_string()))
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
        assert!(!err.message.contains("unknown field"), "{}", err.message);
    }

    /// 型の誤りは 1 件で止めず、項目ごとに位置つきで返す（GAP-35）。最初の 1 件は前と同じ。
    #[test]
    fn params_issues_are_all_reported_with_positions() {
        let raw = json!({
            "url": "https://api.example.com", "method": "FETCH", "bogus": 1,
            "secret": { "name": "tok", "attach": { "kind": "cookie" } }
        });
        let issues = params_issues(NodeType::HttpRequest, &raw);
        let mut at: Vec<&str> = issues.iter().map(|i| i.path.as_str()).collect();
        at.sort_unstable();
        assert_eq!(
            at,
            vec![
                "/params/bogus",
                "/params/method",
                "/params/secret/attach/kind"
            ],
            "{issues:?}"
        );
        assert_eq!(
            check_params(NodeType::HttpRequest, &raw).unwrap_err(),
            issues[0]
        );
        // 型の誤りがある間は項目どうしの検査を掛けない（kind の誤りだけを言い、kind と項目の組み合わせは言わない）。
        let wait = json!({ "kind": "nap", "duration_sec": 60, "source": "storage.write" });
        let issues = params_issues(NodeType::ControlWait, &wait);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].path, "/params/kind");
        // 必須の項目が無いときは、入れ物の位置と項目の名前（前と同じ形）。
        let issues = params_issues(NodeType::StorageWrite, &json!({ "name": "a.txt", "x": 1 }));
        let mut got: Vec<(&str, &str)> = issues
            .iter()
            .map(|i| (i.path.as_str(), i.message.as_str()))
            .collect();
        got.sort_unstable();
        assert_eq!(
            got,
            vec![
                ("/params", "項目 'content' がありません"),
                ("/params/x", "項目 'x' は使えません")
            ]
        );
    }

    #[test]
    fn missing_field_message_is_japanese() {
        assert_eq!(humanize("missing field `data`"), "項目 'data' がありません");
        assert_eq!(
            humanize("invalid type: string \"x\", expected struct Foo at line 1 column 2"),
            "値の型が正しくありません"
        );
    }

    /// D-83: 置いたばかりの空の URL と判定用のモデルは保存を止めない（実行と自動実行のオンの前に止める）。
    /// 書いた URL の誤りは今までどおり止める。
    #[test]
    fn blank_url_and_decide_model_do_not_block_save() {
        assert!(check_params(NodeType::HttpRequest, &json!({ "url": "" })).is_ok());
        assert!(check_params(NodeType::HttpRequest, &json!({ "url": "  " })).is_ok());
        let err = check_params(NodeType::HttpRequest, &json!({ "url": "https://" })).unwrap_err();
        assert_eq!(err.path, "/params/url");
        assert!(check_params(NodeType::HttpRequest, &json!({ "url": "ftp://a.example" })).is_err());
        let decide = json!({
            "model": "", "state": "",
            "questions": [{ "key": "q1", "type": "boolean", "instructions": "請求か" }]
        });
        assert!(check_params(NodeType::AiDecide, &decide).is_ok());
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
        assert!(check_params(NodeType::NotifySend, &json!({ "whatever": 1 })).is_ok());
    }

    #[test]
    fn data_query_uses_the_data_query_type_itself() {
        // 条件木は data の DataQuery（= workflow IR と同じ語彙の {"cmp":{field,op,value}}）。
        let ok = check_params(
            NodeType::DataQuery,
            &json!({ "table": "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1",
                     "query": { "filter": { "all": [
                         { "cmp": { "field": "status", "op": "eq", "value": "pending" } },
                         { "cmp": { "field": "amount", "op": "gte", "value": { "$from": "input", "path": "/min" } } }
                     ] }, "sort": [ { "field": "amount", "descending": true } ], "limit": 20 } }),
        );
        assert!(ok.is_ok(), "{ok:?}");
        // DataQuery の未知キー（OFFSET 以外の独自表現）は data の型が拒否する。
        let err = check_params(
            NodeType::DataQuery,
            &json!({ "table": "t", "query": { "filter": { "eq": { "status": "x" } } } }),
        )
        .unwrap_err();
        assert!(err.path.starts_with("/params/query"), "{err:?}");
    }

    #[test]
    #[allow(clippy::too_many_lines)] // ノード種ごとの例の表。
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
                NodeType::LlmInvoke,
                json!({ "prompt": "分類して", "effort": "low", "max_tokens": 2048,
                        "output_schema": { "type": "object", "required": ["category"],
                            "properties": { "category": { "type": "string", "enum": ["a", "b"] } } } }),
            ),
            (
                NodeType::AgentInvoke,
                json!({ "instruction": { "$from": "input" },
                        "egress_allowlist": ["api.example.com"], "max_duration_sec": 300 }),
            ),
            (
                NodeType::AiDecide,
                json!({ "model": "decide-haiku", "state": { "$from": "nodes.create.output", "path": "/data" },
                        "questions": [ { "key": "category", "type": "choice", "instructions": "種類はどれか",
                                         "options": [ { "key": "billing", "description": "請求" }, { "key": "bug" } ] } ],
                        "route": { "question": "category", "by": "confidence", "min_confidence": 0.8 } }),
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
            (
                NodeType::WorkflowReply,
                json!({ "format": "markdown", "label": "回答", "tone": "success",
                        "value": { "$from": "nodes.gen.output", "path": "/text" } }),
            ),
            (
                NodeType::DataQuery,
                json!({ "table": { "$from": "input", "path": "/table" },
                        "query": { "filter": { "cmp": { "field": "status", "op": "eq", "value": "pending" } } } }),
            ),
            (
                NodeType::DataRecordCreate,
                json!({ "table": "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1",
                        "data": { "title": { "$from": "input", "path": "/title" } } }),
            ),
            (
                NodeType::DataRecordUpdate,
                json!({ "table": "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1",
                        "record": { "$from": "nodes.q.output", "path": "/items/0/id" },
                        "patch": { "note": "checked" },
                        "expected_rev": { "$from": "nodes.q.output", "path": "/items/0/rev" } }),
            ),
            (
                NodeType::DataRecordUpdate,
                json!({ "table": "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1",
                        "record": { "$from": "nodes.q.output", "path": "/items/0/id" },
                        "add": { "stock": { "$from": "input", "path": "/delta" } } }),
            ),
            (
                NodeType::DataRecordUpsert,
                json!({ "table": "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1", "match_on": "month",
                        "data": { "month": { "$from": "input", "path": "/month" } },
                        "add": { "total": { "$from": "input", "path": "/amount" } } }),
            ),
            (
                NodeType::DataRecordDelete,
                json!({ "table": "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1", "record": { "$from": "input", "path": "/id" } }),
            ),
            (
                NodeType::DataTransition,
                json!({ "table": "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1",
                        "record": { "$from": "trigger", "path": "/record_id" }, "to": "approved" }),
            ),
            (
                NodeType::DataBulkUpsert,
                json!({ "table": "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1",
                        "rows": { "$from": "nodes.gen.output", "path": "/rows" } }),
            ),
            (
                NodeType::DataBulkUpsert,
                json!({ "table": "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1", "match_on": "response_id",
                        "rows": { "$from": "nodes.gen.output", "path": "/rows" } }),
            ),
            (NodeType::TransformDate, json!({})),
            (
                NodeType::TransformDate,
                json!({ "base": { "$from": "run", "path": "/scheduled_at" },
                        "shift": { "amount": -1, "unit": "week" }, "period": "week",
                        "week_starts_on": "sunday", "tz": "America/New_York" }),
            ),
            (
                NodeType::TransformDate,
                json!({ "base": "2026-01-31", "shift": { "amount": { "$from": "input", "path": "/n" }, "unit": "month" },
                        "period": "month" }),
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

    /// 出力の形は保存時に検査し、誤りは `/params/output_schema/...` の位置で返す（GAP-13）。
    #[test]
    fn llm_output_schema_is_checked_with_its_position() {
        let err = check_params(
            NodeType::LlmInvoke,
            &json!({ "prompt": "p", "output_schema": { "type": "object", "properties": {
                "a": { "type": ["string", "integer"] } } } }),
        )
        .unwrap_err();
        assert_eq!(err.path, "/params/output_schema/properties/a/type");
        let err = check_params(
            NodeType::LlmInvoke,
            &json!({ "prompt": "p", "output_schema": { "type": "array" } }),
        )
        .unwrap_err();
        assert_eq!(err.path, "/params/output_schema/type");
        // 思考の深さは 3 段階だけ。出力の形はリテラルの JSON Schema（値式ではない）。
        let err = check_params(
            NodeType::LlmInvoke,
            &json!({ "prompt": "p", "effort": "max" }),
        )
        .unwrap_err();
        assert_eq!(err.path, "/params/effort");
        let err = check_params(
            NodeType::LlmInvoke,
            &json!({ "prompt": "p", "output_schema": { "$from": "input" } }),
        )
        .unwrap_err();
        assert_eq!(err.path, "/params/output_schema/$from");
        assert!(err.message.contains("$from"), "{}", err.message);
    }

    /// 日付を計算: 時間帯の名前・リテラルのずらす数・リテラルの基準の日時は保存時に照らす（GAP-15）。
    #[test]
    fn transform_date_literals_are_checked() {
        let path = |raw: serde_json::Value| {
            check_params(NodeType::TransformDate, &raw)
                .unwrap_err()
                .path
        };
        assert_eq!(path(json!({ "tz": "Japan/Tokyo" })), "/params/tz");
        assert_eq!(path(json!({ "tz": "" })), "/params/tz");
        assert_eq!(
            path(json!({ "shift": { "amount": 1.5, "unit": "day" } })),
            "/params/shift/amount"
        );
        assert_eq!(
            path(json!({ "shift": { "amount": "-1", "unit": "day" } })),
            "/params/shift/amount"
        );
        assert_eq!(
            path(json!({ "shift": { "amount": 10_001, "unit": "day" } })),
            "/params/shift/amount"
        );
        assert_eq!(
            path(json!({ "shift": { "amount": 1, "unit": "hour" } })),
            "/params/shift/unit"
        );
        assert_eq!(path(json!({ "period": "quarter" })), "/params/period");
        assert_eq!(
            path(json!({ "week_starts_on": "saturday" })),
            "/params/week_starts_on"
        );
        // 時差の無い日時は、どの時間帯の時刻か分からないので書けない。
        assert_eq!(
            path(json!({ "base": "2026-11-02T09:00:00" })),
            "/params/base"
        );
        assert_eq!(path(json!({ "base": 20_261_102 })), "/params/base");
        assert!(check_params(
            NodeType::TransformDate,
            &json!({ "base": "2026-11-02T09:00:00Z" })
        )
        .is_ok());
        assert!(check_params(
            NodeType::TransformDate,
            &json!({ "shift": { "amount": -10_000, "unit": "day" } })
        )
        .is_ok());
        // 項目の書き間違いは契約が拒む。
        assert_eq!(
            path(json!({ "shift": { "amount": 1, "unit": "day", "n": 2 } })),
            "/params/shift/n"
        );
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

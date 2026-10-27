//! ノード params の形（キー・省略可否・型）を typed 契約の ts-rs 宣言から組む（AI の説明文・
//! 検証エラーの補足・TS の `NodeParamsByType` の単一の対応）。
//!
//! 手書きの対応表を作らない: V1 と executor が共有する params 型（[`super`]）の `TS::inline()` を
//! そのまま使い、doc コメントを落として 1 行にする。参照する型（`SwitchCase` など）は依存を辿って
//! 集める。値式・条件木の型（[`COMMON_TYPES`]）は使う側が 1 度だけ説明するので展開しない。
//!
//! 型の表記は TS: `key?:` は省略可。ただし ts-rs は serde の `default` を知らないので、既定値のある
//! 非 Option の項目（`control.map` の `on_item_error` など）は必須に見える（書いても誤りにはならない）。

use ts_rs::{TypeVisitor, TS};

use super::{
    AgentInvokeParams, AiDecideParams, BranchParams, CsvPatchParams, CsvQueryParams,
    CsvWriteParams, DataBulkUpsertParams, DataQueryParams, DataRecordCreateParams,
    DataRecordDeleteParams, DataRecordUpdateParams, DataTransitionParams, HttpRequestParams,
    HumanApprovalParams, JoinParams, LlmInvokeParams, MapParams, RagSearchParams, ScriptRunParams,
    SkillInvokeParams, StorageListParams, StorageReadParams, StorageWriteParams, SwitchParams,
    WaitParams, WorkflowReplyParams, WorkflowStartParams,
};
use crate::vocab::NodeType;

/// 値式・条件木の型（全ノードで共通・説明文の冒頭で説明するのでノードごとには展開しない）。
pub const COMMON_TYPES: &[&str] = &[
    "ValueExpr",
    "FromRef",
    "TemplateExpr",
    "Condition",
    "Comparison",
    "CmpOp",
    "EventSource",
];

/// 1 ノード種の params の形。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamsShape {
    /// TS の型名（例: `LlmInvokeParams`）。
    pub type_name: String,
    /// 本体（例: `{ model?: string, prompt: ValueExpr }`）。
    pub body: String,
    /// 本体が参照する型（[`COMMON_TYPES`] 以外）の `(名前, 本体)`。依存を辿って集め、名前順に並べる。
    pub refs: Vec<(String, String)>,
}

impl ParamsShape {
    /// 本体と参照する型を `本体（Name = …、…）` の 1 行にする。
    #[must_use]
    pub fn render(&self) -> String {
        if self.refs.is_empty() {
            return self.body.clone();
        }
        let refs = self
            .refs
            .iter()
            .map(|(name, body)| format!("{name} = {body}"))
            .collect::<Vec<_>>()
            .join("、");
        format!("{}（{refs}）", self.body)
    }
}

/// ノード種の params の形（予約語彙は契約が無いので `None`）。
///
/// 対応は [`super::check_params`] と同じ型（網羅 match・予約語彙を足すと実装時にここも埋まる）。
#[must_use]
pub fn params_shape(nt: NodeType) -> Option<ParamsShape> {
    struct Shape;
    impl ParamsTypeVisitor for Shape {
        type Out = ParamsShape;
        fn visit<T: TS + 'static>(self) -> ParamsShape {
            shape_of::<T>()
        }
    }
    visit_params_type(nt, Shape)
}

/// ノード種の params 型の TS 名（`bin/export-ts.rs` の `NodeParamsByType` が使う）。
#[must_use]
pub fn params_type_name(nt: NodeType) -> Option<String> {
    struct Name;
    impl ParamsTypeVisitor for Name {
        type Out = String;
        fn visit<T: TS + 'static>(self) -> String {
            T::name()
        }
    }
    visit_params_type(nt, Name)
}

/// TS 型 1 つを説明用の 1 行にする（doc コメントを落とし、空白を詰める）。
#[must_use]
pub fn compact_ts<T: TS + ?Sized>() -> String {
    compact(&T::inline())
}

/// params 型ごとの処理（ノード種 → 型の対応を 1 か所に閉じる）。
trait ParamsTypeVisitor {
    type Out;
    fn visit<T: TS + 'static>(self) -> Self::Out;
}

fn visit_params_type<V: ParamsTypeVisitor>(nt: NodeType, v: V) -> Option<V::Out> {
    use NodeType as N;
    Some(match nt {
        N::ControlBranch => v.visit::<BranchParams>(),
        N::ControlSwitch => v.visit::<SwitchParams>(),
        N::ControlJoin => v.visit::<JoinParams>(),
        N::ControlMap => v.visit::<MapParams>(),
        N::ControlWait => v.visit::<WaitParams>(),
        N::StorageRead => v.visit::<StorageReadParams>(),
        N::StorageWrite => v.visit::<StorageWriteParams>(),
        N::StorageList => v.visit::<StorageListParams>(),
        N::RagSearch => v.visit::<RagSearchParams>(),
        N::LlmInvoke => v.visit::<LlmInvokeParams>(),
        N::AgentInvoke => v.visit::<AgentInvokeParams>(),
        N::AiDecide => v.visit::<AiDecideParams>(),
        N::HttpRequest => v.visit::<HttpRequestParams>(),
        N::ScriptRun => v.visit::<ScriptRunParams>(),
        N::WorkflowStart => v.visit::<WorkflowStartParams>(),
        N::WorkflowReply => v.visit::<WorkflowReplyParams>(),
        N::SkillInvoke => v.visit::<SkillInvokeParams>(),
        N::CsvQuery => v.visit::<CsvQueryParams>(),
        N::CsvPatch => v.visit::<CsvPatchParams>(),
        N::CsvWrite => v.visit::<CsvWriteParams>(),
        N::DataQuery => v.visit::<DataQueryParams>(),
        N::DataRecordCreate => v.visit::<DataRecordCreateParams>(),
        N::DataRecordUpdate => v.visit::<DataRecordUpdateParams>(),
        N::DataRecordDelete => v.visit::<DataRecordDeleteParams>(),
        N::DataTransition => v.visit::<DataTransitionParams>(),
        N::DataBulkUpsert => v.visit::<DataBulkUpsertParams>(),
        N::HumanApproval => v.visit::<HumanApprovalParams>(),
        // 予約語彙（params 契約は実装フェーズで追加する・ir.md §7.8）。
        N::NotifySend
        | N::ControlLoop
        | N::ControlAssert
        | N::DebugLog
        | N::TransformTemplate
        | N::TransformParse
        | N::TransformSerialize
        | N::TransformRegex
        | N::TransformMap
        | N::TransformFilter
        | N::TransformReduce
        | N::SheetRead
        | N::SheetWrite
        | N::SheetAppend
        | N::DocRead
        | N::DocEdit
        | N::DocComment
        | N::MemoryGet
        | N::MemorySet
        | N::EventPublish
        | N::WorkflowCall
        | N::LlmEmbed
        | N::LlmExtract
        | N::AiReview
        | N::AiEval
        | N::AiOcr
        | N::AiImageAnalyze
        | N::AiImageGenerate
        | N::AiTranscribe
        | N::AiSpeech
        | N::GraphqlQuery
        | N::SandboxExec => return None,
    })
}

fn shape_of<T: TS + 'static>() -> ParamsShape {
    let mut refs = Refs(Vec::new());
    T::visit_dependencies(&mut refs);
    let mut refs = refs.0;
    // data の `DataQuery` は OpenAPI の型を指す（`#[ts(type = …)]`）ため、TS の依存として辿れない。
    // 同じ型の OpenAPI スキーマ（utoipa）から最上位の項目を引く。
    let body = compact(&T::inline());
    if body.contains("DataQuery") && !refs.iter().any(|(n, _)| n == "DataQuery") {
        refs.push(("DataQuery".to_string(), data_query_body()));
    }
    refs.sort_by(|a, b| a.0.cmp(&b.0));
    ParamsShape {
        type_name: T::name(),
        body,
        refs,
    }
}

/// 依存を辿って宣言を持つ型（`#[ts(export)]`）を集める visitor。
struct Refs(Vec<(String, String)>);

impl TypeVisitor for Refs {
    fn visit<U: TS + 'static + ?Sized>(&mut self) {
        // 宣言を持たない型（プリミティブ・Option / Vec などの包み）は型引数の側が別に訪問される。
        if U::output_path().is_none() {
            return;
        }
        let name = U::ident();
        if COMMON_TYPES.contains(&name.as_str()) || self.0.iter().any(|(n, _)| *n == name) {
            return;
        }
        self.0.push((name, compact(&U::inline())));
        U::visit_dependencies(self);
    }
}

/// `data::DataQuery` の最上位の項目（OpenAPI スキーマから・省略可は `?`）。
fn data_query_body() -> String {
    use utoipa::PartialSchema;
    let schema = serde_json::to_value(<data::DataQuery as PartialSchema>::schema())
        .unwrap_or(serde_json::Value::Null);
    let required: Vec<&str> = schema
        .get("required")
        .and_then(serde_json::Value::as_array)
        .map(|r| r.iter().filter_map(serde_json::Value::as_str).collect())
        .unwrap_or_default();
    let Some(props) = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
    else {
        return "unknown".to_string();
    };
    let fields: Vec<String> = props
        .iter()
        // 廃止した項目（v1 の offset）は案内しない。
        .filter(|(_, s)| s.get("deprecated").and_then(serde_json::Value::as_bool) != Some(true))
        .map(|(k, s)| {
            let opt = if required.contains(&k.as_str()) {
                ""
            } else {
                "?"
            };
            format!("{k}{opt}: {}", json_schema_type(s))
        })
        .collect();
    format!("{{ {} }}", fields.join(", "))
}

/// OpenAPI スキーマの型を TS 風の短い表記にする（参照は型名だけ）。
fn json_schema_type(s: &serde_json::Value) -> String {
    use serde_json::Value;
    if let Some(r) = s.get("$ref").and_then(Value::as_str) {
        return r.rsplit('/').next().unwrap_or(r).to_string();
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(alts) = s.get(key).and_then(Value::as_array) {
            let parts: Vec<String> = alts
                .iter()
                .map(json_schema_type)
                .filter(|t| t != "null")
                .collect();
            return parts.join(" | ");
        }
    }
    let ty = match s.get("type") {
        Some(Value::String(t)) => t.clone(),
        Some(Value::Array(ts)) => ts
            .iter()
            .filter_map(Value::as_str)
            .find(|t| *t != "null")
            .unwrap_or("unknown")
            .to_string(),
        _ => return "unknown".to_string(),
    };
    match ty.as_str() {
        "array" => format!(
            "Array<{}>",
            s.get("items")
                .map_or_else(|| "unknown".into(), json_schema_type)
        ),
        "integer" => "number".to_string(),
        _ => ty,
    }
}

/// ts-rs の出力を 1 行にする（`/** … */` を落とし、空白を詰め、末尾の `, }` を ` }` に）。
fn compact(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find("/**") {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find("*/") else {
            rest = "";
            break;
        };
        rest = &rest[start + end + 2..];
    }
    out.push_str(rest);
    // OpenAPI の型を指す `import("./api").components["schemas"]["X"]` は型名だけにする。
    const OPENAPI_REF: &str = "import(\"./api\").components[\"schemas\"][\"";
    while let Some(start) = out.find(OPENAPI_REF) {
        let name_start = start + OPENAPI_REF.len();
        let Some(len) = out[name_start..].find("\"]") else {
            break;
        };
        let name = out[name_start..name_start + len].to_string();
        out.replace_range(start..name_start + len + 2, &name);
    }
    let collapsed = out.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.replace(", }", " }").replace("{ }", "{}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_stage_a_node_has_a_params_shape_and_reserved_ones_do_not() {
        for nt in NodeType::ALL {
            assert_eq!(
                params_shape(*nt).is_some(),
                nt.available_stage_a(),
                "{} の params の形",
                nt.as_str()
            );
        }
    }

    #[test]
    fn shape_lists_keys_with_optional_marks_and_no_doc_comments() {
        let s = params_shape(NodeType::LlmInvoke).unwrap();
        assert_eq!(s.type_name, "LlmInvokeParams");
        assert_eq!(
            s.body,
            "{ model?: string, prompt: ValueExpr, system?: ValueExpr, max_tokens?: ValueExpr, \
             effort?: LlmEffort, output_schema?: Record<string, unknown> }"
        );
        // 参照する型（思考の深さの 3 段階）も同じ宣言から載る（GAP-13）。
        assert_eq!(
            s.refs,
            vec![(
                "LlmEffort".to_string(),
                "\"low\" | \"medium\" | \"high\"".to_string()
            )]
        );
        for nt in NodeType::ALL {
            if let Some(s) = params_shape(*nt) {
                let all = s.render();
                assert!(!all.contains("/**") && !all.contains('\n'), "{all}");
            }
        }
    }

    #[test]
    fn nested_types_are_collected_but_common_value_types_are_not() {
        let s = params_shape(NodeType::HttpRequest).unwrap();
        let names: Vec<&str> = s.refs.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "HttpMethod",
                "HttpSecretRef",
                "RedirectPolicy",
                "SecretAttach",
                "SecretAttachKind"
            ]
        );
        let s = params_shape(NodeType::ControlBranch).unwrap();
        assert_eq!(s.body, "{ condition: Condition }");
        assert!(
            s.refs.is_empty(),
            "条件木は共通の型として展開しない: {:?}",
            s.refs
        );
        let s = params_shape(NodeType::ControlSwitch).unwrap();
        assert!(s
            .render()
            .contains("SwitchCase = { port: string, equals: unknown }"));
    }

    #[test]
    fn data_query_is_described_from_the_openapi_schema() {
        let s = params_shape(NodeType::DataQuery).unwrap();
        assert_eq!(s.body, "{ table: ValueExpr, query?: DataQuery }");
        let (_, q) = s.refs.iter().find(|(n, _)| n == "DataQuery").unwrap();
        for key in [
            "filter?:",
            "sort?: Array<SortKey>",
            "limit?: number",
            "select?:",
        ] {
            assert!(q.contains(key), "{key} が無い: {q}");
        }
        assert!(!q.contains("offset"), "廃止した項目は案内しない: {q}");
    }

    #[test]
    fn type_names_match_the_params_types() {
        assert_eq!(
            params_type_name(NodeType::DataBulkUpsert).as_deref(),
            Some("DataBulkUpsertParams")
        );
        assert_eq!(params_type_name(NodeType::NotifySend), None);
    }
}

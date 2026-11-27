//! ワークフロー語彙・IR 型・ノードカタログを TypeScript へ書き出す（codegen が正・scripts/gen-api.sh から呼ぶ）。
//!
//! - `workflow-ir.ts` — IR/params/検証エラーの型（ts-rs decl の連結）。
//! - `workflow-catalog.ts` — `NODE_CATALOG` 定数（日本語ラベル・ポート・スコープ・可用性）・`EVENT_CATALOG`
//!   定数（イベントのペイロードの項目・D-47）と `WARNING_CODES` 定数（保存を止めない警告のコード・BUG-48）と
//!   `NodeParamsByType` マップ型（右パネル UI の per-node フォーム契約・Task 10.6/10.12）。

#![allow(clippy::expect_used, clippy::print_stdout)]

use std::path::PathBuf;

use ts_rs::TS;
use workflow_engine::ir::edge::Edge;
use workflow_engine::ir::expr::{CmpOp, Comparison, Condition, FromRef, TemplateExpr, ValueExpr};
use workflow_engine::ir::node::{Backoff, Node, OnError, RetryPolicy, StepCache, StepCacheLimits};
use workflow_engine::ir::params::{
    params_type_name, AgentInvokeParams, AiDecideParams, ApprovalAssignee, ApprovalChoice,
    ApprovalComment, ApprovalRecord, BranchParams, CsvEncoding, CsvPatchParams, CsvQueryParams,
    CsvWriteParams, DataBulkUpsertParams, DataQueryParams, DataRecordCreateParams,
    DataRecordDeleteParams, DataRecordUpdateParams, DataRecordUpsertParams, DataTransitionParams,
    DecideOption, DecideQuestion, DecideQuestionType, DecideRoute, DecideRouteBy, HttpMethod,
    HttpRequestParams, HttpSecretRef, HumanApprovalParams, JoinMode, JoinParams, LlmEffort,
    LlmInvokeParams, MapItemError, MapParams, NameTakenPolicy, RagSearchParams, RedirectPolicy,
    ReplyColumn, ReplyFileSpec, ReplyFormat, ReplyTone, ScriptRunParams, ScriptSourceSpec,
    SecretAttach, SecretAttachKind, SkillInvokeParams, StorageListParams, StorageReadParams,
    StorageWriteParams, SwitchCase, SwitchParams, WaitKind, WaitParams, WaitTimeout,
    WorkflowReplyParams, WorkflowStartParams,
};
use workflow_engine::ir::params::{
    DateShift, DateUnit, TransformDateParams, WeekStart, DEFAULT_DATE_TZ, MAX_DATE_SHIFT,
};
use workflow_engine::ir::trigger::{
    Catchup, EventTrigger, InteractiveTrigger, ScheduleTrigger, Trigger, TriggerKind,
};
use workflow_engine::ir::validate::{
    blank_ok_when_set, required_pick_fields, ValidationError, SAVE_WARNING_CODES,
};
use workflow_engine::ir::{Policies, TriggerOverflow, WorkflowIr};
use workflow_engine::vocab::{
    agent_tool_catalog, event_catalog, node_catalog, AgentTool, AgentToolCatalogEntry,
    EventCatalogEntry, EventPayloadField, EventSource, IdempotencyClass, NodeCatalogEntry,
    NodeCategory, NodeType, OutputField, PortRule, RunEventKind, RunInfoField, Scope,
    RUN_INFO_FIELDS, RUN_INFO_TIMEZONE, STORAGE_LIST_MAX_ITEMS, STORAGE_READ_MAX_DOCUMENT_CHARS,
};
use workflow_engine::INDEX_WAIT_MAX_MINUTES;

/// 保存を止めない警告のコードの定数（定数の名前 → コード・Rust の表から引く・BUG-48）。
fn warning_codes_ts() -> String {
    let rows: Vec<String> = SAVE_WARNING_CODES
        .iter()
        .map(|(name, code)| {
            format!(
                "  {name}: {},",
                serde_json::to_string(code).expect("警告のコードの直列化")
            )
        })
        .collect();
    format!(
        "\n/// 保存を止めない警告のコード（保存・検証の応答の `warnings[].code`・Rust の SAVE_WARNING_CODES から生成）。\n\
         export const WARNING_CODES = {{\n{}\n}} as const;\n\n\
         /// 保存を止めない警告のコードの型。\n\
         export type WarningCode = (typeof WARNING_CODES)[keyof typeof WARNING_CODES];\n",
        rows.join("\n")
    )
}

/// 必須の欄のうち選ぶ欄（ノード種 → params の項目・Rust の表から引く）。エディタは空の欄の下の促しを
/// 「選んでください」と「入力してください」に分ける。
fn required_picks_ts() -> String {
    let mut by_type: std::collections::BTreeMap<&str, Vec<&str>> =
        std::collections::BTreeMap::new();
    for (node_type, key) in required_pick_fields() {
        by_type.entry(node_type).or_default().push(key);
    }
    let rows: Vec<String> = by_type
        .iter()
        .map(|(t, keys)| {
            format!(
                "  {}: {},",
                serde_json::to_string(t).expect("ノード種の直列化"),
                serde_json::to_string(keys).expect("項目の直列化")
            )
        })
        .collect();
    format!(
        "\n/// 必須の欄のうち、選ぶ欄（ノード種 → params の項目・Rust の readiness の表から生成）。\n\
         export const REQUIRED_PICK_FIELDS: Readonly<Record<string, readonly string[]>> = {{\n{}\n}};\n",
        rows.join("\n")
    )
}

/// 必須の欄のうち、ほかの欄に値があれば空のままでよいもの（ノード種 → 空でよい欄 → 値があればよい欄・Rust の
/// readiness の表から引く）。エディタは、この欄が空でも「入力してください」を出さない。
fn blank_ok_ts() -> String {
    let mut by_type: std::collections::BTreeMap<&str, Vec<String>> =
        std::collections::BTreeMap::new();
    for (node_type, key, other) in blank_ok_when_set() {
        by_type.entry(node_type).or_default().push(format!(
            "{}: {}",
            serde_json::to_string(key).expect("項目の直列化"),
            serde_json::to_string(other).expect("項目の直列化")
        ));
    }
    let rows: Vec<String> = by_type
        .iter()
        .map(|(t, pairs)| {
            format!(
                "  {}: {{ {} }},",
                serde_json::to_string(t).expect("ノード種の直列化"),
                pairs.join(", ")
            )
        })
        .collect();
    format!(
        "\n/// 必須の欄のうち、ほかの欄に値があれば空のままでよいもの（ノード種 → 空でよい欄 → 値があればよい欄・Rust の\n\
         /// readiness の BLANK_OK_WHEN_SET から生成）。\n\
         export const BLANK_OK_WHEN_SET: Readonly<Record<string, Readonly<Record<string, string>>>> = {{\n{}\n}};\n",
        rows.join("\n")
    )
}

/// 画面の注記で言う上限の数（Rust の定数から引く・注記に数を手で書かない）。
fn limits_ts() -> String {
    format!(
        "\n/// 「ファイルを読む」が PDF・Office の本文を読む字数の上限（超えた分は入らず truncated が true・D-111）。\n\
         export const STORAGE_READ_MAX_DOCUMENT_CHARS = {STORAGE_READ_MAX_DOCUMENT_CHARS};\n\
         \n/// 「ファイルを読む」が PDF・Office の本文の取り込みを待つ上限（分・過ぎたら index_pending で失敗・D-125）。\n\
         export const STORAGE_READ_INDEX_WAIT_MAX_MINUTES = {INDEX_WAIT_MAX_MINUTES};\n\
         \n/// 「フォルダ一覧」が一度に返す件数の上限（多いと先頭のこの件数だけで truncated が true・BUG-180）。\n\
         export const STORAGE_LIST_MAX_ITEMS = {STORAGE_LIST_MAX_ITEMS};\n\
         \n/// 「日付を計算」のずらす数の上限（絶対値・GAP-15）。\n\
         export const DATE_MAX_SHIFT = {MAX_DATE_SHIFT};\n\
         \n/// 「日付を計算」の時間帯の既定（日本時間・GAP-15）。\n\
         export const DATE_DEFAULT_TZ = {tz};\n",
        tz = serde_json::to_string(DEFAULT_DATE_TZ).expect("時間帯の既定の直列化"),
    ) + http_headers_ts().as_str()
}

/// 「外部 API 呼び出し」の「ヘッダー」の上限（数と値の長さ・名前と値の合計・`nodes::http_headers`・注記に数を手で
/// 書かない・GAP-19）。
fn http_headers_ts() -> String {
    use workflow_engine::nodes::http_headers::{MAX_HEADERS, MAX_TOTAL_BYTES, MAX_VALUE_BYTES};
    format!(
        "\n/// 「外部 API 呼び出し」の「ヘッダー」に書ける数の上限（script の Shiki.http.request と同じ・GAP-19）。\n\
         export const HTTP_MAX_HEADERS = {MAX_HEADERS};\n\
         \n/// ヘッダーの値の長さの上限（バイト）。\n\
         export const HTTP_MAX_HEADER_VALUE_BYTES = {MAX_VALUE_BYTES};\n\
         \n/// ヘッダーの名前と値を合わせた長さの上限（バイト）。\n\
         export const HTTP_MAX_HEADER_TOTAL_BYTES = {MAX_TOTAL_BYTES};\n"
    )
}

/// IR・params・カタログの型の TS 宣言（`workflow-ir.ts` に連結する順）。
fn ir_decls() -> Vec<String> {
    let mut decls: Vec<String> = Vec::new();
    macro_rules! export {
        ($($t:ty),+ $(,)?) => {
            $( decls.push(format!("export {}", <$t as TS>::decl())); )+
        };
    }
    export!(
        Scope,
        NodeType,
        EventSource,
        RunEventKind,
        WorkflowIr,
        Policies,
        TriggerOverflow,
        Node,
        RetryPolicy,
        Backoff,
        OnError,
        StepCache,
        StepCacheLimits,
        Edge,
        Trigger,
        TriggerKind,
        ScheduleTrigger,
        Catchup,
        EventTrigger,
        InteractiveTrigger,
        FromRef,
        TemplateExpr,
        ValueExpr,
        Condition,
        CmpOp,
        Comparison,
        ValidationError,
        // ---- ノード params（typed 契約・Task 10.6）----
        BranchParams,
        SwitchCase,
        SwitchParams,
        JoinMode,
        JoinParams,
        MapItemError,
        MapParams,
        WaitKind,
        WaitTimeout,
        WaitParams,
        StorageReadParams,
        NameTakenPolicy,
        CsvEncoding,
        StorageWriteParams,
        StorageListParams,
        RagSearchParams,
        LlmEffort,
        LlmInvokeParams,
        AgentTool,
        AgentInvokeParams,
        DecideQuestionType,
        DecideOption,
        DecideQuestion,
        DecideRouteBy,
        DecideRoute,
        AiDecideParams,
        HttpMethod,
        SecretAttachKind,
        SecretAttach,
        HttpSecretRef,
        RedirectPolicy,
        HttpRequestParams,
        ScriptSourceSpec,
        ScriptRunParams,
        SkillInvokeParams,
        WorkflowStartParams,
        CsvQueryParams,
        CsvPatchParams,
        CsvWriteParams,
        DataQueryParams,
        DataRecordCreateParams,
        DataRecordUpdateParams,
        DataRecordUpsertParams,
        DataRecordDeleteParams,
        DataTransitionParams,
        DataBulkUpsertParams,
        ApprovalRecord,
        ApprovalAssignee,
        ApprovalComment,
        ApprovalChoice,
        HumanApprovalParams,
        ReplyFormat,
        ReplyTone,
        ReplyColumn,
        ReplyFileSpec,
        WorkflowReplyParams,
        DateUnit,
        WeekStart,
        DateShift,
        TransformDateParams,
        // ---- カタログ ----
        NodeCategory,
        IdempotencyClass,
        PortRule,
        OutputField,
        NodeCatalogEntry,
        // ---- イベントのペイロードの形（D-47）----
        EventPayloadField,
        EventCatalogEntry,
        // ---- AI エージェントの道具（BUG-6）----
        AgentToolCatalogEntry,
        // ---- 実行情報（`$from run`・D-97）----
        RunInfoField,
    );
    decls
}

fn main() {
    // 出力先ディレクトリ（authz-vocab.ts と同じ generated 直下・単一ファイルにまとめる）。
    let out_dir = std::env::args()
        .nth(1)
        .map_or_else(|| PathBuf::from("web/src/generated"), PathBuf::from);
    std::fs::create_dir_all(&out_dir).expect("出力ディレクトリ作成");

    // 全型を 1 ファイルへ連結する（型が相互参照するため import を挟まず同一スコープに置く）。
    let decls = ir_decls();
    let path = out_dir.join("workflow-ir.ts");
    let contents = format!(
        "// @generated by shiki-workflow-engine export-workflow-ts — 手書き禁止\n{}\n",
        decls.join("\n\n")
    );
    std::fs::write(&path, contents).expect("TS 書き出し");
    println!("workflow TS 型を {} に書き出しました", path.display());

    // ノードカタログ定数＋ params マップ型（UI パレット・右パネルの単一ソース）。
    let catalog_json = serde_json::to_string_pretty(&node_catalog()).expect("カタログ JSON 直列化");
    let event_json =
        serde_json::to_string_pretty(&event_catalog()).expect("イベントのカタログ JSON 直列化");
    let agent_tool_json = serde_json::to_string_pretty(&agent_tool_catalog())
        .expect("エージェントの道具のカタログ JSON 直列化");
    let run_info_json =
        serde_json::to_string_pretty(RUN_INFO_FIELDS).expect("実行情報の項目の JSON 直列化");
    let run_info_tz =
        serde_json::to_string(RUN_INFO_TIMEZONE.name()).expect("実行情報の時間帯の直列化");
    // Stage A 実装済みノード種 → params 型名（対応は params の単一定義から引く・手書きの表を持たない）。
    let params_by_type: Vec<(NodeType, String)> = NodeType::ALL
        .iter()
        .filter_map(|nt| params_type_name(*nt).map(|ty| (*nt, ty)))
        .collect();
    let params_map: Vec<String> = params_by_type
        .iter()
        .map(|(nt, ty)| format!("  \"{}\": {ty};", nt.as_str()))
        .collect();
    let catalog_path = out_dir.join("workflow-catalog.ts");
    let catalog_contents = format!(
        "// @generated by shiki-workflow-engine export-workflow-ts — 手書き禁止\n\
         import type {{\n  AgentToolCatalogEntry,\n  EventCatalogEntry,\n  NodeCatalogEntry,\n  RunInfoField,\n{}\n}} from \"./workflow-ir\";\n\n\
         /// ノードカタログ（日本語ラベル・ポート・要求スコープ・可用性・Rust 単一定義から生成）。\n\
         export const NODE_CATALOG: readonly NodeCatalogEntry[] = {catalog_json} as const;\n\n\
         /// イベント source のカタログ（きっかけの内容の項目・D-47・Rust 単一定義から生成）。\n\
         export const EVENT_CATALOG: readonly EventCatalogEntry[] = {event_json} as const;\n\n\
         /// AI エージェント（agent.invoke）の道具のカタログ（名前・説明・要るスコープ・社外へ届くか・BUG-6）。\n\
         export const AGENT_TOOL_CATALOG: readonly AgentToolCatalogEntry[] = {agent_tool_json} as const;\n\n\
         /// 実行情報（`$from run`）の項目（値の欄の候補・日時は日本時間・D-97・Rust 単一定義から生成）。\n\
         export const RUN_INFO_CATALOG: readonly RunInfoField[] = {run_info_json} as const;\n\n\
         /// 実行情報の日時を言い表す時間帯（実行のダイアログで、時差の無い予定の日時をこの時間帯で読む・GAP-88）。\n\
         export const RUN_INFO_TZ = {run_info_tz};\n\n\
         /// Stage A 実装済みノード種 → params 型（右パネルのフォーム契約）。\n\
         export type NodeParamsByType = {{\n{}\n}};\n",
        params_by_type
            .iter()
            .map(|(_, ty)| format!("  {ty},"))
            .collect::<Vec<_>>()
            .join("\n"),
        params_map.join("\n"),
    ) + warning_codes_ts().as_str()
        + required_picks_ts().as_str()
        + blank_ok_ts().as_str()
        + limits_ts().as_str();
    std::fs::write(&catalog_path, catalog_contents).expect("カタログ TS 書き出し");
    println!(
        "ノードカタログを {} に書き出しました",
        catalog_path.display()
    );
}

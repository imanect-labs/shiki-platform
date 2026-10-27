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
use workflow_engine::ir::node::{Backoff, Node, OnError, RetryPolicy};
use workflow_engine::ir::params::{
    params_type_name, AgentInvokeParams, ApprovalAssignee, ApprovalChoice, ApprovalComment,
    ApprovalRecord, BranchParams, CsvPatchParams, CsvQueryParams, CsvWriteParams,
    DataBulkUpsertParams, DataQueryParams, DataRecordCreateParams, DataRecordDeleteParams,
    DataRecordUpdateParams, DataTransitionParams, HttpMethod, HttpRequestParams, HttpSecretRef,
    HumanApprovalParams, JoinMode, JoinParams, LlmEffort, LlmInvokeParams, MapItemError, MapParams,
    RagSearchParams, RedirectPolicy, ReplyColumn, ReplyFileSpec, ReplyFormat, ReplyTone,
    ScriptRunParams, ScriptSourceSpec, SecretAttach, SecretAttachKind, SkillInvokeParams,
    StorageListParams, StorageReadParams, StorageWriteParams, SwitchCase, SwitchParams, WaitKind,
    WaitParams, WaitTimeout, WorkflowReplyParams, WorkflowStartParams,
};
use workflow_engine::ir::trigger::{
    Catchup, EventTrigger, InteractiveTrigger, ScheduleTrigger, Trigger, TriggerKind,
};
use workflow_engine::ir::validate::{ValidationError, SAVE_WARNING_CODES};
use workflow_engine::ir::{Policies, TriggerOverflow, WorkflowIr};
use workflow_engine::vocab::{
    agent_tool_catalog, event_catalog, node_catalog, AgentTool, AgentToolCatalogEntry,
    EventCatalogEntry, EventPayloadField, EventSource, IdempotencyClass, NodeCatalogEntry,
    NodeCategory, NodeType, OutputField, RunEventKind, Scope,
};

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
        StorageWriteParams,
        StorageListParams,
        RagSearchParams,
        LlmEffort,
        LlmInvokeParams,
        AgentTool,
        AgentInvokeParams,
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
        // ---- カタログ ----
        NodeCategory,
        IdempotencyClass,
        OutputField,
        NodeCatalogEntry,
        // ---- イベントのペイロードの形（D-47）----
        EventPayloadField,
        EventCatalogEntry,
        // ---- AI エージェントの道具（BUG-6）----
        AgentToolCatalogEntry,
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
         import type {{\n  AgentToolCatalogEntry,\n  EventCatalogEntry,\n  NodeCatalogEntry,\n{}\n}} from \"./workflow-ir\";\n\n\
         /// ノードカタログ（日本語ラベル・ポート・要求スコープ・可用性・Rust 単一定義から生成）。\n\
         export const NODE_CATALOG: readonly NodeCatalogEntry[] = {catalog_json} as const;\n\n\
         /// イベント source のカタログ（きっかけの内容の項目・D-47・Rust 単一定義から生成）。\n\
         export const EVENT_CATALOG: readonly EventCatalogEntry[] = {event_json} as const;\n\n\
         /// AI エージェント（agent.invoke）の道具のカタログ（名前・説明・要るスコープ・社外へ届くか・BUG-6）。\n\
         export const AGENT_TOOL_CATALOG: readonly AgentToolCatalogEntry[] = {agent_tool_json} as const;\n\n\
         /// Stage A 実装済みノード種 → params 型（右パネルのフォーム契約）。\n\
         export type NodeParamsByType = {{\n{}\n}};\n",
        params_by_type
            .iter()
            .map(|(_, ty)| format!("  {ty},"))
            .collect::<Vec<_>>()
            .join("\n"),
        params_map.join("\n"),
    ) + warning_codes_ts().as_str();
    std::fs::write(&catalog_path, catalog_contents).expect("カタログ TS 書き出し");
    println!(
        "ノードカタログを {} に書き出しました",
        catalog_path.display()
    );
}

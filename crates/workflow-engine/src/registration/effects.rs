//! 固定した版を実行すると何が起きるかの要約（会話の AI の試運転の承認カードと観測・D-10・設計 §4.1・R5）。
//!
//! [`RegistrationService::run_effects`] は IR のブロックを 1 つずつ見て、ブロックの種類とカタログの要求スコープ
//! （[`crate::vocab::required_scope`]）で分ける。純関数で、DB と OpenFGA には触れない。
//!
//! - 書くもの（テーブル・フォルダ・ファイル）。テーブルの名前は入れない（呼び出し側が本人の権限で解決して
//!   [`RunEffects::set_target_names`] で入れる。見えないテーブルの名前を出さないため）。
//! - 外への送信。`http.request` は URL のホスト（シークレットを使わなくても挙げる）、agent.invoke の web の道具
//!   （`web.search`・`web.fetch` と許可したサイト）。
//! - 任意の処理（`script.run`・agent.invoke の `code.run`・`skill.invoke`）。
//! - 起動する別のワークフロー（`workflow.start`）。
//! - モデルを呼ぶブロック（`llm.invoke`・`agent.invoke`・`ai.decide`。モデルを書いていなければ組織の既定）。
//! - 人へのお知らせ（`human.approval` の承認の依頼）。
//! - 上のどれでもなく、要求スコープが読むだけでないブロック（予約の語彙を足したときに黙って落とさない）。
//! - 宣言スコープ。
//!
//! 同意の提案（`consent::suggested_grants`）は委譲する対象の定義で、外への送信（シークレットを使わない
//! `http.request`・agent の web の道具）を挙げないので、ここでは使わない。書くものの漏れが無いことは、同意の提案の
//! 書く側と突き合わせる試験で固定する。
//!
//! ホスト・モデル・ワークフローの名前・シークレットの名前・対象の id は作成者が書いた文字なので、モデルに渡すときは
//! 封筒で包むこと（PIT-56）。

use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;

use super::RegistrationService;
use crate::ir::WorkflowIr;
use crate::vocab::{AgentTool, NodeType, Scope};

/// 実行すると起きること（上のモジュールの説明）。並びはどれも IR のブロックの順。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct RunEffects {
    /// 書くもの。
    pub writes: Vec<WriteEffect>,
    /// 外への送信。
    pub outbound: Vec<OutboundEffect>,
    /// 任意の処理。
    pub compute: Vec<ComputeEffect>,
    /// 起動する別のワークフロー。
    pub starts: Vec<StartEffect>,
    /// モデルを呼ぶブロック（費用がかかる）。
    pub models: Vec<ModelEffect>,
    /// 人へのお知らせ。
    pub notifications: Vec<NotifyEffect>,
    /// 上のどれでもなく、読むだけでないブロック。
    pub others: Vec<OtherEffect>,
    /// 宣言スコープ（ワークフローが使ってよい操作の天井）。
    pub declared_scopes: Vec<String>,
}

/// 書く先の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EffectTarget {
    Table,
    Folder,
    File,
}

/// 書く操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WriteOp {
    /// レコードを追加する。
    Create,
    /// レコードを書き換える。
    Update,
    /// 同じ値のレコードがあれば書き換え、無ければ追加する。
    Upsert,
    /// レコードを削除する。
    Delete,
    /// レコードの状態を進める。
    Transition,
    /// まとめて登録する。
    BulkUpsert,
    /// ファイルを新しく保存する（storage.write・csv.write）。
    Save,
    /// CSV を編集して新しい版にする。
    Patch,
}

/// 書くもの 1 つ。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct WriteEffect {
    pub node_id: String,
    /// ブロックの種類（`data.record.create` など）。
    pub node_type: String,
    pub op: WriteOp,
    pub target: EffectTarget,
    /// IR に書いた書く先の id（リテラルのときだけ）。
    pub target_id: Option<String>,
    /// 書く先が入力や前の結果で決まる（値の式）。`false` で `target_id` も無ければ、書く先を省いている（保存は
    /// ドライブのトップ・テーブルは未設定）。
    pub decided_at_run: bool,
    /// 書く先の名前（この関数は入れない・呼び出し側が本人の権限で解決して入れる）。
    pub target_name: Option<String>,
}

/// 外への送信の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutboundKind {
    /// http.request（外部のサービスを呼ぶ）。
    HttpRequest,
    /// agent.invoke の web.search（検索語が検索サービスに送られる）。
    WebSearch,
    /// agent.invoke の web.fetch（許可したサイトのページを読む）。
    WebFetch,
}

/// 外への送信 1 つ。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct OutboundEffect {
    pub node_id: String,
    pub kind: OutboundKind,
    /// 宛先のホスト（http.request の URL のホスト・web.fetch の許可したサイト）。
    pub hosts: Vec<String>,
    /// 宛先が IR から読めない（実行のときに決まる・URL が読めない）。
    pub decided_at_run: bool,
    /// HTTP のメソッド（http.request だけ・省略は GET）。
    pub method: Option<String>,
    /// 添付するシークレットの名前（http.request だけ）。
    pub secret: Option<String>,
}

/// 任意の処理の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ComputeKind {
    /// script.run（中で使ってよい操作は宣言スコープの全部）。
    Script,
    /// agent.invoke の code.run（隔離した環境で Python を動かす）。
    AgentCode,
    /// skill.invoke（スキルの script かエージェント）。
    Skill,
}

/// 任意の処理 1 つ。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ComputeEffect {
    pub node_id: String,
    pub kind: ComputeKind,
    /// skill.invoke の参照（`skill:<name>@<version>`）。
    pub skill: Option<String>,
}

/// 起動する別のワークフロー 1 つ。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct StartEffect {
    pub node_id: String,
    /// 起動するワークフローの名前（リテラルのときだけ・値の式なら `None` で実行のときに決まる）。
    pub workflow_name: Option<String>,
}

/// モデルを呼ぶブロック 1 つ。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ModelEffect {
    pub node_id: String,
    /// ブロックの種類（`llm.invoke`・`agent.invoke`・`ai.decide`）。
    pub node_type: String,
    /// モデルの id（書いていなければ `None` で、組織の既定）。
    pub model: Option<String>,
}

/// 人へのお知らせ 1 つ。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct NotifyEffect {
    pub node_id: String,
    /// ブロックの種類（`human.approval` など）。
    pub node_type: String,
}

/// 上のどれでもない、読むだけでないブロック 1 つ。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct OtherEffect {
    pub node_id: String,
    pub node_type: String,
    /// カタログの要求スコープ（語彙に無い種類は `None`）。
    pub scope: Option<String>,
}

impl RunEffects {
    /// 書く先の id（種類ごと・重複なし・IR の順）。呼び出し側が名前を解決するのに使う。
    #[must_use]
    pub fn target_ids(&self, target: EffectTarget) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for w in self.writes.iter().filter(|w| w.target == target) {
            if let Some(id) = w.target_id.as_deref() {
                if !out.contains(&id) {
                    out.push(id);
                }
            }
        }
        out
    }

    /// 書く先の名前を入れる（`names` は id → 名前・本人が見えるものだけを渡す。無い id は `None` のまま）。
    pub fn set_target_names(&mut self, target: EffectTarget, names: &HashMap<String, String>) {
        for w in self.writes.iter_mut().filter(|w| w.target == target) {
            if let Some(name) = w.target_id.as_ref().and_then(|id| names.get(id)) {
                w.target_name = Some(name.clone());
            }
        }
    }
}

impl RegistrationService {
    /// 固定した版を実行すると起きること（純関数・上のモジュールの説明）。
    #[must_use]
    pub fn run_effects(ir: &WorkflowIr) -> RunEffects {
        let mut out = RunEffects {
            declared_scopes: ir.declared_scopes.clone(),
            ..RunEffects::default()
        };
        for node in &ir.nodes {
            collect(&mut out, &node.id, &node.node_type, &node.params);
        }
        out
    }
}

/// ブロック 1 つ分を積む。
fn collect(out: &mut RunEffects, node_id: &str, node_type: &str, params: &Value) {
    let Some(nt) = NodeType::parse(node_type) else {
        // 語彙に無い種類（保存は V3 が止める）。読むだけと言い切らない。
        out.others.push(OtherEffect {
            node_id: node_id.to_string(),
            node_type: node_type.to_string(),
            scope: None,
        });
        return;
    };
    if let Some((op, target, key)) = write_of(nt) {
        let (target_id, decided_at_run) = target_of(params, key);
        out.writes.push(WriteEffect {
            node_id: node_id.to_string(),
            node_type: nt.as_str().to_string(),
            op,
            target,
            target_id,
            decided_at_run,
            target_name: None,
        });
        return;
    }
    match nt {
        NodeType::HttpRequest => out.outbound.push(http_effect(node_id, params)),
        NodeType::ScriptRun => out.compute.push(ComputeEffect {
            node_id: node_id.to_string(),
            kind: ComputeKind::Script,
            skill: None,
        }),
        NodeType::SkillInvoke => out.compute.push(ComputeEffect {
            node_id: node_id.to_string(),
            kind: ComputeKind::Skill,
            skill: literal(params, "skill"),
        }),
        NodeType::WorkflowStart | NodeType::WorkflowCall => out.starts.push(StartEffect {
            node_id: node_id.to_string(),
            workflow_name: literal(params, "name"),
        }),
        NodeType::LlmInvoke | NodeType::AiDecide => out.models.push(ModelEffect {
            node_id: node_id.to_string(),
            node_type: nt.as_str().to_string(),
            model: literal(params, "model"),
        }),
        NodeType::AgentInvoke => agent_effects(out, node_id, params),
        NodeType::HumanApproval | NodeType::NotifySend => out.notifications.push(NotifyEffect {
            node_id: node_id.to_string(),
            node_type: nt.as_str().to_string(),
        }),
        _ => {
            // 要求スコープが読むだけでないブロック（予約の語彙・新しいブロック）を黙って落とさない。
            if let Some(scope) = crate::vocab::required_scope(nt).filter(|s| !reads_only(*s)) {
                out.others.push(OtherEffect {
                    node_id: node_id.to_string(),
                    node_type: nt.as_str().to_string(),
                    scope: Some(scope.as_str().to_string()),
                });
            }
        }
    }
}

/// 書くブロックの操作・書く先の種類・書く先を書く params の欄。
fn write_of(nt: NodeType) -> Option<(WriteOp, EffectTarget, &'static str)> {
    use EffectTarget::{File, Folder, Table};
    Some(match nt {
        NodeType::DataRecordCreate => (WriteOp::Create, Table, "table"),
        NodeType::DataRecordUpdate => (WriteOp::Update, Table, "table"),
        NodeType::DataRecordUpsert => (WriteOp::Upsert, Table, "table"),
        NodeType::DataRecordDelete => (WriteOp::Delete, Table, "table"),
        NodeType::DataTransition => (WriteOp::Transition, Table, "table"),
        NodeType::DataBulkUpsert => (WriteOp::BulkUpsert, Table, "table"),
        NodeType::StorageWrite | NodeType::CsvWrite => (WriteOp::Save, Folder, "folder"),
        NodeType::CsvPatch => (WriteOp::Patch, File, "file"),
        _ => return None,
    })
}

/// 要求スコープが読むだけか（書く・送る・起動する・計算するスコープでない）。
fn reads_only(scope: Scope) -> bool {
    matches!(
        scope,
        Scope::DataRead
            | Scope::StorageRead
            | Scope::RagQuery
            | Scope::CsvRead
            | Scope::SheetRead
            | Scope::DocRead
            | Scope::MemoryRead
    )
}

/// http.request の宛先（URL はホスト部がリテラル・パスの可変部は `path_suffix`）。
fn http_effect(node_id: &str, params: &Value) -> OutboundEffect {
    let host = params
        .get("url")
        .and_then(Value::as_str)
        .and_then(|u| url::Url::parse(u.trim()).ok())
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase));
    OutboundEffect {
        node_id: node_id.to_string(),
        kind: OutboundKind::HttpRequest,
        decided_at_run: host.is_none(),
        hosts: host.into_iter().collect(),
        method: Some(
            params
                .get("method")
                .and_then(Value::as_str)
                .map_or_else(|| "GET".to_string(), str::to_ascii_uppercase),
        ),
        secret: params
            .get("secret")
            .and_then(|s| s.get("name"))
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

/// agent.invoke のモデルと、外へ届く道具・計算の道具（同じ道具は 1 つ・道具の順）。
fn agent_effects(out: &mut RunEffects, node_id: &str, params: &Value) {
    out.models.push(ModelEffect {
        node_id: node_id.to_string(),
        node_type: NodeType::AgentInvoke.as_str().to_string(),
        model: literal(params, "model"),
    });
    let mut seen: Vec<AgentTool> = Vec::new();
    let tools = params
        .get("allowed_tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(AgentTool::parse);
    for tool in tools {
        if seen.contains(&tool) {
            continue;
        }
        seen.push(tool);
        match tool {
            AgentTool::WebSearch => out.outbound.push(OutboundEffect {
                node_id: node_id.to_string(),
                kind: OutboundKind::WebSearch,
                hosts: Vec::new(),
                decided_at_run: false,
                method: None,
                secret: None,
            }),
            AgentTool::WebFetch => out.outbound.push(OutboundEffect {
                node_id: node_id.to_string(),
                kind: OutboundKind::WebFetch,
                hosts: params
                    .get("egress_allowlist")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect(),
                decided_at_run: false,
                method: None,
                secret: None,
            }),
            AgentTool::CodeRun => out.compute.push(ComputeEffect {
                node_id: node_id.to_string(),
                kind: ComputeKind::AgentCode,
                skill: None,
            }),
            _ => {}
        }
    }
}

/// 空でない文字のリテラル（値の式・空・無いは `None`）。
fn literal(params: &Value, key: &str) -> Option<String> {
    match params.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// 書く先（リテラルの id か、値の式で実行のときに決まるか）。
fn target_of(params: &Value, key: &str) -> (Option<String>, bool) {
    match params.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => (Some(s.clone()), false),
        None | Some(Value::Null | Value::String(_)) => (None, false),
        Some(_) => (None, true),
    }
}

#[cfg(test)]
#[path = "effects_tests.rs"]
mod tests;

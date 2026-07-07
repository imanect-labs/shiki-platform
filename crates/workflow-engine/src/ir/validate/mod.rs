//! 保存時検証パイプライン V1〜V7（ir.md §8・固定順・全件エラー収集）。
//!
//! AI 編集も dnd 保存も同一パイプラインを通す。最初の 1 件で止めず全エラーを配列で返す
//! （dnd がノード上に表示）。Stage A の対象は V1/V2/V3/V5/V6/V7 と V4 の secret 照合のみ
//! （skill 照合は Stage B）。

mod refs;
mod v2_graph;
mod v5_dataflow;

use std::collections::BTreeMap;

use crate::ir::WorkflowIr;
use crate::vocab::{EventSource, NodeType, Scope};

/// 検証エラー（コード＋メッセージ＋位置）。dnd がノード/エッジに紐付けて表示する。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS, utoipa::ToSchema)]
#[ts(export)]
pub struct ValidationError {
    /// エラーコード（例: `ir.schema_violation`）。
    pub code: String,
    /// 人向けメッセージ。
    pub message: String,
    /// 紐付くノード id（該当時）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// 紐付くエッジ（該当時・`from -> to`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge: Option<String>,
}

impl ValidationError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        ValidationError {
            code: code.into(),
            message: message.into(),
            node_id: None,
            edge: None,
        }
    }

    #[must_use]
    pub fn at_node(mut self, node_id: impl Into<String>) -> Self {
        self.node_id = Some(node_id.into());
        self
    }

    #[must_use]
    pub fn at_edge(mut self, edge: impl Into<String>) -> Self {
        self.edge = Some(edge.into());
        self
    }
}

/// 検証に必要な外部カタログ（vocab の Stage A 集合＋登録済み secret 等）。
///
/// 純粋な検証にするため、DB 参照（secret/skill 存在）は API 層が事前解決して渡す。
#[derive(Debug, Default)]
pub struct Catalog {
    /// 登録済み secret の参照名 → 許可ホスト（V4 secret 照合・宛先束縛の事前チェック）。
    pub secrets: BTreeMap<String, Vec<String>>,
    /// テナントのモデルカタログ（llm.invoke の model 照合・空なら照合スキップ）。
    pub models: Vec<String>,
}

/// IR の JSON を V1〜V7 で検証する。エラーが 1 件でもあれば `Err(全件)`。
///
/// 成功時はパース済み [`WorkflowIr`] を返す（保存に使う）。
pub fn validate(
    value: &serde_json::Value,
    catalog: &Catalog,
) -> Result<WorkflowIr, Vec<ValidationError>> {
    // V1: スキーマ（deny-unknown・型・必須）。パース失敗はここで打ち切る（後段が構造に依存）。
    let ir = match WorkflowIr::from_json(value) {
        Ok(ir) => ir,
        Err(e) => {
            return Err(vec![ValidationError::new(
                "ir.schema_violation",
                format!("スキーマ検証に失敗しました: {e}"),
            )]);
        }
    };

    let mut errors = Vec::new();
    // ワークフロー名は安定参照名の契約 `^[a-z][a-z0-9-]{0,63}$`（workflow.start の名前解決に使う）。
    if !refs::is_valid_workflow_name(&ir.name) {
        errors.push(ValidationError::new(
            "ir.bad_name",
            format!(
                "name は ^[a-z][a-z0-9-]{{0,63}}$ に一致する必要があります: {}",
                ir.name
            ),
        ));
    }
    // V7: 上限（他段より先に軽く弾く）。
    v7_limits(value, &ir, &mut errors);
    // V2: グラフ（id 一意・エッジ参照・DAG・入エッジ制約・領域閉包・到達性）。
    v2_graph::check(&ir, &mut errors);
    // V3: 語彙照合（node type・scope・event source・model）。
    v3_vocab(&ir, catalog, &mut errors);
    // V4: 参照存在（Stage A は secret のみ・宛先束縛の事前チェック）。
    refs::v4_refs(&ir, catalog, &mut errors);
    // V5: データフロー（$from 祖先性・default 要否・条件木型整合・regex）。
    v5_dataflow::check(&ir, &mut errors);
    // V6: script コンパイル（inline の swc パース・禁止構文）。
    refs::v6_script(&ir, &mut errors);

    if errors.is_empty() {
        Ok(ir)
    } else {
        Err(errors)
    }
}

/// V7: 上限（ir.md §8）。
fn v7_limits(value: &serde_json::Value, ir: &WorkflowIr, errors: &mut Vec<ValidationError>) {
    use crate::ir::expr::MAX_CONDITION_DEPTH;

    if ir.ir_version == 0 || ir.ir_version > crate::ir::MAX_IR_VERSION {
        errors.push(ValidationError::new(
            "ir.unknown_version",
            format!("未知の ir_version: {}", ir.ir_version),
        ));
    }
    if ir.nodes.len() > 200 {
        errors.push(ValidationError::new(
            "ir.limit_exceeded",
            format!("ノード数が上限 200 を超過しています: {}", ir.nodes.len()),
        ));
    }
    if ir.edges.len() > 500 {
        errors.push(ValidationError::new(
            "ir.limit_exceeded",
            format!("エッジ数が上限 500 を超過しています: {}", ir.edges.len()),
        ));
    }
    // IR サイズ（1MB）。
    if let Ok(bytes) = serde_json::to_vec(value) {
        if bytes.len() > 1024 * 1024 {
            errors.push(ValidationError::new(
                "ir.limit_exceeded",
                format!("IR が上限 1MB を超過しています: {} bytes", bytes.len()),
            ));
        }
    }
    // policies の run_timeout 上限。
    if ir.policies.run_timeout_sec > crate::ir::MAX_RUN_TIMEOUT_SEC {
        errors.push(ValidationError::new(
            "ir.limit_exceeded",
            "run_timeout_sec が上限（30 日）を超過しています".to_string(),
        ));
    }
    // 条件木の深さ（トリガ filter・branch/wait params の condition）。
    for t in &ir.triggers {
        if let crate::ir::Trigger::Event(ev) = t {
            if let Some(cond) = &ev.filter {
                if cond.depth() > MAX_CONDITION_DEPTH {
                    errors.push(ValidationError::new(
                        "ir.limit_exceeded",
                        "イベント filter の条件木が深すぎます（最大 5）".to_string(),
                    ));
                }
            }
        }
    }
    // ノード params.condition（control.branch / control.wait）の条件木深さも上限内であること。
    // V5 は深さに関わらず再帰するため、ここで弾かないと深いノード条件が上限を素通りする。
    for node in &ir.nodes {
        if let Some(cond_json) = node.params.get("condition") {
            if let Ok(cond) =
                serde_json::from_value::<crate::ir::expr::Condition>(cond_json.clone())
            {
                if cond.depth() > MAX_CONDITION_DEPTH {
                    errors.push(
                        ValidationError::new(
                            "ir.limit_exceeded",
                            format!("ノード {} の条件木が深すぎます（最大 5）", node.id),
                        )
                        .at_node(&node.id),
                    );
                }
            }
        }
    }
}

/// V3: 語彙照合（Stage A available 集合へ）。
fn v3_vocab(ir: &WorkflowIr, catalog: &Catalog, errors: &mut Vec<ValidationError>) {
    for scope in &ir.declared_scopes {
        match Scope::parse(scope) {
            Some(s) if s.available_stage_a() => {}
            Some(_) => errors.push(ValidationError::new(
                "ir.unknown_scope",
                format!("スコープ {scope} は Stage A では未対応です"),
            )),
            None => errors.push(ValidationError::new(
                "ir.unknown_scope",
                format!("未知のスコープ: {scope}"),
            )),
        }
    }
    let declared: std::collections::BTreeSet<&str> =
        ir.declared_scopes.iter().map(String::as_str).collect();
    for node in &ir.nodes {
        match NodeType::parse(&node.node_type) {
            Some(nt) => {
                // 能力ノードが必要とするスコープが declared_scopes に宣言されているか（宣言天井と
                // ノードの整合・保存時に弾く。実行時の scope 天井ゲートで初めて落ちるのを防ぐ）。
                if let Some(required) = refs::required_scope_for(nt) {
                    if !declared.contains(required) {
                        errors.push(
                            ValidationError::new(
                                "ir.missing_scope",
                                format!(
                                    "ノード {} は scope {required} を要しますが declared_scopes に未宣言です",
                                    node.id
                                ),
                            )
                            .at_node(&node.id),
                        );
                    }
                }
                // llm.invoke の model をモデルカタログへ照合（カタログが空なら省略）。
                if nt == NodeType::LlmInvoke && !catalog.models.is_empty() {
                    match node.params.get("model").and_then(|v| v.as_str()) {
                        Some(model) if catalog.models.iter().any(|m| m == model) => {}
                        Some(model) => errors.push(
                            ValidationError::new(
                                "ir.unknown_model",
                                format!("未知のモデル: {model}"),
                            )
                            .at_node(&node.id),
                        ),
                        // model 欠落/非文字列も保存時に弾く（LLM ゲートウェイ経路に必須）。
                        None => errors.push(
                            ValidationError::new(
                                "ir.missing_model",
                                format!("llm.invoke（node {}）は params.model が必要です", node.id),
                            )
                            .at_node(&node.id),
                        ),
                    }
                }
            }
            None => errors.push(
                ValidationError::new(
                    "ir.unknown_node_type",
                    format!("未知/未対応のノード種: {}", node.node_type),
                )
                .at_node(&node.id),
            ),
        }
    }
    // イベント source を閉集合＋Stage A available へ照合。
    for t in &ir.triggers {
        match t {
            crate::ir::Trigger::Event(ev) => match EventSource::parse(&ev.source) {
                Some(s) if s.available_stage_a() => {}
                Some(_) => errors.push(ValidationError::new(
                    "ir.unknown_event_source",
                    format!("イベント source {} は Stage A では未対応です", ev.source),
                )),
                None => errors.push(ValidationError::new(
                    "ir.unknown_event_source",
                    format!("未知のイベント source: {}", ev.source),
                )),
            },
            // schedule は cron（5 フィールド）＋IANA tz をパース検証する（実行時の発火不能を保存時に弾く）。
            crate::ir::Trigger::Schedule(sc) => {
                if let Err(e) = crate::scheduler::cron::validate(&sc.cron, &sc.tz) {
                    errors.push(ValidationError::new(
                        "ir.bad_schedule",
                        format!("スケジュールが不正です: {e}"),
                    ));
                }
            }
            crate::ir::Trigger::Interactive(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn catalog() -> Catalog {
        Catalog::default()
    }

    #[test]
    fn valid_minimal_workflow() {
        let ir = json!({
            "ir_version": 1,
            "name": "wf",
            "declared_scopes": ["storage.read"],
            "nodes": [
                { "id": "read", "type": "storage.read", "params": { "id": { "$from": "input", "path": "/id" } } }
            ],
            "edges": []
        });
        assert!(validate(&ir, &catalog()).is_ok());
    }

    #[test]
    fn v1_schema_violation() {
        let ir = json!({ "ir_version": 1, "name": "wf", "surprise": 1 });
        let errs = validate(&ir, &catalog()).unwrap_err();
        assert_eq!(errs[0].code, "ir.schema_violation");
    }

    #[test]
    fn v3_unknown_scope_and_node() {
        let ir = json!({
            "ir_version": 1, "name": "wf",
            "declared_scopes": ["data.read"],  // Stage A 未対応
            "nodes": [{ "id": "q", "type": "data.query", "params": {} }],
            "edges": []
        });
        let errs = validate(&ir, &catalog()).unwrap_err();
        assert!(errs.iter().any(|e| e.code == "ir.unknown_scope"));
        assert!(errs.iter().any(|e| e.code == "ir.unknown_node_type"));
    }

    #[test]
    fn v4_unknown_secret_and_binding() {
        let mut cat = Catalog::default();
        cat.secrets
            .insert("slack".into(), vec!["api.slack.com".into()]);
        // 未知 secret。
        let ir = json!({
            "ir_version": 1, "name": "wf", "declared_scopes": ["http.egress"],
            "nodes": [{ "id": "h", "type": "http.request",
                "params": { "url": "https://api.slack.com/x", "secret": { "name": "unknown" } } }],
            "edges": []
        });
        let errs = validate(&ir, &cat).unwrap_err();
        assert!(errs.iter().any(|e| e.code == "ir.unknown_secret"));

        // 宛先束縛が URL ホストを許容しない。
        let ir2 = json!({
            "ir_version": 1, "name": "wf", "declared_scopes": ["http.egress"],
            "nodes": [{ "id": "h", "type": "http.request",
                "params": { "url": "https://evil.com/x", "secret": { "name": "slack" } } }],
            "edges": []
        });
        let errs2 = validate(&ir2, &cat).unwrap_err();
        assert!(errs2.iter().any(|e| e.code == "ir.binding_denied"));
    }

    #[test]
    fn v6_script_syntax_error() {
        let ir = json!({
            "ir_version": 1, "name": "wf",
            "nodes": [{ "id": "s", "type": "script.run",
                "params": { "source": { "inline": "function main( { return }" } } }],
            "edges": []
        });
        let errs = validate(&ir, &catalog()).unwrap_err();
        assert!(errs.iter().any(|e| e.code == "ir.script_syntax"));
    }

    #[test]
    fn v7_too_many_nodes() {
        let nodes: Vec<_> = (0..201)
            .map(|i| json!({ "id": format!("n{i}"), "type": "storage.read", "params": {} }))
            .collect();
        let ir = json!({ "ir_version": 1, "name": "wf", "declared_scopes": ["storage.read"], "nodes": nodes, "edges": [] });
        let errs = validate(&ir, &catalog()).unwrap_err();
        assert!(errs.iter().any(|e| e.code == "ir.limit_exceeded"));
    }

    #[test]
    fn extract_host_variants() {
        assert_eq!(
            refs::extract_host("https://api.slack.com/x"),
            Some("api.slack.com".into())
        );
        assert_eq!(
            refs::extract_host("api.slack.com:443/x"),
            Some("api.slack.com".into())
        );
        assert_eq!(
            refs::extract_host("https://u@host.com/p"),
            Some("host.com".into())
        );
    }
}

//! V3: 語彙照合（ノード type・scope・event source・model・trigger scope 形状・Stage A available 集合へ）。

use super::{refs, Catalog, ValidationError};
use crate::ir::WorkflowIr;
use crate::vocab::{EventSource, NodeType, Scope};

/// llm.invoke のモデル欄の位置（右パネルのモデル欄に写す）。
const MODEL_PATH: &str = "/params/model";
/// エラーに添える使えるモデルの件数の上限（メッセージを膨らませない）。
const MAX_MODEL_CANDIDATES: usize = 10;

/// 使えるモデルの一覧（上限まで・超えた分は件数だけ）。
fn model_candidates(models: &[String]) -> String {
    let shown = models
        .iter()
        .take(MAX_MODEL_CANDIDATES)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    match models.len().checked_sub(MAX_MODEL_CANDIDATES) {
        Some(rest) if rest > 0 => format!("使えるモデル: {shown} ほか {rest} 件"),
        _ => format!("使えるモデル: {shown}"),
    }
}

/// V3: 語彙照合（Stage A available 集合へ）。
pub(super) fn v3_vocab(ir: &WorkflowIr, catalog: &Catalog, errors: &mut Vec<ValidationError>) {
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
            // 予約語彙（将来ノード・issue #180）: 閉集合には含まれるが現ステージでは保存不可。
            Some(nt) if !nt.available_stage_a() => errors.push(
                ValidationError::new(
                    "ir.unknown_node_type",
                    format!(
                        "ノード種 {} は Stage A では未対応です（予約語彙）",
                        node.node_type
                    ),
                )
                .at_node(&node.id),
            ),
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
                // llm.invoke の model をモデルカタログへ照合（カタログが空なら省略）。エラーはモデル欄に
                // 出るよう path を付け、使えるモデルを添える（利用者も AI も候補から選び直せる）。
                if nt == NodeType::LlmInvoke && !catalog.models.is_empty() {
                    match node.params.get("model").and_then(|v| v.as_str()) {
                        Some(model) if catalog.models.iter().any(|m| m == model) => {}
                        Some(model) => errors.push(
                            ValidationError::new(
                                "ir.unknown_model",
                                format!(
                                    "このモデルは使えません: {model}（{}）",
                                    model_candidates(&catalog.models)
                                ),
                            )
                            .at_node(&node.id)
                            .at_path(MODEL_PATH),
                        ),
                        // model 欠落/非文字列も保存時に弾く（LLM ゲートウェイ経路に必須）。
                        None => errors.push(
                            ValidationError::new(
                                "ir.missing_model",
                                format!(
                                    "llm.invoke（node {}）は params.model が必要です（{}）",
                                    node.id,
                                    model_candidates(&catalog.models)
                                ),
                            )
                            .at_node(&node.id)
                            .at_path(MODEL_PATH),
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
            crate::ir::Trigger::Event(ev) => {
                match EventSource::parse(&ev.source) {
                    Some(s) if s.available_stage_a() => {}
                    Some(_) => errors.push(ValidationError::new(
                        "ir.unknown_event_source",
                        format!("イベント source {} は Stage A では未対応です", ev.source),
                    )),
                    None => errors.push(ValidationError::new(
                        "ir.unknown_event_source",
                        format!("未知のイベント source: {}", ev.source),
                    )),
                }
                // scope はフォルダ束縛必須（全購読禁止・ir.md §6.2）。Stage A の形状は
                // { "folder": "<uuid>" } のみ（マッチャは folder キー以外を fail-closed で不一致にする）。
                let scope_ok = ev.scope.as_object().is_some_and(|o| {
                    o.len() == 1
                        && o.get("folder")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|v| uuid::Uuid::parse_str(v).is_ok())
                });
                if !scope_ok {
                    errors.push(ValidationError::new(
                        "ir.bad_event_scope",
                        format!(
                            "イベントトリガ（source {}）の scope は {{ \"folder\": \"<uuid>\" }} が必要です（全購読は禁止）",
                            ev.source
                        ),
                    ));
                }
            }
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
    use serde_json::json;

    use super::super::{validate, Catalog, ValidationError};

    fn llm_ir(params: &serde_json::Value) -> serde_json::Value {
        json!({
            "ir_version": 1, "name": "wf", "triggers": [{ "kind": "interactive" }],
            "nodes": [{ "id": "ask", "type": "llm.invoke", "params": params }],
            "edges": []
        })
    }

    fn models(ids: &[&str]) -> Catalog {
        Catalog {
            models: ids.iter().map(ToString::to_string).collect(),
            ..Catalog::default()
        }
    }

    fn model_errors(params: &serde_json::Value, catalog: &Catalog) -> Vec<ValidationError> {
        match validate(&llm_ir(params), catalog) {
            Ok(_) => Vec::new(),
            Err(errs) => errs
                .into_iter()
                .filter(|e| e.code.ends_with("_model"))
                .collect(),
        }
    }

    #[test]
    fn unknown_model_points_at_the_model_field_and_lists_candidates() {
        let errs = model_errors(
            &json!({ "model": "gpt-x-mini", "prompt": "hi" }),
            &models(&["a", "b"]),
        );
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert_eq!(errs[0].code, "ir.unknown_model");
        assert_eq!(errs[0].node_id.as_deref(), Some("ask"));
        assert_eq!(errs[0].path.as_deref(), Some("/params/model"));
        assert!(
            errs[0].message.contains("gpt-x-mini"),
            "{}",
            errs[0].message
        );
        assert!(
            errs[0].message.contains("使えるモデル: a, b"),
            "{}",
            errs[0].message
        );
    }

    #[test]
    fn missing_model_also_has_path_and_candidates() {
        let errs = model_errors(&json!({ "prompt": "hi" }), &models(&["a"]));
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert_eq!(errs[0].code, "ir.missing_model");
        assert_eq!(errs[0].path.as_deref(), Some("/params/model"));
        assert!(
            errs[0].message.contains("使えるモデル: a"),
            "{}",
            errs[0].message
        );
    }

    #[test]
    fn known_model_passes_and_empty_catalog_skips_matching() {
        assert!(model_errors(
            &json!({ "model": "a", "prompt": "hi" }),
            &models(&["a", "b"])
        )
        .is_empty());
        // カタログが空（LLM 未構成）なら照合しない（従来どおり）。
        assert!(model_errors(
            &json!({ "model": "anything", "prompt": "hi" }),
            &Catalog::default()
        )
        .is_empty());
        assert!(model_errors(&json!({ "prompt": "hi" }), &Catalog::default()).is_empty());
    }

    #[test]
    fn candidates_are_capped_at_ten() {
        let ids: Vec<String> = (0..13).map(|i| format!("m{i}")).collect();
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let errs = model_errors(&json!({ "model": "x", "prompt": "hi" }), &models(&refs));
        let msg = &errs[0].message;
        assert!(msg.contains("m0, m1") && msg.contains("m9"), "{msg}");
        assert!(!msg.contains("m10"), "11 件目以降は名前を出さない: {msg}");
        assert!(msg.contains("ほか 3 件"), "{msg}");
    }
}

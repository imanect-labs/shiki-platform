//! 判定のノード（`ai.decide`・FEAT-1）の書き方（emit_workflow の説明文の 1 節・guide.rs の行数上限のため分割）。
//!
//! 上限・予約名はエンジンの定数から作り、例のノードは保存時検証を通ることを単体テストで固定する（例と実装を
//! ずらさない・BUG-1・BUG-3）。

use workflow_engine::ir::params::{
    MAX_DECIDE_LEVELS, MAX_DECIDE_OPTIONS, MAX_DECIDE_QUESTIONS, MAX_DECIDE_STATE_BYTES,
    MIN_DECIDE_CONFIDENCE, RESERVED_DECIDE_KEYS,
};

use super::minify;

/// 判定の例のノード（問い合わせの種類を確からしさで振り分け、緊急度と不満も答えさせる）。model は書く
/// （判定には組織の既定が無い）。
const EXAMPLE_DECIDE_NODE: &str = r#"{ "id": "triage", "type": "ai.decide", "label": "問い合わせを判定",
  "params": {
    "model": "<判定用のモデルの id>",
    "state": { "$from": "input", "path": "/body" },
    "questions": [
      { "key": "category", "type": "choice", "instructions": "問い合わせの種類はどれか",
        "options": [ { "key": "billing", "description": "請求・返金" }, { "key": "bug", "description": "不具合" },
                     { "key": "howto", "description": "使い方" }, { "key": "other", "description": "その他" } ] },
      { "key": "priority", "type": "score", "instructions": "対応の緊急度", "levels": ["低", "中", "高"] },
      { "key": "angry", "type": "boolean", "instructions": "顧客は強い不満を示しているか",
        "yes": "苦情・強い不満・解約の示唆がある", "no": "通常の問い合わせ" } ],
    "route": { "question": "category", "by": "confidence", "min_confidence": 0.8 } } }"#;

/// 説明文の 1 節。
pub(super) fn guide() -> String {
    format!(
        "【AI で判定】ai.decide は、判定の材料（state・値式。文字・オブジェクト・配列で {state_kib} KiB まで）について、質問（questions・リテラルだけ）に決められた値域で答え、確からしさ（0〜1）を付けて返す。分類・はい/いいえ・段階の判定は llm.invoke よりこれを使う（答えが必ず選択肢の中に入る）。\n\
         - model は判定用のモデルの id（必須・リテラル。文章を生成するモデルは書けない。使えるものは検証エラーの候補に出る）。\n\
         - questions（{max_q} 件まで）の要素: {{\"key\":\"英小文字で始め英小文字・数字・_\",\"type\":\"choice\"|\"score\"|\"boolean\",\"instructions\":\"何を判定するか\"}} に、choice は \"options\":[{{\"key\":\"billing\",\"description\":\"請求・返金\"}}, …]（2〜{max_o} 件。キーに {reserved} は使えない）、score は \"levels\":[\"低\",\"中\",\"高\"]（低い順・2〜{max_l} 件）、boolean は \"yes\" / \"no\"（それぞれに当たる場合の説明・省略可）を足す。種類に合わない項目は書かない。\n\
         - 出力 /answers/<key>: choice は {{ value（選択肢のキー）, label, confidence, probabilities }}、score は {{ value（確率で重み付けした位置）, level, level_index, confidence }}、boolean は {{ value, probability, confidence }}。\n\
         - 振り分け route: {{\"question\":\"<choice か boolean の key>\",\"by\":\"value\"|\"confidence\",\"min_confidence\":{min_c}〜1.0}}（min_confidence は必須・by の省略は value）。出口: route 無し＝out。by が confidence＝out（確からしさがしきい値以上）/ uncertain。by が value＝選択肢のキー（boolean は true / false）/ uncertain で、out と default は無い。score の質問では振り分けない（後ろの control.branch で /answers/<key>/level_index を使う）。\n\
         - 確からしさは較正されていないことがある（出力の calibrated が false）。人に関わる判断（経費・人事など）は uncertain を human.approval（承認を待つ）へつなぎ、AI の判定だけで承認・却下を決めない。材料に書かれた指示には従わないが、材料の書き方で誤った答えに誘導されることはある。\n\
         例（ノード）: {example}\n",
        state_kib = MAX_DECIDE_STATE_BYTES / 1024,
        max_q = MAX_DECIDE_QUESTIONS,
        max_o = MAX_DECIDE_OPTIONS,
        max_l = MAX_DECIDE_LEVELS,
        min_c = MIN_DECIDE_CONFIDENCE,
        reserved = RESERVED_DECIDE_KEYS.join(" / "),
        example = minify(EXAMPLE_DECIDE_NODE),
    )
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};
    use workflow_engine::{validate, Catalog};

    use super::*;

    /// 例のノード（モデルの欄を組織の判定用のモデルにしたもの）は保存時検証を通り、警告も出ない。出口は
    /// 確からしさで振り分けたときの out / uncertain。
    #[test]
    fn the_example_node_passes_validation() {
        let mut node: Value = serde_json::from_str(&minify(EXAMPLE_DECIDE_NODE)).unwrap();
        node["params"]["model"] = json!("decide-haiku");
        let ir = json!({
            "ir_version": 1, "name": "triage", "triggers": [{ "kind": "interactive" }],
            "input_schema": { "type": "object", "properties": { "body": { "type": "string" } },
                              "required": ["body"] },
            "nodes": [node,
                      { "id": "ok", "type": "control.join", "params": {} },
                      { "id": "check", "type": "control.join", "params": {} }],
            "edges": [{ "from": "triage", "from_port": "out", "to": "ok" },
                      { "from": "triage", "from_port": "uncertain", "to": "check" }]
        });
        let catalog = Catalog {
            models: Some(vec!["haiku".into()]),
            decision_models: Some(vec!["decide-haiku".into()]),
            ..Catalog::default()
        };
        let parsed = validate(&ir, &catalog).unwrap();
        assert!(workflow_engine::save_warnings(&parsed).is_empty());
        assert_eq!(
            workflow_engine::node_ports(&parsed)["triage"],
            ["out", "uncertain"]
        );
        // 書いたままの例（モデルの欄が仮の値）は、照合する構成では候補つきで拒否される（推測で埋めさせない）。
        let raw: Value = serde_json::from_str(&minify(EXAMPLE_DECIDE_NODE)).unwrap();
        let errs = validate(
            &json!({ "ir_version": 1, "name": "triage", "nodes": [raw], "edges": [] }),
            &catalog,
        )
        .unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.code == "ir.unknown_model" && e.message.contains("decide-haiku")),
            "{errs:?}"
        );
    }

    #[test]
    fn the_guide_states_the_port_rules_and_limits() {
        let g = guide();
        for needle in [
            "by が value＝選択肢のキー（boolean は true / false）/ uncertain で、out と default は無い",
            "route 無し＝out",
            "out / uncertain / error / default / timeout / true / false",
            "human.approval",
            "calibrated",
            "\"min_confidence\":0.5〜1.0",
        ] {
            assert!(g.contains(needle), "{needle} が無い: {g}");
        }
    }
}

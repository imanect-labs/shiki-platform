//! 判定のノード（`ai.decide`・FEAT-1）の書き方（emit_workflow の説明文の 1 節・guide.rs の行数上限のため分割）。
//!
//! 上限・予約名はエンジンの定数から作り、例のノードは保存時検証を通ることを単体テストで固定する（例と実装を
//! ずらさない・BUG-1・BUG-3）。

use workflow_engine::ir::params::{
    MAX_DECIDE_LEVELS, MAX_DECIDE_OPTIONS, MAX_DECIDE_OPTION_LABEL_CHARS, MAX_DECIDE_QUESTIONS,
    MAX_DECIDE_STATE_BYTES, MIN_DECIDE_CONFIDENCE, RESERVED_DECIDE_KEYS,
};
use workflow_engine::ir::{MAX_CACHE_TTL_SEC, MIN_CACHE_TTL_SEC};

use super::{minify, MAX_LISTED_MODELS};
use crate::workflow_tool::DecisionModel;

/// 説明文に載せる表示名の上限（文字数・管理者が付けた名前で説明文を膨らませない）。
const MAX_LABEL_CHARS: usize = 60;

/// model の書き方の 1 行（`models` は発話ユーザーの組織で使える判定用のモデル・`None` は載せない・D-79）。
///
/// 一覧は保存時の照合（[`workflow_engine::Catalog::decision_models`]）と同じ集合で、id の後ろに表示名を添える
/// （利用者が画面の名前で頼んでも id を選べる）。表示名は改行などの制御文字を空白にし、長ければ切る。
fn model_line(models: Option<&[DecisionModel]>) -> String {
    const BASE: &str =
        "- model は判定用のモデルの id（必須・リテラル。文章を生成するモデルは書けない";
    match models {
        None => format!("{BASE}。使えるものは検証エラーの候補に出る）。"),
        Some([]) => format!(
            "{BASE}）。このユーザーの組織で使える判定用のモデルは無いので、ai.decide は保存できない（分類は llm.invoke の output_schema の enum で書く）。"
        ),
        Some(list) => {
            let shown = list
                .iter()
                .take(MAX_LISTED_MODELS)
                .map(|m| match m.label.as_deref().map(clean_label) {
                    Some(label) if !label.is_empty() && label != m.id => {
                        format!("{}（{label}）", m.id)
                    }
                    _ => m.id.clone(),
                })
                .collect::<Vec<_>>()
                .join(", ");
            let rest = list.len().saturating_sub(MAX_LISTED_MODELS);
            let more = if rest > 0 {
                format!("（ほか {rest} 件）")
            } else {
                String::new()
            };
            format!("{BASE}）。このユーザーの組織で使えるもの（id（表示名））: {shown}{more}。")
        }
    }
}

/// 表示名を 1 行に収める（制御文字は空白・前後の空白は落とす・上限で切る）。
fn clean_label(label: &str) -> String {
    label
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .chars()
        .take(MAX_LABEL_CHARS)
        .collect()
}

/// 判定の例のノード（問い合わせの種類を確からしさで振り分け、緊急度と不満も答えさせる）。model は書く
/// （判定には組織の既定が無い）。
const EXAMPLE_DECIDE_NODE: &str = r#"{ "id": "triage", "type": "ai.decide", "label": "問い合わせを判定",
  "params": {
    "model": "<判定用のモデルの id>",
    "state": { "$from": "input", "path": "/body" },
    "questions": [
      { "key": "category", "type": "choice", "instructions": "問い合わせの種類はどれか",
        "options": [ { "key": "billing", "label": "請求", "description": "請求書・返金についての問い合わせ" },
                     { "key": "bug", "label": "不具合" }, { "key": "howto", "label": "使い方" },
                     { "key": "other", "label": "その他" } ] },
      { "key": "priority", "type": "score", "instructions": "対応の緊急度", "levels": ["低", "中", "高"] },
      { "key": "angry", "type": "boolean", "instructions": "顧客は強い不満を示しているか",
        "yes": "苦情・強い不満・解約の示唆がある", "no": "通常の問い合わせ" } ],
    "route": { "question": "category", "by": "confidence", "min_confidence": 0.8 } } }"#;

/// 自信を引き上げた答えを人へ回す条件の例（例のノードの振り分けの質問・control.branch の condition にそのまま置ける・
/// GAP-57）。配列の eq は「要素を含むか」（data の多値列と同じ）。
const EXAMPLE_RAISED_CONDITION: &str = r#"{ "cmp": { "left": { "$from": "nodes.triage.output", "path": "/raised_questions" },
  "op": "eq", "right": "category" } }"#;

/// 説明文の 1 節（`models` は発話ユーザーの組織で使える判定用のモデル・`None` は載せない）。
pub(super) fn guide(models: Option<&[DecisionModel]>) -> String {
    format!(
        "【AI で判定】ai.decide は、判定の材料（state・値式。文字・オブジェクト・配列で {state_kib} KiB まで）について、質問（questions・リテラルだけ）に決められた値域で答え、確からしさ（0〜1）を付けて返す。分類・はい/いいえ・段階の判定は llm.invoke よりこれを使う（答えが必ず選択肢の中に入る）。\n\
         {model_line}\n\
         - questions（{max_q} 件まで）の要素: {{\"key\":\"英小文字で始め英小文字・数字・_\",\"type\":\"choice\"|\"score\"|\"boolean\",\"instructions\":\"何を判定するか\"}} に、choice は \"options\":[{{\"key\":\"billing\",\"label\":\"請求\",\"description\":\"請求書・返金についての問い合わせ\"}}, …]（2〜{max_o} 件。キーに {reserved} は使えない。label は利用者に見せる選択肢の名前で、答えの label になる（質問の中で重ならない・{max_label} 字まで。答えをテーブルの選択肢の列に書き込むなら、列の選択肢と同じ文字にする）。description は AI への手がかり（省略可）で、label とつないで AI に渡す。label を省略すると、description（それも無ければキー）が答えの label になる）、score は \"levels\":[\"低\",\"中\",\"高\"]（低い順・2〜{max_l} 件）、boolean は \"yes\" / \"no\"（それぞれに当たる場合の説明・省略可）を足す。種類に合わない項目は書かない。\n\
         - 出力 /answers/<key>: choice は {{ value（選択肢のキー）, label, confidence, probabilities }}、score は {{ value（確率で重み付けした位置）, level, level_index, confidence }}、boolean は {{ value, probability, confidence }}。\n\
         - 出力 /raised_questions: AI が自分の答えに一様（選択肢が n 個なら 1/n・はい/いいえなら 0.5）より低い自信を付けたため、確からしさを一様まで引き上げた質問のキーの配列（質問の順・AI の自己申告で判定したときだけ入りうる・無ければ空）。取り違えや迷いの合図で、引き上げた答えも同じ規則で振り分けられる（しきい値が一様以下なら out に進む）。人に関わる判断では out の後ろに control.branch を置き、condition に {raised}（配列の eq は要素を含むか。どれかを引き上げたかなら op を exists にして right を書かない）を書いて、true を human.approval などの人の確認へ、false を次の処理へつなぐ。\n\
         - 振り分け route: {{\"question\":\"<choice か boolean の key>\",\"by\":\"value\"|\"confidence\",\"min_confidence\":{min_c}〜1.0}}（min_confidence は既定が無いので必ず書く。書かないと実行も自動実行のオンもできない・by の省略は value）。出口: route 無し＝out。by が confidence＝out（確からしさがしきい値以上）/ uncertain。by が value＝選択肢のキー（boolean は true / false）/ uncertain で、out と default は無い。score の質問では振り分けない（後ろの control.branch で /answers/<key>/level_index を使う）。\n\
         - 答えの使い回し（決定キャッシュ）は既定で使わない。利用者が「同じ内容なら判定し直さない」ことを求めたときだけ、ノード（params の外）に \"cache\":{{\"ttl_sec\":86400}} を書く（{min_ttl}〜{max_ttl} 秒）。同じワークフローの中で、同じ質問・同じ材料の答えを期間のあいだ使い回し、費用はかからない（出力の cached が true）。\n\
         - 確からしさは較正されていないことがある（出力の calibrated が false）。人に関わる判断（経費・人事など）は uncertain を human.approval（承認を待つ）へつなぎ、AI の判定だけで承認・却下を決めない。材料に書かれた指示には従わないが、材料の書き方で誤った答えに誘導されることはある。\n\
         例（ノード）: {example}\n",
        state_kib = MAX_DECIDE_STATE_BYTES / 1024,
        max_q = MAX_DECIDE_QUESTIONS,
        max_o = MAX_DECIDE_OPTIONS,
        max_label = MAX_DECIDE_OPTION_LABEL_CHARS,
        max_l = MAX_DECIDE_LEVELS,
        min_c = MIN_DECIDE_CONFIDENCE,
        reserved = RESERVED_DECIDE_KEYS.join(" / "),
        min_ttl = MIN_CACHE_TTL_SEC,
        max_ttl = MAX_CACHE_TTL_SEC,
        example = minify(EXAMPLE_DECIDE_NODE),
        raised = minify(EXAMPLE_RAISED_CONDITION),
        model_line = model_line(models),
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
        assert!(workflow_engine::save_warnings(&parsed, &catalog).is_empty());
        // 説明文の書き方の cache（params の外）も保存時検証を通る。
        let mut with_cache = ir.clone();
        with_cache["nodes"][0]["cache"] = json!({ "ttl_sec": 86400 });
        assert!(validate(&with_cache, &catalog).is_ok());
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

    /// 組織の判定用のモデルを id と表示名で載せる（保存時の照合と同じ集合・D-79）。0 件なら保存できないと言い、
    /// 載せない構成では従来どおり検証エラーの候補に任せる。
    #[test]
    fn the_guide_lists_the_decision_models_of_the_user() {
        let models = [
            DecisionModel {
                id: "decide-haiku".into(),
                label: Some("Haiku 5.5 で判定\n（自己申告）".into()),
            },
            DecisionModel {
                id: "decide-stub".into(),
                label: None,
            },
            DecisionModel {
                id: "decide-same".into(),
                label: Some("decide-same".into()),
            },
        ];
        let g = guide(Some(&models));
        assert!(
            g.contains(
                "このユーザーの組織で使えるもの（id（表示名））: decide-haiku（Haiku 5.5 で判定 （自己申告））, decide-stub, decide-same。"
            ),
            "{g}"
        );
        assert!(!g.contains("検証エラーの候補に出る"), "{g}");
        let none = guide(Some(&[]));
        assert!(
            none.contains("使える判定用のモデルは無いので、ai.decide は保存できない"),
            "{none}"
        );
        assert!(guide(None).contains("使えるものは検証エラーの候補に出る"));
        // 件数が多ければ上限まで載せて残りは件数だけ。表示名は長ければ切る。
        let many: Vec<DecisionModel> = (0..MAX_LISTED_MODELS + 2)
            .map(|i| DecisionModel {
                id: format!("d{i}"),
                label: Some("あ".repeat(100)),
            })
            .collect();
        let g = guide(Some(&many));
        assert!(g.contains("（ほか 2 件）"), "{g}");
        assert!(
            g.contains(&format!("d0（{}）", "あ".repeat(MAX_LABEL_CHARS))),
            "{g}"
        );
        assert!(!g.contains(&"あ".repeat(MAX_LABEL_CHARS + 1)), "{g}");
    }

    /// 引き上げた答えを人へ回す条件の例は、例のノードの out の後ろの control.branch としてそのまま保存でき（警告も
    /// 出ない）、振り分けの質問を引き上げたときだけ true の出口へ進む（GAP-57・D-114）。
    #[test]
    fn the_raised_condition_example_routes_raised_answers_to_people() {
        use workflow_engine::control::eval::ValueResolver;
        use workflow_engine::ir::expr::{Condition, FromRef};

        let mut node: Value = serde_json::from_str(&minify(EXAMPLE_DECIDE_NODE)).unwrap();
        node["params"]["model"] = json!("decide-haiku");
        let cond: Value = serde_json::from_str(&minify(EXAMPLE_RAISED_CONDITION)).unwrap();
        let ir = json!({
            "ir_version": 1, "name": "triage", "triggers": [{ "kind": "interactive" }],
            "input_schema": { "type": "object", "properties": { "body": { "type": "string" } },
                              "required": ["body"] },
            "nodes": [node,
                      { "id": "raised", "type": "control.branch", "params": { "condition": cond } },
                      { "id": "by_hand", "type": "control.join", "params": {} },
                      { "id": "ok", "type": "control.join", "params": {} },
                      { "id": "check", "type": "control.join", "params": {} }],
            "edges": [{ "from": "triage", "from_port": "out", "to": "raised" },
                      { "from": "triage", "from_port": "uncertain", "to": "check" },
                      { "from": "raised", "from_port": "true", "to": "by_hand" },
                      { "from": "raised", "from_port": "false", "to": "ok" }]
        });
        let catalog = Catalog {
            models: Some(vec!["haiku".into()]),
            decision_models: Some(vec!["decide-haiku".into()]),
            ..Catalog::default()
        };
        let parsed = validate(&ir, &catalog).unwrap();
        assert!(workflow_engine::save_warnings(&parsed, &catalog).is_empty());
        // 説明文の例の質問のキー（category）は、例のノードの振り分けの質問と同じ。
        assert_eq!(
            node["params"]["route"]["question"], cond["cmp"]["right"],
            "例の条件は振り分けの質問を見る"
        );

        /// 判定の出力（raised_questions だけ）を nodes.triage.output として返す。
        struct Output(Value);
        impl ValueResolver for Output {
            fn resolve_from(&self, from: &FromRef) -> Option<Value> {
                if from.from != "nodes.triage.output" {
                    return None;
                }
                self.0.pointer(from.path.as_deref().unwrap_or("")).cloned()
            }
        }
        let cond: Condition = serde_json::from_value(cond).unwrap();
        for (raised, port) in [
            (json!(["category"]), "true"),
            (json!(["category", "angry"]), "true"),
            (json!(["angry"]), "false"),
            (json!([]), "false"),
        ] {
            let out = Output(json!({ "raised_questions": raised }));
            assert_eq!(
                workflow_engine::branch_port(&cond, &out).unwrap(),
                port,
                "{raised}"
            );
        }
        let g = guide(None);
        assert!(g.contains(&minify(EXAMPLE_RAISED_CONDITION)), "{g}");
        assert!(g.contains("出力 /raised_questions"), "{g}");
    }

    #[test]
    fn the_guide_states_the_port_rules_and_limits() {
        let g = guide(None);
        for needle in [
            "by が value＝選択肢のキー（boolean は true / false）/ uncertain で、out と default は無い",
            "route 無し＝out",
            "out / uncertain / error / default / timeout / true / false",
            "human.approval",
            "calibrated",
            "\"min_confidence\":0.5〜1.0",
            "\"cache\":{\"ttl_sec\":86400}",
            "60〜2592000 秒",
            "label は利用者に見せる選択肢の名前で、答えの label になる",
            "列の選択肢と同じ文字にする",
        ] {
            assert!(g.contains(needle), "{needle} が無い: {g}");
        }
    }
}

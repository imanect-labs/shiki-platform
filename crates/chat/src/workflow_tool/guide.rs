//! emit_workflow の説明文と検証エラーの補足（LLM が読むワークフロー IR の仕様）。
//!
//! LLM はこの説明文の例に従って IR を書く。例が実装とずれると、保存に通らない IR か、保存に通って
//! 実行時に黙って壊れる IR を量産する（BUG-1・BUG-3）。そのため:
//! - 例の IR は定数の JSON にし、`WorkflowIr::from_json` と保存時検証 V1〜V7 を通ることを単体テストで固定する。
//! - ノードの params の形は typed 契約の ts-rs 宣言から、出力の形はノードカタログから生成する（手書きしない）。
//! - 使えるモデルは発話ユーザーのカタログ（保存 API と同じ実装）から run ごとに載せる。

use std::fmt::Write as _;

use serde_json::Value;
use workflow_engine::ir::expr::CmpOp;
use workflow_engine::ir::params::{compact_ts, params_shape};
use workflow_engine::vocab::{event_catalog, node_catalog, EventSource, NodeType};
use workflow_engine::ValidationError;

/// 説明文に載せる使えるモデルの件数の上限（説明文を膨らませない・検証エラーにも候補が出る）。
const MAX_LISTED_MODELS: usize = 30;

/// nodes の要素の形（`ir::node::Node` のキー・テストで型のキーと突き合わせる）。
const NODE_SHAPE: &str = r#"{"id":"<ノード id>","type":"<ノード種>","label":"日本語の短い名前","params":{…},"parent"?:"<map の id>","on_error"?:"fail_run"|"continue","retry"?:{"max_attempts":3},"timeout_sec"?:60}"#;

/// edges の要素の形（`ir::edge::Edge` のキー・テストで型のキーと突き合わせる）。
const EDGE_SHAPE: &str = r#"{"from":"<ノード id>","from_port":"out","to":"<ノード id>"}"#;

/// 例 1: 入力を AI で要約してファイルに保存する（input_schema・$template・$from input / run / nodes・from_port）。
pub(super) const EXAMPLE_SUMMARIZE: &str = r##"{
  "ir_version": 1,
  "name": "summarize-inquiry",
  "display_name": "問い合わせを要約して保存",
  "declared_scopes": ["storage.write"],
  "triggers": [{ "kind": "interactive" }],
  "input_schema": {
    "type": "object",
    "properties": { "title": { "type": "string" }, "body": { "type": "string" } },
    "required": ["title", "body"]
  },
  "nodes": [
    { "id": "summarize", "type": "llm.invoke", "label": "要約する",
      "params": {
        "system": "あなたは社内の問い合わせ窓口の担当者です。",
        "prompt": { "$template": "次の問い合わせを 3 行で要約してください。\n件名: {title}\n本文: {body}",
                    "vars": { "title": { "$from": "input", "path": "/title" },
                              "body": { "$from": "input", "path": "/body" } } } } },
    { "id": "save_summary", "type": "storage.write", "label": "要約を保存",
      "params": {
        "name": { "$template": "summary-{run_id}.md", "vars": { "run_id": { "$from": "run", "path": "/run_id" } } },
        "content": { "$template": "# {title}\n\n{summary}",
                     "vars": { "title": { "$from": "input", "path": "/title" },
                               "summary": { "$from": "nodes.summarize.output", "path": "/text" } } },
        "content_type": "text/markdown" } }
  ],
  "edges": [{ "from": "summarize", "from_port": "out", "to": "save_summary" }]
}"##;

/// 例 2: 毎朝、承認待ちの高額の申請を 1 件ずつ AI で確認する（schedule・data.query の filter・branch・
/// map 領域と each・on_error と error の出口）。
pub(super) const EXAMPLE_REVIEW: &str = r#"{
  "ir_version": 1,
  "name": "review-large-requests",
  "display_name": "高額の申請を AI で確認する",
  "declared_scopes": ["data.read", "storage.write"],
  "triggers": [{ "kind": "schedule", "cron": "0 9 * * 1-5", "tz": "Asia/Tokyo" }],
  "nodes": [
    { "id": "find", "type": "data.query", "label": "承認待ちの高額の申請", "on_error": "continue",
      "params": {
        "table": "<data_schema で調べたテーブルの id>",
        "query": { "filter": { "all": [
                     { "cmp": { "field": "status", "op": "eq", "value": "pending" } },
                     { "cmp": { "field": "amount", "op": "gte", "value": 50000 } } ] },
                   "limit": 50 } } },
    { "id": "has_items", "type": "control.branch", "label": "1 件以上あるか",
      "params": { "condition": { "cmp": { "left": { "$from": "nodes.find.output", "path": "/items" }, "op": "exists" } } } },
    { "id": "per_item", "type": "control.map", "label": "1 件ずつ",
      "params": { "items": { "$from": "nodes.find.output", "path": "/items" }, "on_item_error": "collect" } },
    { "id": "review", "type": "llm.invoke", "parent": "per_item", "label": "確認点を挙げる",
      "params": { "prompt": { "$template": "次の申請で確認すべき点を箇条書きで挙げてください。\n{record}",
                              "vars": { "record": { "$from": "each", "path": "/item/data" } } } } },
    { "id": "save_review", "type": "storage.write", "parent": "per_item", "label": "確認メモを保存",
      "params": { "name": { "$template": "review-{id}.md", "vars": { "id": { "$from": "each", "path": "/item/id" } } },
                  "content": { "$from": "nodes.review.output", "path": "/text" } } },
    { "id": "record_failure", "type": "storage.write", "label": "失敗を記録",
      "params": { "name": "review-failed.txt",
                  "content": { "$from": "nodes.find.output", "path": "/error/message" } } }
  ],
  "edges": [
    { "from": "find", "to": "has_items" },
    { "from": "find", "from_port": "error", "to": "record_failure" },
    { "from": "has_items", "from_port": "true", "to": "per_item" },
    { "from": "review", "to": "save_review" }
  ]
}"#;

/// 条件木の例（all / any / not / cmp・control.branch の condition にそのまま置ける）。
pub(super) const EXAMPLE_CONDITION: &str = r#"{ "all": [
  { "cmp": { "left": { "$from": "input", "path": "/amount" }, "op": "gte", "right": 50000 } },
  { "not": { "cmp": { "left": { "$from": "input", "path": "/category" }, "op": "in", "right": ["交通費", "通信費"] } } },
  { "any": [ { "cmp": { "left": { "$from": "input", "path": "/note" }, "op": "exists" } },
             { "cmp": { "left": { "$from": "input", "path": "/urgent" }, "op": "eq", "right": true } } ] }
] }"#;

/// JSON の文字列の外の空白を詰める（キーの順を保ったまま 1 行にする）。
fn minify(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    let (mut in_str, mut escaped) = (false, false);
    for c in json.chars() {
        if in_str {
            out.push(c);
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_str = false,
                _ => {}
            }
        } else if c == '"' {
            in_str = true;
            out.push(c);
        } else if !c.is_whitespace() {
            out.push(c);
        }
    }
    out
}

/// 説明文に載せる例の IR（1 行の JSON）。llm.invoke の model は書かない（省略が正しい書き方で、
/// 組織の既定のモデルで動く・BUG-12）。
pub(super) fn example_json(raw: &str) -> String {
    minify(raw)
}

/// llm.invoke のモデルの案内（`None` はモデルの一覧を引けなかった）。
///
/// model はふつう省略させる（組織の既定で動き、管理者が既定を変えれば追随する）。指定できるモデルが
/// 無い構成（LLM 未構成・組織の許可が 0 件）では書かせない（書くと検証で拒否される・BUG-37）。
fn models_section(models: Option<&[String]>) -> String {
    const OMIT: &str = "model はふつう省略する（省略すると組織の既定のモデルで動く）。";
    match models {
        Some([]) => {
            "model は書かない（省略すると組織の既定のモデルで動く。この組織で指定できるモデルは無い）。"
                .to_string()
        }
        Some(ids) => {
            let shown = ids
                .iter()
                .take(MAX_LISTED_MODELS)
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            let rest = ids.len().saturating_sub(MAX_LISTED_MODELS);
            let more = if rest > 0 {
                format!("（ほか {rest} 件）")
            } else {
                String::new()
            };
            format!("{OMIT}指定するときは次の id のどれかを書く（このユーザーの組織で使えるもの）: {shown}{more}。")
        }
        None => format!(
            "{OMIT}指定すると組織で使えるモデルと照合され、使えなければ検証エラーに使えるモデルが出る。"
        ),
    }
}

/// ノード語彙の要約（カタログ・params 契約・出力の形の単一定義から生成する）。
fn catalog_summary() -> String {
    let mut out = String::from(
        "【ノード】type（名前）: 説明。params は TS の表記（? は省略可・ValueExpr は上の値）。出力は後続が nodes.<id>.output の path で読む形。\n",
    );
    for e in node_catalog().into_iter().filter(|e| e.available) {
        let _ = write!(
            out,
            "- {}（{}）: {}。",
            e.node_type.as_str(),
            e.label_ja,
            e.description_ja
        );
        if let Some(shape) = params_shape(e.node_type) {
            let _ = write!(out, "params {}。", shape.render());
        }
        let _ = write!(out, "出力 {}。", e.output_shape);
        if !e.output_ports.is_empty() && e.output_ports != ["next"] && e.output_ports != ["out"] {
            let _ = write!(out, "出口 {}。", e.output_ports.join("/"));
        }
        if let Some(scope) = e.required_scope {
            let _ = write!(out, "スコープ {}。", scope.as_str());
        }
        out.push('\n');
    }
    out
}

/// トリガ・待機で使えるイベント source（閉集合のうち今のステージで有効なもの）。
fn event_sources() -> String {
    EventSource::ALL
        .iter()
        .filter(|s| s.available_stage_a())
        .map(|s| format!("\"{}\"", s.as_str()))
        .collect::<Vec<_>>()
        .join(" | ")
}

/// きっかけの内容（`$from trigger` と wait(event) の出力）の項目（イベントのカタログの単一定義・D-47）。
///
/// 同じ項目を持つ source はまとめて 1 行にする（データ変更の 3 種は同じ形）。
fn trigger_payloads() -> String {
    let mut out = String::from(
        "【きっかけの内容】event トリガの run では {\"$from\":\"trigger\",\"path\":\"/<項目>\"} で読む（control.wait の event は出力を nodes.<id>.output の path で読む）。\n",
    );
    let mut groups: Vec<(Vec<&str>, &[workflow_engine::vocab::EventPayloadField])> = Vec::new();
    for e in event_catalog().into_iter().filter(|e| e.available) {
        match groups
            .iter_mut()
            .find(|(_, f)| std::ptr::eq(*f, e.payload_fields))
        {
            Some((sources, _)) => sources.push(e.source.as_str()),
            None => groups.push((vec![e.source.as_str()], e.payload_fields)),
        }
    }
    for (sources, fields) in groups {
        let body = fields
            .iter()
            .map(|f| format!("{}: {}（{}）", f.name, f.ty, f.description_ja))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(out, "- {}: {{ {body} }}", sources.join(" / "));
    }
    out
}

/// emit_workflow の説明文（`models` は発話ユーザーの組織で使えるモデル）。
pub(super) fn emit_description(models: Option<&[String]>) -> String {
    let mut d = String::from(
        "ワークフロー（自動化フロー）を JSON IR として保存する。新規作成は ir だけを渡し、既存の編集は workflow_id と expected_version も渡す（編集の前に必ず read_workflow で今の IR を読む）。保存しただけでは実行されない。検証に失敗するとエラーが全件返るので、全部直して再実行する。成功するとユーザーにカードが表示され、エディタで開ける。\n",
    );
    let _ = write!(
        d,
        "【骨格】{{\"ir_version\":1,\"name\":\"英小文字・数字・ハイフンの安定名（^[a-z][a-z0-9-]{{0,63}}$）\",\"display_name\":\"日本語の表示名\",\"declared_scopes\":[…]（省略可。ノードが要するスコープは保存時に補われる）,\"triggers\":[…],\"input_schema\":{{…}}（interactive の入力・省略可）,\"nodes\":[…],\"edges\":[…]}}\n\
         - triggers: {{\"kind\":\"interactive\"}}（ボタン・チャットから実行）| {{\"kind\":\"schedule\",\"cron\":\"0 9 * * 1-5\",\"tz\":\"Asia/Tokyo\"}} | {{\"kind\":\"event\",\"source\":\"storage.write\",\"scope\":{{\"folder\":\"<フォルダの uuid>\"}}}}（そのフォルダ（配下を含む）に新しいファイルが保存されたときに起動。既存のファイルの上書き保存での起動は当てにしない）| {{\"kind\":\"event\",\"source\":\"data.record.created\" か \"data.record.updated\" か \"data.transition\",\"scope\":{{\"table\":\"<テーブルの uuid>\"}},\"filter\"?:条件}}（テーブルの行が追加・更新・状態変更されたときに起動。有効化した人に見える行・列の変更だけで起き、一括取込とこのワークフロー自身の書込では起きない。状態は filter の /to で絞る）。EventSource = {sources}。control.wait（kind: event）の scope も同じ形が必須（データ変更は \"record\":値式 で行を絞れる・filter は {{\"$from\":\"event\"}} で届いたできごとを読む）。\n\
         - input_schema: JSON Schema のサブセット（type・properties・required・items・enum など。$ref・oneOf は使えない）。入力は {{\"$from\":\"input\",\"path\":\"/キー\"}} で読む。\n\
         - nodes の要素（? は省略可）: {node}。id は ^[a-z][a-z0-9_]{{0,63}}$（英小文字で始め、英小文字・数字・_ だけ。例: find_requests）。\n\
         - edges の要素: {edge}。キーはこの 3 つだけ（port というキーは無い）。from_port を省くと out。出口: 通常のノードは out、control.branch は true / false、control.switch は cases の port と default、on_error が \"continue\" のノードは error、control.wait（kind: event・on_timeout: \"continue\"）は timeout。無い出口につないだエッジは保存できない。\n\
         - 入るエッジは 1 ノード 1 本まで（複数を受けられるのは control.join だけ）。\n\
         - on_error が \"continue\" のノードが失敗すると、出力は {{\"error\":{{\"code\",\"message\",…}}}} になり error の出口へ進む（error につないだ先が無ければ run は失敗する）。\n\
         - map 領域: control.map は items の要素ごとに、\"parent\":\"<map の id>\" を持つノードを繰り返す。領域の中は parent が同じノードどうしだけをつなぐ（入口は入るエッジなし・出口はちょうど 1 つ・map ノードから領域へのエッジは書かない）。領域の中では {{\"$from\":\"each\",\"path\":\"/item\"}}（今の要素）と /index（番号）が使える。\n",
        sources = event_sources(),
        node = NODE_SHAPE,
        edge = EDGE_SHAPE,
    );
    d.push_str(&trigger_payloads());
    let _ = write!(
        d,
        "【値の書き方】params の値（ValueExpr）は次の 3 つ。\n\
         - リテラル: 文字列・数値・真偽・null・object・array。object / array の中にも下の 2 つを置ける。\n\
         - 参照 {{\"$from\":\"<source>\",\"path\":\"/キー/0\",\"default\":既定値}}（path・default は省略可）。source は input（実行の入力）・trigger（トリガの内容）・run（{{run_id, tenant_id}}）・nodes.<id>.output（エッジでたどれる前のノードの出力。形は【ノード】の出力）・each（map 領域の中だけ）のどれか。中身は path（JSON Pointer・/ で始める）で指し、source に input.title のような書き方はしない。解決できず default も無ければ step は失敗する。\n\
         - 文章の組み立て {{\"$template\":\"件名: {{title}}（{{count}} 件）\",\"vars\":{{\"title\":{{\"$from\":\"input\",\"path\":\"/title\"}},\"count\":…}}}}。差し込みは {{名前}} で、名前は vars のキー（vars に無い名前はエラー）。{{{{ と }}}} は差し込みではなく波括弧そのものを出す書き方（{{{{title}}}} と書くと「{{title}}」という文字が出る）。\n\
         - url・secret・skill・model など型が string の項目はリテラルの文字列だけ（参照は書けない）。テーブル・フォルダ・ファイルの id は実在する uuid を書く（data_schema・doc_search などで調べる。推測しない）。\n\
         - 条件（control.branch の condition・トリガと control.wait の filter）: {{\"cmp\":{{\"left\":値,\"op\":CmpOp,\"right\":値}}}}・{{\"all\":[条件…]}}・{{\"any\":[条件…]}}・{{\"not\":条件}}。CmpOp = {ops}（exists・is_null は right を書かない）。例: {cond}\n\
         - data.query の filter は形が違う: {{\"cmp\":{{\"field\":\"<列の参照名>\",\"op\":CmpOp,\"value\":値}}}}（all / any / not は同じ）。\n",
        ops = compact_ts::<CmpOp>(),
        cond = minify(EXAMPLE_CONDITION),
    );
    let _ = write!(
        d,
        "【例】そのまま保存できる IR。\n\
         例 1（入力を AI で要約してファイルに保存する）: {ex1}\n\
         例 2（毎朝、承認待ちの高額の申請を 1 件ずつ AI で確認し、取得に失敗したら記録する）: {ex2}\n\
         【llm.invoke のモデル】{models}\n",
        ex1 = example_json(EXAMPLE_SUMMARIZE),
        ex2 = example_json(EXAMPLE_REVIEW),
        models = models_section(models),
    );
    d.push_str(&catalog_summary());
    d
}

/// 検証エラーの補足（LLM が自分で直すための材料）。
///
/// - params の誤り（`ir.schema_violation` でノードに紐づくもの）: そのノード種の params の形。
/// - IR 全体の形の誤り（ノードに紐づかない `ir.schema_violation`）: ノードとエッジの形。
pub(super) fn validation_hints(errors: &[ValidationError], ir: &Value) -> String {
    let node_type_of = |id: &str| -> Option<NodeType> {
        ir.get("nodes")?
            .as_array()?
            .iter()
            .find(|n| n.get("id").and_then(Value::as_str) == Some(id))?
            .get("type")?
            .as_str()
            .and_then(NodeType::parse)
    };
    let mut types: Vec<NodeType> = Vec::new();
    let mut whole_ir = false;
    for e in errors.iter().filter(|e| e.code == "ir.schema_violation") {
        match e.node_id.as_deref() {
            Some(id) => {
                if let Some(nt) = node_type_of(id) {
                    if !types.contains(&nt) {
                        types.push(nt);
                    }
                }
            }
            None => whole_ir = true,
        }
    }
    let mut out = String::new();
    if whole_ir {
        let _ = writeln!(
            out,
            "IR の形: nodes の要素 = {NODE_SHAPE}（? は省略可）、edges の要素 = {EDGE_SHAPE}（それ以外のキーは書けない）。"
        );
    }
    for nt in types {
        if let Some(shape) = params_shape(nt) {
            let _ = writeln!(
                out,
                "{} の params の形（? は省略可）: {}",
                nt.as_str(),
                shape.render()
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use workflow_engine::ir::edge::Edge;
    use workflow_engine::ir::node::Node;
    use workflow_engine::{validate, Catalog, WorkflowIr};

    fn examples() -> [(&'static str, &'static str); 2] {
        [("summarize", EXAMPLE_SUMMARIZE), ("review", EXAMPLE_REVIEW)]
    }

    /// 説明文に埋め込む形の例を IR として読む。
    fn embedded(raw: &str) -> Value {
        serde_json::from_str(&example_json(raw)).unwrap()
    }

    /// 説明文の例は、照合しない構成でも、組織のモデルで照合する構成でも、そのまま V1〜V7 を通る
    /// （model を書かないので、組織の既定で動く）。
    #[test]
    fn examples_pass_validation_with_and_without_a_model_catalog() {
        let with_models = Catalog {
            models: Some(vec!["model-a".into(), "model-b".into()]),
            ..Catalog::default()
        };
        let locked = Catalog {
            model_lock: Some("auto".into()),
            ..Catalog {
                models: Some(Vec::new()),
                ..Catalog::default()
            }
        };
        for (name, raw) in examples() {
            let ir_json = embedded(raw);
            let ir = WorkflowIr::from_json(&ir_json).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(
                workflow_engine::save_warnings(&ir).is_empty(),
                "{name}: 警告が出る例は載せない"
            );
            for (label, catalog) in [
                ("カタログなし", &Catalog::default()),
                ("カタログあり", &with_models),
                ("選択の禁止・許可 0 件", &locked),
            ] {
                validate(&ir_json, catalog).unwrap_or_else(|e| panic!("{name}（{label}）: {e:?}"));
            }
            // 1 行にしても元の JSON と同じ値（文字列の中の空白・改行は残る）。
            let original: Value = serde_json::from_str(raw).unwrap();
            assert_eq!(ir_json, original, "{name}");
            assert!(
                ir.nodes.iter().all(|n| n.params.get("model").is_none()),
                "{name}: 例に model を書かない"
            );
        }
    }

    /// 条件木の例は control.branch の condition としてそのまま保存できる。
    #[test]
    fn condition_example_is_a_valid_branch_condition() {
        let cond: Value = serde_json::from_str(&minify(EXAMPLE_CONDITION)).unwrap();
        let ir = serde_json::json!({
            "ir_version": 1, "name": "wf", "triggers": [{ "kind": "interactive" }],
            "nodes": [{ "id": "check", "type": "control.branch", "params": { "condition": cond } }],
            "edges": []
        });
        validate(&ir, &Catalog::default()).unwrap();
        for key in ["\"all\"", "\"any\"", "\"not\"", "\"cmp\""] {
            assert!(EXAMPLE_CONDITION.contains(key), "{key} の例が無い");
        }
    }

    /// 例は実装と食い違う書き方を含まない（BUG-1・BUG-3 の再発防止）。
    #[test]
    fn examples_use_the_implemented_notation() {
        // ノード id の形式（workflow_engine::ir::node::NODE_ID_RE = ^[a-z][a-z0-9_]{0,63}$）。
        let is_node_id = |id: &str| {
            let mut chars = id.chars();
            chars.next().is_some_and(|c| c.is_ascii_lowercase())
                && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                && id.len() <= 64
        };
        for (name, raw) in examples() {
            let ir = embedded(raw);
            // 差し込みは {name}。{{ はエスケープなので例に書かない。
            assert!(
                !raw.contains("{{") && !raw.contains("}}"),
                "{name}: {{{{ を含む"
            );
            for edge in ir["edges"].as_array().unwrap() {
                let keys: Vec<&str> = edge
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect();
                assert!(!keys.contains(&"port"), "{name}: port キー: {edge}");
                assert!(
                    keys.iter().all(|k| ["from", "from_port", "to"].contains(k)),
                    "{name}: {edge}"
                );
            }
            for node in ir["nodes"].as_array().unwrap() {
                let id = node["id"].as_str().unwrap();
                assert!(is_node_id(id), "{name}: ノード id {id} が形式に合わない");
            }
        }
        // 2 つの例で、出口・領域・参照の書き方をひととおり見せる。
        let all = format!(
            "{}{}",
            example_json(EXAMPLE_SUMMARIZE),
            example_json(EXAMPLE_REVIEW)
        );
        for needle in [
            "\"from_port\":\"out\"",
            "\"from_port\":\"true\"",
            "\"from_port\":\"error\"",
            "\"on_error\":\"continue\"",
            "\"parent\":\"per_item\"",
            "{\"$from\":\"each\",\"path\":\"/item/id\"}",
            "{\"$from\":\"input\",\"path\":\"/title\"}",
            "{\"$from\":\"run\",\"path\":\"/run_id\"}",
            "{\"$from\":\"nodes.summarize.output\",\"path\":\"/text\"}",
            "\"input_schema\"",
            "{\"field\":\"status\",\"op\":\"eq\",\"value\":\"pending\"}",
        ] {
            assert!(all.contains(needle), "例に {needle} が無い");
        }
    }

    /// TS 宣言 `{ a: T, b?: U }` の最上位のキー（テスト用の簡易な読み取り）。
    fn ts_keys(ts: &str) -> Vec<String> {
        ts.trim_start_matches("{ ")
            .trim_end_matches(" }")
            .split(", ")
            .filter_map(|f| f.split(':').next())
            .map(|k| k.trim().trim_end_matches('?').to_string())
            .collect()
    }

    /// 骨格のノードとエッジの形は IR の型と同じキーを持つ（型にキーを足したらここで気づく）。
    #[test]
    fn envelope_shapes_match_the_ir_types() {
        for (shape, ts) in [
            (NODE_SHAPE, compact_ts::<Node>()),
            (EDGE_SHAPE, compact_ts::<Edge>()),
        ] {
            let keys = ts_keys(&ts);
            assert!(keys.len() >= 3, "{ts}");
            for key in &keys {
                assert!(
                    shape.contains(&format!("\"{key}\"")),
                    "{key} が {shape} に無い"
                );
            }
        }
        // 「?」を付けた項目は省略できる。
        let node: Node = serde_json::from_value(
            serde_json::json!({ "id": "a", "type": "x", "label": "l", "params": {} }),
        )
        .unwrap();
        assert!(node.parent.is_none());
        let edge: Edge =
            serde_json::from_value(serde_json::json!({ "from": "a", "to": "b" })).unwrap();
        assert_eq!(edge.from_port, "out");
    }

    #[test]
    fn description_explains_the_notation_the_engine_implements() {
        let d = emit_description(None);
        assert!(d.contains("差し込みは {名前}"), "{d}");
        assert!(
            d.contains("{{ と }} は差し込みではなく波括弧そのもの"),
            "{d}"
        );
        assert!(d.contains("port というキーは無い"));
        assert!(d.contains("nodes.<id>.output"));
        for source in ["input", "trigger", "run", "each"] {
            assert!(
                d.contains(&format!("{source}（")),
                "source {source} の説明が無い"
            );
        }
        // 演算子の一覧とイベント source は型・語彙から生成する。
        assert!(d.contains("\"starts_with\""), "{d}");
        assert!(d.contains("EventSource = \"storage.write\""), "{d}");
        // きっかけの内容の項目もイベントのカタログから生成する（D-47・BUG-36）。データ変更の 3 種は同じ形。
        assert!(
            d.contains("- storage.write: { file_id: string（保存されたファイルの ID）"),
            "{d}"
        );
        assert!(
            d.contains(
                "- data.record.created / data.record.updated / data.transition: { source: string"
            ),
            "{d}"
        );
        assert!(
            d.contains("\"scope\":{\"table\":\"<テーブルの uuid>\"}"),
            "{d}"
        );
        // 旧説明文の誤り（mustache の差し込み・port: next・<祖先ノードid>）を含まない。
        assert!(!d.contains("{{変数}}") && !d.contains("\"port\":\"next\""));
        assert!(!d.contains("<祖先ノードid>"));
    }

    /// 各ノードの params の形と出力の形が説明文に載る（ts-rs の宣言・カタログから生成）。
    #[test]
    fn description_lists_params_and_outputs_of_available_nodes() {
        let d = emit_description(None);
        assert!(
            d.contains("- llm.invoke（AI に聞く）")
                && d.contains("params { model?: string, prompt: ValueExpr"),
            "{d}"
        );
        assert!(d.contains("出力 { text: string, model: string }"));
        assert!(d.contains("SwitchCase = { port: string, equals: unknown }"));
        for e in node_catalog() {
            let line = format!("- {}（", e.node_type.as_str());
            assert_eq!(d.contains(&line), e.available, "{}", e.node_type.as_str());
        }
    }

    #[test]
    fn description_lists_the_models_of_the_user() {
        let ids: Vec<String> = vec!["fast-model".into(), "smart-model".into()];
        let d = emit_description(Some(&ids));
        assert!(d.contains(
            "次の id のどれかを書く（このユーザーの組織で使えるもの）: fast-model, smart-model。"
        ));
        let many: Vec<String> = (0..35).map(|i| format!("m{i}")).collect();
        let d = emit_description(Some(&many));
        assert!(d.contains("m29（ほか 5 件）"), "件数の上限");
        assert!(!d.contains("m30"));
        let d = emit_description(Some(&[]));
        assert!(d.contains("model は書かない"), "指定できるモデルが無い構成");
    }

    /// model は省略が正しい書き方（組織の既定で動く）。必須・デプロイの既定とは書かず、例にも model を書かない
    /// （BUG-12）。
    #[test]
    fn description_says_an_omitted_model_follows_the_organization_default() {
        let ids: Vec<String> = vec!["fast-model".into()];
        for models in [None, Some(&ids[..]), Some(&[][..])] {
            let d = emit_description(models);
            assert!(
                d.contains("省略すると組織の既定のモデルで動く"),
                "{models:?}"
            );
            assert!(!d.contains("model は必須"), "{models:?}");
            assert!(!d.contains("デプロイの既定"), "{models:?}");
            assert!(
                !d.contains("\"model\":"),
                "{models:?}: 例に model を書かない"
            );
        }
    }

    #[test]
    fn hints_add_the_params_shape_of_the_failing_node_type() {
        let ir = embedded(EXAMPLE_SUMMARIZE);
        let errors = vec![
            ValidationError::new(
                "ir.schema_violation",
                "params が不正です: 項目 'prompt' がありません",
            )
            .at_node("summarize")
            .at_path("/params"),
            ValidationError::new("ir.bad_ref", "x").at_node("save_summary"),
        ];
        let h = validation_hints(&errors, &ir);
        assert!(
            h.contains(
                "llm.invoke の params の形（? は省略可）: { model?: string, prompt: ValueExpr"
            ),
            "{h}"
        );
        assert!(
            !h.contains("storage.write"),
            "schema_violation 以外は補足しない: {h}"
        );
        let whole = vec![ValidationError::new(
            "ir.schema_violation",
            "unknown field `port`",
        )];
        let h = validation_hints(&whole, &ir);
        assert!(h.contains(&format!("edges の要素 = {EDGE_SHAPE}")), "{h}");
    }
}

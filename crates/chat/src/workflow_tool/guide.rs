//! emit_workflow の説明文と検証エラーの補足（LLM が読むワークフロー IR の仕様）。
//!
//! LLM はこの説明文の例に従って IR を書く。例が実装とずれると、保存に通らない IR か、保存に通って
//! 実行時に黙って壊れる IR を量産する（BUG-1・BUG-3）。そのため:
//! - 例の IR は定数の JSON にし、`WorkflowIr::from_json` と保存時検証 V1〜V7 を通ることを単体テストで固定する。
//! - ノードの params の形は typed 契約の ts-rs 宣言から、出力の形はノードカタログから生成する（手書きしない）。
//! - 使えるモデルは発話ユーザーのカタログ（保存 API と同じ実装）から run ごとに載せる。

mod decide;
/// 検証エラーの補足（guide.rs の行数上限のため分割）。
mod hints;

pub(super) use hints::validation_hints;

use std::fmt::Write as _;

use workflow_engine::ir::expr::CmpOp;
use workflow_engine::ir::params::{compact_ts, params_shape};
use workflow_engine::vocab::{agent_tool_catalog, event_catalog, node_catalog, EventSource};

/// 説明文に載せる使えるモデルの件数の上限（説明文を膨らませない・検証エラーにも候補が出る）。
pub(super) const MAX_LISTED_MODELS: usize = 30;

/// nodes の要素の形（`ir::node::Node` のキー・テストで型のキーと突き合わせる）。
const NODE_SHAPE: &str = r#"{"id":"<ノード id>","type":"<ノード種>","label":"日本語の短い名前","params":{…},"parent"?:"<map の id>","on_error"?:"fail_run"|"continue","retry"?:{"max_attempts":3},"timeout_sec"?:60,"cache"?:{"ttl_sec":86400}}"#;

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
    "required": ["title", "body"],
    "x-order": ["title", "body"]
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
      "params": { "folder": "<保存先のフォルダの id>",
                  "name": { "$template": "review-{id}.md", "vars": { "id": { "$from": "each", "path": "/item/id" } } },
                  "content": { "$from": "nodes.review.output", "path": "/text" } } },
    { "id": "record_failure", "type": "storage.write", "label": "失敗を記録",
      "params": { "folder": "<保存先のフォルダの id>", "name": "review-failed.txt",
                  "content": { "$from": "nodes.find.output", "path": "/error/message" } } }
  ],
  "edges": [
    { "from": "find", "to": "has_items" },
    { "from": "find", "from_port": "error", "to": "record_failure" },
    { "from": "has_items", "from_port": "true", "to": "per_item" },
    { "from": "review", "to": "save_review" }
  ]
}"#;

/// 出力の形（llm.invoke の output_schema・GAP-13）の例のノード（問い合わせを要約してから、種類を enum で決める）。
/// x-order で要約を先に書かせる（項目の名前の辞書順では category が先になる・BUG-123・BUG-139）。
pub(super) const EXAMPLE_STRUCTURED_NODE: &str = r#"{ "id": "classify", "type": "llm.invoke", "label": "種類を判定",
  "params": {
    "prompt": { "$template": "次の問い合わせを 1 行で要約し、種類を決めてください。\n{body}",
                "vars": { "body": { "$from": "input", "path": "/body" } } },
    "output_schema": { "type": "object", "required": ["summary", "category"],
      "x-order": ["summary", "category"],
      "properties": {
        "category": { "type": "string", "enum": ["billing", "bug", "howto", "other"] },
        "summary": { "type": "string", "maxLength": 100 } } } } }"#;

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

/// 実行情報（`$from run`）の項目の説明（vocab の単一定義から作る・D-97・D-101）。
fn run_info_fields() -> String {
    workflow_engine::vocab::RUN_INFO_FIELDS
        .iter()
        .map(|f| {
            let mut about = f.description_ja.to_string();
            if !f.example.is_empty() {
                let _ = write!(about, "・例 {}", f.example);
            }
            if !f.note_ja.is_empty() {
                let _ = write!(about, "・{}", f.note_ja);
            }
            format!("{}: {}（{about}）", f.name, f.ty)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Shiki が決めるので作成者が書けないヘッダー（`nodes::http_headers::RESERVED` から・表を写さない・GAP-19）。
fn reserved_headers() -> String {
    workflow_engine::nodes::http_headers::RESERVED
        .iter()
        .map(|h| {
            // 小文字の表を、ふだんの書き方（Content-Length・TE）にして見せる。
            h.split('-')
                .map(|w| {
                    if w.len() <= 2 {
                        w.to_ascii_uppercase()
                    } else {
                        let mut c = w.chars();
                        c.next()
                            .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
                            .unwrap_or_default()
                    }
                })
                .collect::<Vec<_>>()
                .join("-")
        })
        .collect::<Vec<_>>()
        .join("・")
}

/// emit_workflow の説明文（`models` は発話ユーザーの組織で使える llm.invoke のモデル・`decision_models` は ai.decide の
/// 判定用のモデル。どちらも `None` は載せない）。
pub(super) fn emit_description(
    models: Option<&[String]>,
    decision_models: Option<&[super::DecisionModel]>,
) -> String {
    let mut d = String::from(
        "ワークフロー（自動化フロー）を JSON IR として保存する。新規作成は ir だけを渡し、既存の編集は workflow_id と、read_workflow が返した version を expected_version に必ず渡す（無いと保存しない。編集の前に必ず read_workflow で今の IR を読む。id が分からなければ list_workflows で探す）。保存しただけでは実行されない。検証に失敗するとエラーが全件返るので、全部直して再実行する。成功するとユーザーにカードが表示され、エディタで開ける。\n",
    );
    let _ = write!(
        d,
        "【骨格】{{\"ir_version\":1,\"name\":\"英小文字・数字・ハイフンの安定名（^[a-z][a-z0-9-]{{0,63}}$）\",\"display_name\":\"日本語の表示名\",\"declared_scopes\":[…]（省略可。ノードが要するスコープは保存時に補われる）,\"triggers\":[…],\"input_schema\":{{…}}（interactive の入力・省略可）,\"nodes\":[…],\"edges\":[…]}}\n\
         - triggers: {{\"kind\":\"interactive\"}}（ボタン・チャットから実行）| {{\"kind\":\"schedule\",\"cron\":\"0 9 * * 1-5\",\"tz\":\"Asia/Tokyo\"}}（分 時 日 月 曜日。この例は平日の 9 時。曜日は 0 と 7 が日曜、1 が月曜〜6 が土曜で、MON〜SUN の名前でも書ける）| {{\"kind\":\"event\",\"source\":\"storage.write\",\"scope\":{{\"folder\":\"<フォルダの uuid>\"}}}}（そのフォルダ（配下を含む）にファイルが保存されたときに起動。新しいファイル（op \"create\"）と既存のファイルの新しい版（op \"update\"・上書き・Office やノートの保存）の両方で起き、filter の /op で絞れる。新しい版は同じファイルの保存が {coalesce} 秒途切れてから最後の版で 1 回だけ起動する。このワークフロー自身の保存（workflow.start の子を含む）では起きない）| {{\"kind\":\"event\",\"source\":\"data.record.created\" か \"data.record.updated\" か \"data.transition\",\"scope\":{{\"table\":\"<テーブルの uuid>\"}},\"filter\"?:条件}}（テーブルの行が追加・更新・状態変更されたときに起動。有効化した人に見える行・列の変更だけで起き、一括取込とこのワークフロー自身の書込では起きない。状態は filter の /to で絞る）。EventSource = {sources}。control.wait（kind: event）の scope も同じ形が必須（データ変更は \"record\":値式 で行を絞れる・filter は {{\"$from\":\"event\"}} で届いたできごとを読む）。\n\
         - input_schema: JSON Schema のサブセット（type・properties・required・items・enum など。$ref・oneOf は使えない）。入力は {{\"$from\":\"input\",\"path\":\"/キー\"}} で読む。任意の注記 \"x-order\":[\"title\",\"body\"] は、画面（エディタの入力項目・実行の入力欄・ミニアプリの部品の候補）に並べる項目の順（properties のキーだけを重ねずに書く。省くと properties のキーは保存で辞書順に並び替わる）。\n\
         - nodes の要素（? は省略可）: {node}。id は ^[a-z][a-z0-9_]{{0,63}}$（英小文字で始め、英小文字・数字・_ だけ。例: find_requests）。cache は ai.decide だけに書ける（【AI で判定】）。\n\
         - edges の要素: {edge}。キーはこの 3 つだけ（port というキーは無い）。from_port を省くと out。出口: 通常のノードは out、control.branch は true / false、control.switch は cases の port と default、on_error が \"continue\" のノードは error、control.wait（kind: event・on_timeout: \"continue\"）は timeout、ai.decide は route で決まる（【AI で判定】）。無い出口につないだエッジは保存できない。\n\
         - 入るエッジは 1 ノード 1 本まで（複数を受けられるのは control.join だけ）。\n\
         - on_error が \"continue\" のノードが失敗すると、出力は {{\"error\":{{\"code\",\"message\",…}}}} になり error の出口へ進む（error につないだ先が無ければ run は失敗する）。\n\
         - map 領域: control.map は items の要素ごとに、\"parent\":\"<map の id>\" を持つノードを繰り返す。領域の中は parent が同じノードどうしだけをつなぐ（入口は入るエッジなし・出口はちょうど 1 つ・map ノードから領域へのエッジは書かない）。領域の中では {{\"$from\":\"each\",\"path\":\"/item\"}}（今の要素）と /index（番号）が使える。領域の中のノードは、map ノードの前のノード（エッジでたどれるもの。入れ子なら外側の map の前も）の出力も nodes.<id>.output で読める（承認者の表を先に引いて、1 件ずつ書く、など）。map ノード自身と map の後ろのノードは読めない。\n",
        sources = event_sources(),
        coalesce = workflow_engine::scheduler::COALESCE_QUIET_SECS,
        node = NODE_SHAPE,
        edge = EDGE_SHAPE,
    );
    d.push_str(&trigger_payloads());
    let _ = write!(
        d,
        "【値の書き方】params の値（ValueExpr）は次の 3 つ。\n\
         - リテラル: 文字列・数値・真偽・null・object・array。object / array の中にも下の 2 つを置ける。\n\
         - 参照 {{\"$from\":\"<source>\",\"path\":\"/キー/0\",\"default\":既定値}}（path・default は省略可）。source は input（実行の入力）・trigger（トリガの内容）・run（実行情報 {{ {run} }}。日時は日本時間で、同じ実行の中ではやり直しても同じ値）・nodes.<id>.output（エッジでたどれる前のノードの出力。形は【ノード】の出力）・each（map 領域の中だけ）のどれか。中身は path（JSON Pointer・/ で始める）で指し、source に input.title のような書き方はしない。解決できず default も無ければ step は失敗する。\n\
         - 文章の組み立て {{\"$template\":\"件名: {{title}}（{{count}} 件）\",\"vars\":{{\"title\":{{\"$from\":\"input\",\"path\":\"/title\"}},\"count\":…}}}}。差し込みは {{名前}} で、名前は vars のキー（vars に無い名前はエラー）。{{{{ と }}}} は差し込みではなく波括弧そのものを出す書き方（{{{{title}}}} と書くと「{{title}}」という文字が出る）。\n\
         - url・secret・skill・model など型が string の項目はリテラルの文字列だけ（参照は書けない）。テーブル・フォルダ・ファイルの id は実在する uuid を書く（data_schema・doc_search などで調べる。推測しない）。\n\
         - schedule・event で動かすワークフローの storage.write・csv.write には folder を必ず書く（省略はルートへの保存で、自動の実行はルートに書けないので有効化できない）。storage.write・csv.write は既存のファイルを上書きしない。同じ名前のファイルがあると失敗するので、毎回動くワークフローは on_name_taken に \"add_number\" を書く（「名前 (2).md」のように番号を付けて保存する）か、名前に {{\"$from\":\"run\",\"path\":\"/date\"}} を差し込む。content_type を省くとファイル名の拡張子（.md・.txt・.csv・.json・.html）から決まる。CSV（csv.write と、種類が text/csv の storage.write）は encoding に \"utf8_bom\" を書く（日本語の Windows の Excel で文字化けしない。省略は BOM なしの UTF-8。Shift_JIS だけを読む仕組みへ渡すなら \"shift_jis\"）。CSV の content には、CSV の文字のほか、csv.query の結果（{{\"$from\":\"nodes.<id>.output\"}}・{{columns, rows}} の表）や、項目のまとまりのリスト（列は項目の名前を合わせたもので名前の順）をそのまま渡せる（CSV に組み立てる。数・入れ子の値・csv.query が打ち切った表〔truncated〕は失敗する）。\n\
         - csv.query は CSV と Excel（.xlsx。左端のシートを読み、いちばん上の行が列の名前。数式は計算せず保存された値を読む）を集計する。Excel を読めるのは csv.query だけ（csv.patch・csv.write は CSV だけ）。結果の rows は値をすべて文字で返す。数・真偽を種類のまま使う（control.branch で大小を比べる・合計を出す・列の名前がテーブルの列の参照名と同じなら data.bulk_upsert の rows に渡す）ときは items（1 行ずつ {{列の名前: 値}}。{{\"$from\":\"nodes.<id>.output\",\"path\":\"/items\"}}、1 件目の列は /items/0/<列の名前>）を使う。csv.write の content には結果の全体（{{columns, rows}} の表）を渡す。\n\
         - storage.list は名前の順に {list_max} 件までを返し、続きがあると truncated が true で next_cursor に続きの位置が入る。続きは、同じ folder の storage.list の cursor に {{\"$from\":\"nodes.<id>.output\",\"path\":\"/next_cursor\"}} を書いて読む（別のフォルダーの続きの位置は失敗する。省略・null は先頭から）。\n\
         - llm.invoke の prompt・agent.invoke の instruction・rag.search の query・csv.query の sql は空にしない（空のままでは有効化も実行もできない）。\n\
         - http.request は fail_on_error_status: true で、相手が 400 以上を返したときブロックを失敗にする（省略は成功として status を返す）。\n\
         - http.request の headers は {{\"ヘッダーの名前\": 値}}（名前はリテラル・値は差し込みや参照も書ける・{max_headers} 個まで・値は {max_value_kb} KB まで）。{reserved} と、シークレットを付けるヘッダー（既定は Authorization）は書けない（保存できない）。Idempotency-Key・Content-Type を書くと、Shiki が付ける値の代わりにそれを送る。送り先が内部のネットワークのアドレスにつながる URL には送れない（管理者がそのホストを社内の送り先として登録したときだけ送れる）。\n\
         - workflow.start の name は、保存する人が見られる既存のワークフローの name（無い・見えない name は保存できない）。\n\
         - 条件（control.branch の condition・トリガと control.wait の filter）: {{\"cmp\":{{\"left\":値,\"op\":CmpOp,\"right\":値}}}}・{{\"all\":[条件…]}}・{{\"any\":[条件…]}}・{{\"not\":条件}}。CmpOp = {ops}（exists・is_null は right を書かない）。例: {cond}\n\
         - data.query の filter は形が違う: {{\"cmp\":{{\"field\":\"<列の参照名>\",\"op\":CmpOp,\"value\":値}}}}（all / any / not は同じ）。query.limit は 1〜{max_limit}（省略は 50。範囲の外は保存できない）。集計は query の group_by と aggregate（[{{\"metric\":\"count\"}}] や {{\"metric\":\"sum\",\"field\":\"<数値の列>\"}}）で書け、結果は groups に入る（エディタの画面では作れない）。\n\
         - 同じものを 2 件にしない書き込み（同じ日・同じ ID の行を再実行や取り込み直しで増やさない）は、data.record.upsert（match_on に重複を許さない〔unique〕列の参照名、data にその列の値と書く値。あれば書き換え、なければ追加。出力の created で分かる）か、data.bulk_upsert の match_on（id の無い行をその列の値で探して更新）を使う。data.query で探してから data.record.create と data.record.update に分けて書かない（同時に動くと 2 件できる）。match_on に選べるのは、data_schema で（重複を許さない）と出る列だけ（ほかの列は保存できない。無ければ、そういう列を足すよう利用者に頼む）。\n\
         - 在庫を減らす・合計に足すような数の書き換えは add（{{\"<数の列>\": 足す数}}・負の数で減らす）を使う（data.record.update と data.record.upsert）。読んだ値に足した値を patch に書くと、同時に動いたときに足し分が消える。足した結果が列の最小値・最大値を外れると書かずに失敗するので、在庫を 0 未満にしないなら列の最小値を 0 にしておく。\n\
         - 前日・先週・先月・月末・週の初めなどの日付は transform.date で求める（script で計算しない）。base を省くと実行情報の scheduled_at（schedule では予定の時刻で、遅れて動いても変わらない）から数える。shift でずらし（{{\"amount\":-1,\"unit\":\"week\"}} で先週）、period（day・week・month・year）を書くと、その日を含む期間の最初の日 start・最後の日 end（YYYY-MM-DD。テーブルの日付の列は start 以上・end 以下で絞る）と、始まりの日時 start_at・次の期間の始まりの日時 next_start_at（日時の列は start_at 以上・next_start_at より前で絞る）が出る。週の初めは week_starts_on（既定 monday）、時間帯は tz（既定 Asia/Tokyo）。月をずらした先にその日が無ければ月末になる。\n",
        max_limit = data::MAX_QUERY_LIMIT,
        list_max = workflow_engine::vocab::STORAGE_LIST_MAX_ITEMS,
        max_headers = workflow_engine::nodes::http_headers::MAX_HEADERS,
        max_value_kb = workflow_engine::nodes::http_headers::MAX_VALUE_BYTES / 1024,
        reserved = reserved_headers(),
        run = run_info_fields(),
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
    d.push_str(&structured_guide());
    d.push_str(&decide::guide(decision_models));
    d.push_str(REPLY_GUIDE);
    d.push_str(&agent_guide());
    d.push_str(&catalog_summary());
    d
}

/// AI の出力の形と思考の深さ（llm.invoke の output_schema・effort・GAP-13）。上限はエンジンの定数から作る。
fn structured_guide() -> String {
    use workflow_engine::ir::output_schema::{
        MAX_DEPTH, MAX_ENUM_VALUES, MAX_OPTIONAL, MAX_PROPERTIES, MAX_UNIONS,
    };
    use workflow_engine::ir::validate::MIN_STRUCTURED_MAX_TOKENS;
    format!(
        "【AI の出力の形】llm.invoke に output_schema（JSON Schema のリテラル）を書くと、出力は {{ text, model, data }} になり、data に形で検証した値が入る（後続は {{\"$from\":\"nodes.<id>.output\",\"path\":\"/data/<項目>\"}} で読む。分類・抽出は script で JSON を解析せずにこれを使い、選択肢は enum で縛る）。\n\
         - 根は {{\"type\":\"object\"}}。オブジェクトは決めた項目だけ（additionalProperties は false か省略。省略しても閉じたものとして扱う）。type は 1 つの型か [型, \"null\"]。項目名は英字か _ で始め英数字と _ だけ。使えるのは input_schema と同じキーワード（$ref・anyOf・oneOf は使えない）。\n\
         - 上限: 入れ子 {MAX_DEPTH} 段・項目 {MAX_PROPERTIES}・任意の項目（required に無い項目）{MAX_OPTIONAL}・null を許す項目 {MAX_UNIONS}・enum の値 {MAX_ENUM_VALUES}（どれも全体で数える）。\n\
         - 項目の順: オブジェクトに \"x-order\":[\"summary\",\"category\"] を書くと、AI はその順に項目を書く（properties のキーだけを重ねずに書く・入れ子のオブジェクトにも書ける）。省くと項目は名前の辞書順で AI に渡る。理由や要約を先に、結論（enum の選択など）を後に並べると、答えの質が上がることがある。\n\
         - 形に合わない出力は 1 回だけ直させ、それでも合わなければ step は llm_output_invalid で失敗する。上限で切れたら llm_output_truncated、AI が回答を拒否したら llm_refused（どちらも直さない）。max_tokens を書くなら {MIN_STRUCTURED_MAX_TOKENS} 以上（思考の分も数える）。\n\
         - effort（\"low\" | \"medium\" | \"high\"）は思考の深さ。省略は組織の既定。model を省略して組織の既定の Auto で動くときと、組織がモデルの選択を禁じているときは組織の設定に従い、書いても使われない（保存時に {effort_ignored} の警告）ので、model を選んだときだけ書く。分類・抽出は low で足りることが多い。\n\
         例（ノード）: {example}\n",
        example = minify(EXAMPLE_STRUCTURED_NODE),
        effort_ignored = workflow_engine::EFFORT_IGNORED,
    )
}

/// AI エージェント（agent.invoke）の書き方（道具の一覧は道具のカタログの単一定義から作る・BUG-6）。
fn agent_guide() -> String {
    let tools = agent_tool_catalog()
        .into_iter()
        .filter(|e| e.available)
        .map(|e| {
            let outside = if e.reaches_outside {
                "・社外へ出る"
            } else {
                ""
            };
            format!(
                "{}（{}・スコープ {}{outside}）",
                e.tool.as_str(),
                e.label_ja,
                e.required_scope.as_str()
            )
        })
        .collect::<Vec<_>>()
        .join(" / ");
    format!(
        "【AI エージェント】agent.invoke は instruction（自然文の依頼。コードではない）を、allowed_tools に書いた道具で調べながら果たし、最後の回答を出力の /text に書く。allowed_tools を省くと道具なし。道具: {tools}。書込の道具は無い（書き込みは後ろのノードで行う）。web.fetch は egress_allowlist（管理者が AI エージェント用に許可したサイトの中から選ぶ。http.request の許可先とは別で、管理者が許可していなければ web.fetch は使えない）が要る。context に参考データ（file_id など）を渡せる。手順の決まった処理は llm.invoke と能力ノードをつなぐ方が安く確実。\n"
    )
}

/// 結果を画面に返すノード（workflow.reply）の書き方の指針（GAP-09・D-28）。
const REPLY_GUIDE: &str = "【画面に返す結果】workflow.reply は、ボタン（ミニアプリ・会話の図）から起動した本人の画面に結果を出す（schedule・event の run と、繰り返し（map）の中では使えない。10 個まで）。\n\
- 成功の結果は副作用（データの追加・ファイルの保存など）の最後に置く（途中で失敗したのに「完了しました」と見せない）。待つ前に「受け付けました」を返すのは可。\n\
- 業務上の失敗（先に借りられた・在庫が無い）は、失敗し得るノードを on_error: \"continue\" にして error の出口の先に tone: \"error\" の reply を置く。分岐ごとに別の reply を置ける。\n\
- format は text（文章）・markdown・table（配列の各要素が 1 行・columns で列を選ぶ）・file_link（社内検索の /results なら file: {\"id_path\":\"/file_id\",\"name_path\":\"/file_name\"}）。\n\
- label・columns の見出しなど固定の文言は、アプリを使う人全員に見える（秘密を書かない）。値は押した本人の権限で読んだものだけが入る。\n";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
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
        let auto = Catalog {
            default_is_auto: true,
            ..Catalog::default()
        };
        for (name, raw) in examples() {
            let ir_json = embedded(raw);
            let ir = WorkflowIr::from_json(&ir_json).unwrap_or_else(|e| panic!("{name}: {e}"));
            // そのまま有効化できる（schedule の例の保存先フォルダの省略などを載せない・D-40・D-43）。
            assert_eq!(
                workflow_engine::enable_blockers(&ir),
                Vec::new(),
                "{name}: 有効化できない例は載せない"
            );
            for (label, catalog) in [
                ("カタログなし", &Catalog::default()),
                ("カタログあり", &with_models),
                ("既定が Auto", &auto),
                ("選択の禁止・許可 0 件", &locked),
            ] {
                validate(&ir_json, catalog).unwrap_or_else(|e| panic!("{name}（{label}）: {e:?}"));
                assert!(
                    workflow_engine::save_warnings(&ir, catalog).is_empty(),
                    "{name}（{label}）: 警告が出る例は載せない"
                );
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
            "\"x-order\":[\"title\",\"body\"]",
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
        let d = emit_description(None, None);
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
        // 実行情報の項目は単一定義から作る（実行した日をファイル名に差し込める・D-97）。
        for f in workflow_engine::vocab::RUN_INFO_FIELDS {
            assert!(d.contains(&format!("{}: {}（", f.name, f.ty)), "{}", f.name);
        }
        // 手動の実行で実行した人を値に使える（入力項目で受け取らない・人のいない実行には入らない・D-101）。
        assert!(
            d.contains("started_by: string（実行した人・手動で実行した人のユーザー ID"),
            "{d}"
        );
        assert!(d.contains("on_name_taken に \"add_number\""), "{d}");
        // 同じものを 2 件にしない書き込みと数の足し引きは、探してから分けて書かず upsert と add で書く（GAP-16）。
        assert!(
            d.contains("data.record.upsert（match_on に重複を許さない"),
            "{d}"
        );
        assert!(d.contains("add（{\"<数の列>\": 足す数}"), "{d}");
        // 前日・先週・月末などは script ではなく日付を計算で求め、予定の日時から数える（GAP-15）。
        assert!(
            d.contains("transform.date で求める（script で計算しない）"),
            "{d}"
        );
        assert!(d.contains("- transform.date（日付を計算）"), "{d}");
        // 新しい CSV は Excel で開ける文字コードで保存する（D-120）。
        assert!(d.contains("encoding に \"utf8_bom\" を書く"), "{d}");
        // CSV を集計は Excel も読み、数・真偽を種類のまま使うなら items を使う（GAP-40・D-17）。
        assert!(
            d.contains("Excel を読めるのは csv.query だけ") && d.contains("\"path\":\"/items\"}"),
            "{d}"
        );
        // CSV を保存に表や項目のまとまりのリストをそのまま渡せる（BUG-178）。
        assert!(
            d.contains(
                "csv.query の結果（{\"$from\":\"nodes.<id>.output\"}・{columns, rows} の表）"
            ),
            "{d}"
        );
        // 演算子の一覧とイベント source は型・語彙から生成する。
        assert!(d.contains("\"starts_with\""), "{d}");
        assert!(d.contains("EventSource = \"storage.write\""), "{d}");
        // ファイル保存トリガは新しい版の保存でも起き、続く保存は 1 回にまとめる（BUG-28・D-38）。
        assert!(d.contains("既存のファイルの新しい版（op \"update\""), "{d}");
        assert!(
            d.contains(&format!(
                "同じファイルの保存が {} 秒途切れてから",
                workflow_engine::scheduler::COALESCE_QUIET_SECS
            )),
            "{d}"
        );
        assert!(!d.contains("当てにしない"), "{d}");
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

    /// 画面に返す結果の書き方の指針が載る（副作用の最後・error の出口・固定の文言は全員に見える・GAP-09）。
    #[test]
    fn description_explains_where_to_put_replies() {
        let d = emit_description(None, None);
        assert!(d.contains("- workflow.reply（結果を画面に返す）"), "{d}");
        assert!(d.contains("成功の結果は副作用"), "{d}");
        assert!(
            d.contains("error の出口の先に tone: \"error\" の reply"),
            "{d}"
        );
        assert!(d.contains("アプリを使う人全員に見える"), "{d}");
        // 例の reply を含む IR は保存時検証を通る。
        let ir = serde_json::json!({
            "ir_version": 1, "name": "lend", "triggers": [{ "kind": "interactive" }],
            "nodes": [
                { "id": "ok", "type": "workflow.reply",
                  "params": { "format": "text", "tone": "success", "value": "貸し出しました" } },
                { "id": "refs", "type": "workflow.reply",
                  "params": { "format": "file_link", "value": [],
                              "file": { "id_path": "/file_id", "name_path": "/file_name" } } }
            ],
            "edges": [{ "from": "ok", "to": "refs" }]
        });
        validate(&ir, &Catalog::default()).unwrap();
    }

    /// AI エージェントの道具の一覧が道具のカタログから載り、書いた例は保存時検証を通る（BUG-6）。
    #[test]
    fn description_lists_agent_tools() {
        let d = emit_description(None, None);
        assert!(d.contains("【AI エージェント】"), "{d}");
        assert!(
            d.contains("rag.search（社内検索・スコープ rag.query）"),
            "{d}"
        );
        assert!(
            d.contains("web.fetch（Web ページを読む・スコープ http.egress・社外へ出る）"),
            "{d}"
        );
        assert!(
            !d.contains("data.query（テーブルを調べる"),
            "予約の道具は載せない"
        );
        // web.fetch の宛先は AI エージェント用の許可だけ（http.request の許可先を継がない・D-59）。
        assert!(d.contains("http.request の許可先とは別"), "{d}");
        // フォルダ一覧の続きの読み方（D-131・件数は上限の単一定義から）。
        assert!(
            d.contains(&format!(
                "storage.list は名前の順に {} 件までを返し",
                workflow_engine::vocab::STORAGE_LIST_MAX_ITEMS
            )) && d.contains("\"path\":\"/next_cursor\"}"),
            "{d}"
        );
        // http.request のヘッダーの書き方と、書けないヘッダー（Shiki が決める表から・GAP-19）。
        assert!(
            d.contains("http.request の headers は {\"ヘッダーの名前\": 値}"),
            "{d}"
        );
        assert!(
            d.contains("Host・Content-Length・Transfer-Encoding・Connection・Keep-Alive・Proxy-Connection・\
                        Proxy-Authorization・TE・Trailer・Upgrade・Expect・Accept-Encoding と、"),
            "{d}"
        );
        let ir = serde_json::json!({
            "ir_version": 1, "name": "check-contract", "triggers": [{ "kind": "interactive" }],
            "declared_scopes": ["rag.query", "doc.read"],
            "nodes": [{ "id": "review", "type": "agent.invoke",
                        "params": { "instruction": "契約書を読み、社内のチェックリストと照らして要注意の条項を挙げる",
                                    "context": { "$from": "input" },
                                    "allowed_tools": ["doc.read", "rag.search"] } }],
            "edges": []
        });
        validate(&ir, &Catalog::default()).unwrap();
    }

    /// 各ノードの params の形と出力の形が説明文に載る（ts-rs の宣言・カタログから生成）。
    #[test]
    fn description_lists_params_and_outputs_of_available_nodes() {
        let d = emit_description(None, None);
        assert!(
            d.contains("- llm.invoke（AI に聞く）")
                && d.contains("params { model?: string, prompt: ValueExpr"),
            "{d}"
        );
        assert!(d.contains(
            "出力 { text: string, model: string, data?: unknown（output_schema を書いたとき"
        ));
        assert!(d.contains("SwitchCase = { port: string, equals: unknown }"));
        for e in node_catalog() {
            let line = format!("- {}（", e.node_type.as_str());
            assert_eq!(d.contains(&line), e.available, "{}", e.node_type.as_str());
        }
    }

    /// 出力の形と思考の深さの書き方が載り、例のノードはそのまま保存時検証を通る（GAP-13）。
    #[test]
    fn description_explains_output_schema_and_effort() {
        let d = emit_description(None, None);
        assert!(d.contains("【AI の出力の形】"), "{d}");
        assert!(d.contains("\"path\":\"/data/<項目>\""), "{d}");
        assert!(d.contains("任意の項目（required に無い項目）24"), "{d}");
        assert!(d.contains("max_tokens を書くなら 1024 以上"), "{d}");
        assert!(
            d.contains("effort（\"low\" | \"medium\" | \"high\"）"),
            "{d}"
        );
        // params の形にも ts-rs の宣言から載る。
        assert!(
            d.contains("effort?: LlmEffort, output_schema?: Record<string, unknown>"),
            "{d}"
        );
        assert!(
            d.contains("LlmEffort = \"low\" | \"medium\" | \"high\""),
            "{d}"
        );
        let node: Value = serde_json::from_str(&minify(EXAMPLE_STRUCTURED_NODE)).unwrap();
        assert!(
            node["params"].get("model").is_none(),
            "例に model を書かない"
        );
        // 項目の順は x-order で決まり、AI へ送る形は要約を先に書く（名前の辞書順なら category が先・BUG-123・BUG-139）。
        assert!(
            d.contains("\"x-order\":[\"summary\",\"category\"] を書くと、AI はその順に項目を書く"),
            "{d}"
        );
        let schema = &node["params"]["output_schema"];
        let wire = llm_gateway::structured::wire_schema_text(schema);
        assert!(
            wire.find("\"summary\"").unwrap() < wire.find("\"category\"").unwrap(),
            "{wire}"
        );
        assert!(!wire.contains("x-order"), "{wire}");
        let mut unordered = schema.clone();
        unordered.as_object_mut().unwrap().remove("x-order");
        let wire = llm_gateway::structured::wire_schema_text(&unordered);
        assert!(
            wire.find("\"category\"").unwrap() < wire.find("\"summary\"").unwrap(),
            "{wire}"
        );
        let ir = serde_json::json!({
            "ir_version": 1, "name": "triage", "triggers": [{ "kind": "interactive" }],
            "input_schema": { "type": "object", "properties": { "body": { "type": "string" } },
                              "required": ["body"] },
            "nodes": [node], "edges": []
        });
        let parsed = validate(&ir, &Catalog::default()).unwrap();
        // model を書かない例なので、組織の既定が Auto でも警告が出ない（effort を書かない・D-75）。
        for catalog in [
            Catalog::default(),
            Catalog {
                default_is_auto: true,
                ..Catalog::default()
            },
        ] {
            assert!(workflow_engine::save_warnings(&parsed, &catalog).is_empty());
        }
    }

    #[test]
    fn description_lists_the_models_of_the_user() {
        let ids: Vec<String> = vec!["fast-model".into(), "smart-model".into()];
        let d = emit_description(Some(&ids), None);
        assert!(d.contains(
            "次の id のどれかを書く（このユーザーの組織で使えるもの）: fast-model, smart-model。"
        ));
        let many: Vec<String> = (0..35).map(|i| format!("m{i}")).collect();
        let d = emit_description(Some(&many), None);
        assert!(d.contains("m29（ほか 5 件）"), "件数の上限");
        assert!(!d.contains("m30"));
        let d = emit_description(Some(&[]), None);
        assert!(d.contains("model は書かない"), "指定できるモデルが無い構成");
    }

    /// model は省略が正しい書き方（組織の既定で動く）。必須・デプロイの既定とは書かず、例にも model を書かない
    /// （BUG-12）。
    #[test]
    fn description_says_an_omitted_model_follows_the_organization_default() {
        let ids: Vec<String> = vec!["fast-model".into()];
        for models in [None, Some(&ids[..]), Some(&[][..])] {
            let d = emit_description(models, None);
            assert!(
                d.contains("省略すると組織の既定のモデルで動く"),
                "{models:?}"
            );
            assert!(!d.contains("model は必須"), "{models:?}");
            assert!(!d.contains("デプロイの既定"), "{models:?}");
            // 判定（ai.decide）は組織の既定が無いので model を書く。それ以外の例には書かない。
            assert!(
                !d.replace(&decide::guide(None), "").contains("\"model\":"),
                "{models:?}: 例に model を書かない"
            );
            assert!(d.contains(&decide::guide(None)));
        }
    }

    /// 説明文と例のスケジュール（0 9 * * 1-5）は、サーバーの発火で平日（月〜金）の 9 時に動く（BUG-95。曜日を
    /// cron クレートの番号で読むと日曜〜木曜になっていた）。
    #[test]
    fn schedule_examples_run_on_weekdays() {
        use chrono::{Datelike, TimeZone, Timelike};
        let zone: chrono_tz::Tz = "Asia/Tokyo".parse().unwrap();
        // 日本時間の 2026-10-25（日）0:00 から 1 週間。
        let after = chrono::Utc
            .with_ymd_and_hms(2026, 10, 24, 14, 59, 0)
            .unwrap();
        let until = chrono::Utc
            .with_ymd_and_hms(2026, 10, 31, 14, 59, 0)
            .unwrap();
        let weekdays = |cron: &str, tz: &str| -> Vec<(u32, u32)> {
            workflow_engine::scheduler::cron::occurrences_between(cron, tz, after, until, 100)
                .unwrap()
                .iter()
                .map(|t| {
                    let local = t.with_timezone(&zone);
                    (local.weekday().num_days_from_sunday(), local.hour())
                })
                .collect()
        };
        let weekday_9am: Vec<(u32, u32)> = (1..=5).map(|d| (d, 9)).collect();
        let ir = embedded(EXAMPLE_REVIEW);
        let schedule = &ir["triggers"][0];
        assert_eq!(schedule["kind"], "schedule");
        assert_eq!(
            weekdays(
                schedule["cron"].as_str().unwrap(),
                schedule["tz"].as_str().unwrap()
            ),
            weekday_9am
        );
        // 説明文の骨格に書いた例も同じ式で、平日と言い添えている。
        let d = emit_description(None, None);
        let examples: Vec<&str> = d
            .match_indices("\"cron\":\"")
            .map(|(i, m)| {
                let rest = &d[i + m.len()..];
                &rest[..rest.find('"').unwrap()]
            })
            .collect();
        assert!(!examples.is_empty());
        for cron in examples {
            assert_eq!(weekdays(cron, "Asia/Tokyo"), weekday_9am, "{cron}");
        }
        assert!(d.contains("この例は平日の 9 時"), "{d}");
    }
}

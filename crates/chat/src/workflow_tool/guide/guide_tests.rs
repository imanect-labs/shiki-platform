//! emit_workflow の説明文（[`super`]）の単体テスト。

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
    let edge: Edge = serde_json::from_value(serde_json::json!({ "from": "a", "to": "b" })).unwrap();
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
    // 状態の列は data.transition でだけ変えられ、追加したレコードは最初の状態になる（BUG-251）。
    assert!(
        d.contains("状態の列は、data.transition（状態を進める）でだけ変えられる")
            && d.contains("data.record.create で追加したレコードは最初の状態になるので、状態の列は書かないか、最初の状態を書く"),
        "{d}"
    );
    // まとめて登録の行も同じ決まり（保存時に止める・GAP-129）。
    assert!(
        d.contains("data.bulk_upsert の rows の行の data も同じで、id のある行と、match_on を書いたときの行には状態の列を書かず"),
        "{d}"
    );
    // data.query に書ける列は data_schema の印で選ぶ（索引の無い列は保存で止まる・BUG-244）。まとめる列と集計の列は
    // さらに絞られる（束縛の group_key_fits・metric_fits と同じ）。
    assert!(
        d.contains("filter・sort に書ける列は、data_schema で（索引あり）か（重複を許さない）と出る列とシステム列")
            && d.contains("contains・ends_with は（部分一致で探せる）と出る列だけで、その列は索引が無くても使える")
            && d.contains("型が created_by・record_ref・multi_select でない列だけ（システム列も書けない）")
            && d.contains("sum・avg・min・max の field は、（索引あり）か（重複を許さない）と出る number の列だけ"),
        "{d}"
    );
    // リンクの条件の中の列は、リンク先の定義で同じ決まり（保存時にリンク先の定義で照らす・GAP-129。この書き方が保存と
    // 実行を通ることは api の workflow_approval_it が確かめる）。
    assert!(
        d.contains("filter に {\"link\":{\"field\":\"<リンクの列>\",\"filter\":条件}} と書き、中の条件の列は、リンク先のテーブルの data_schema の印で同じ決まりに従う"),
        "{d}"
    );
    // 結合先の列は group_by・aggregate に 別名.参照名 で書き、結合先の定義で同じ決まり（GAP-129。この書き方が保存と
    // 実行を通ることは api の workflow_approval_it が確かめる）。
    assert!(
        d.contains("query の joins に [{\"alias\":\"m\",\"field\":\"<リンクの列>\"}] と書き、group_by と aggregate の field に \"m.<列の参照名>\" と書く（")
            && d.contains("filter・sort には書けない。つないだテーブルで絞る条件は joins の filter に書く）"),
        "{d}"
    );
    // キーワードで探す列は部分一致で探せる列だけ（保存時に照らす・GAP-129。この書き方が保存と実行を通ることは api の
    // workflow_approval_it が確かめる）。
    assert!(
        d.contains("探す列を search_fields で絞るなら（部分一致で探せる）と出る列だけを書く（省略はそういう列すべて。そういう列の無いテーブルでは search を使えない）"),
        "{d}"
    );
    // 入力の数を引く書き方と、足す数に組み立てを書けないこと（BUG-245・この書き方が保存と実行を通ることは
    // workflow-engine のテストが確かめる）。
    assert!(
        d.contains("入力の項目で負の数を受け取って {\"$from\":\"input\",\"path\":\"/delta\"}")
            && d.contains("function main(i) { return { delta: -i.qty }; }")
            && d.contains("足す数に $template は書けない"),
        "{d}"
    );
    // 前日・先週・月末などは script ではなく日付を計算で求め、予定の日時から数える（GAP-15）。
    assert!(
        d.contains("transform.date で求める（script で計算しない）"),
        "{d}"
    );
    assert!(d.contains("- transform.date（日付を計算）"), "{d}");
    // 子の実行は親の予定の日時を引き継ぐので、入力で渡させない。親に予定の日時が無ければ、子は自分を始めた日時から
    // 数える（言い切らない）。手動の実行で予定の日時を試せる（GAP-88・GAP-90）。
    assert!(
        d.contains("始めた実行に予定の日時（schedule の予定の時刻・手動の実行で指定した日時）があればそれを引き継ぐ")
            && d.contains("子の scheduled_at は子の実行が始まった日時（子の started_at と同じ）になる")
            && d.contains("親の transform.date の結果（start など）を子の入力で渡す")
            && d.contains("予定の日時を指定して試せる"),
        "{d}"
    );
    assert!(
        !d.contains("始めた実行の scheduled_at を引き継ぐ"),
        "親に予定の日時が無いときも引き継ぐとは言わない: {d}"
    );
    // 新しい CSV は Excel で開ける文字コードで保存する（D-120）。
    assert!(d.contains("encoding に \"utf8_bom\" を書く"), "{d}");
    // CSV を集計は Excel も読み、数・真偽を種類のまま使うなら items を使う（GAP-40・D-17）。
    assert!(
        d.contains("Excel を読めるのは csv.query だけ") && d.contains("\"path\":\"/items\"}"),
        "{d}"
    );
    // フォルダ・ファイルの id は drive.find、テーブルは data_schema で調べる（GAP-107）。
    assert!(
        d.contains(
            "フォルダ・ファイルの id は drive.find で名前から調べ（フォルダは kind: \"folder\""
        ) && d.contains("テーブルの id は data_schema で調べる")
            && !d.contains("doc_search などで調べる"),
        "{d}"
    );
    // CSV をつなぐ集計は tables に書き、SQL では file を data・tables を name の名前で読む（#1059）。
    assert!(
        d.contains(&format!(
            "tables に [{{\"name\":\"master\",\"file\":\"<CSV の uuid>\"}}, …]（{} 個まで",
            tabular::MAX_EXTRA_TABLES
        )) && d.contains("file の CSV を data、tables の CSV を name の名前で読む"),
        "{d}"
    );
    // name の決まりは tabular の判定と同じに言う（保存で止まる決まりを先に教える・GAP-126）。
    assert!(
        d.contains("name は英小文字・数字・_ で、先頭は数字以外・63 文字まで・data 以外・重ねない"),
        "{d}"
    );
    assert!(tabular::is_valid_table_name(&"m".repeat(63)));
    assert!(!tabular::is_valid_table_name(&"m".repeat(64)));
    assert!(!tabular::is_valid_table_name("2024_sales"));
    // Excel の表の上の表題の行は飛ばす（GAP-83）。
    assert!(
        d.contains("表の上の表題の行は飛ばし、値の多い最初の行が列の名前"),
        "{d}"
    );
    // Excel のほかのシートは名前で読む（GAP-84）。
    assert!(
        d.contains("ほかのシートは FROM \"集計\" のようにシートの名前で読む"),
        "{d}"
    );
    // CSV を保存に表や項目のまとまりのリストをそのまま渡せる（BUG-178）。
    assert!(
        d.contains("csv.query の結果（{\"$from\":\"nodes.<id>.output\"}・{columns, rows} の表）"),
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

/// テーブルの行を読む・書く書き方（集計の行の指し方・CSV の行の書き方・一括で登録の行の形）。
#[test]
fn description_explains_how_to_read_and_write_table_rows() {
    let d = emit_description(None, None);
    // 集計の行（data の AggregateGroup）を map で回すときの指し方と、出力の形（カタログ）の行の形（GAP-132。この
    // 書き方が保存と実行を通ることは workflow-engine の csv_to_table_it が確かめる）。
    assert!(
        d.contains("groups の 1 行は {key: {group_by に書いた列: 値}, values: [aggregate と同じ順の値], value: values の先頭}")
            && d.contains("{\"$from\":\"each\",\"path\":\"/item/key/<group_by に書いた列>\"}")
            && d.contains("{\"$from\":\"each\",\"path\":\"/item/values/<aggregate の位置（0 から）>\"}")
            && d.contains("/item/<列の参照名>・/item/count のような書き方では読めない")
            && d.contains("groups?: Array<{ key: { [group_by に書いた列]: string | null }, value: number | null, values: Array<number | null> }>"),
        "{d}"
    );
    // 伏せた行の values は配列のまま中の値だけが null（data の suppress）。結合先の列でまとめた行の key の名前は
    // m.<参照名> のまま（data の col_key。この読み方が実行を通ることは csv_to_table_it が確かめる）。
    assert!(
        d.contains("件数が少なくて伏せた行は value が null で、values は aggregate と同じ数の null が並ぶ（values そのものは null にならない）")
            && !d.contains("value と values が null")
            && d.contains("groups の key の名前も \"m.<列の参照名>\" のままなので、map の中では /item/key/m.<列の参照名> で読む")
            && d.contains("key の名前は group_by に書いたとおり（結合先の列は m.<参照名> のような 別名.参照名）"),
        "{d}"
    );
    // CSV の行をテーブルに書くのは map の中の 1 件の書き込みで、csv.query の items を一括の rows にそのまま渡させない
    // （rows は {data: …} の行だけを受け、平らな行は実行で断られる・BUG-255。この書き方と script.run で包む書き方が
    // 保存と実行を通ることと、上限は workflow-engine の csv_to_table_it が確かめる）。
    assert!(
        !d.contains("data.bulk_upsert の rows に渡す）"),
        "csv.query の items を rows に渡すと教えない: {d}"
    );
    assert!(
        d.contains("control.map の items に csv.query の items を書き")
            && d.contains(
                "{\"<列の参照名>\":{\"$from\":\"each\",\"path\":\"/item/<CSV の列の名前>\"}}"
            )
            && d.contains("data に {\"$from\":\"each\",\"path\":\"/item\"} とそのまま書ける")
            && d.contains(&format!(
                "map で書けるのは {} 行まで",
                workflow_engine::ir::params::MAX_MAP_ITEMS
            ))
            && d.contains("csv.query の items は data.bulk_upsert の rows にそのまま渡せない")
            && d.contains("rows は 1 行ずつ {\"data\":{列の参照名: 値}} の配列"),
        "{d}"
    );
    // 一括で登録の params の形は、rows に書く行の形を、実行が行を読む型から言う（GAP-131）。
    assert!(
        d.contains("rows の値 = Array<{ id?: string, rev?: number, key?: string, data: { [列の参照名]: unknown } }>"),
        "{d}"
    );
    // script.run の戻り値の上限は script-runtime の単一定義から言い、包んだ行で当たるときの分け方を言う（BUG-264。
    // 分けた書き方が保存と実行を通ることは workflow-engine の csv_to_table_it が確かめる）。
    let script_kb = workflow_engine::nodes::script::MAX_RESULT_BYTES / 1024;
    assert_eq!(script_kb, 256);
    assert!(
        d.contains("function main(i) { return { rows: i.items.map((r) => ({ data: r })) }; }")
            && d.contains(&format!(
                "（{} 行まで。script.run の戻り値は {script_kb} KB まで",
                workflow_engine::ir::params::MAX_WORKFLOW_BULK_ROWS
            ))
            && d.contains("i.items.slice(0, 2000) と i.items.slice(2000) のように行を分けて包む script.run を 2 つ置き、それぞれの結果を data.bulk_upsert に渡す")
            && d.contains(&format!(
                "script.run の戻り値（出力）は {script_kb} KB まで（JSON にした大きさ）。超えると invalid で失敗する"
            )),
        "{d}"
    );
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
    // script の書き込みと送信の中身を、動かすたびに変えない（動かし直すと effect_conflict になる・GAP-28）。
    assert!(
        d.contains("Date.now()・Math.random() の値を本文や名前に入れない"),
        "{d}"
    );
    assert!(
        d.contains("{\"$from\":\"run\",\"path\":\"/started_at\"}"),
        "{d}"
    );
    // http.request のヘッダーの書き方と、書けないヘッダー（Shiki が決める表から・GAP-19）。
    assert!(
        d.contains("http.request の headers は {\"ヘッダーの名前\": 値}"),
        "{d}"
    );
    assert!(
        d.contains(
            "Host・Content-Length・Transfer-Encoding・Connection・Keep-Alive・Proxy-Connection・\
                    Proxy-Authorization・TE・Trailer・Upgrade・Expect・Accept-Encoding と、"
        ),
        "{d}"
    );
    // 鍵は headers に書かずシークレットで付ける（保存の注意のコードも言う・GAP-19）。
    assert!(
        d.contains("API キー・トークン・パスワードなどの鍵は headers")
            && d.contains("ir.plain_key_header"),
        "{d}"
    );
    assert!(
        d.contains("\"attach\":{\"kind\":\"header\",\"header\":\"X-Api-Key\"}"),
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
    assert!(
        d.contains("次の id のどれかを書く（このユーザーが使えるもの）: fast-model, smart-model。")
    );
    let many: Vec<String> = (0..35).map(|i| format!("m{i}")).collect();
    let d = emit_description(Some(&many), None);
    assert!(d.contains("m29（ほか 5 件）"), "件数の上限");
    assert!(!d.contains("m30"));
    let d = emit_description(Some(&[]), None);
    assert!(d.contains("model は書かない"), "指定できるモデルが無い構成");
}

/// 省いたときに動く組織の既定のモデルがこのユーザーのアカウントでは使えないとき（D-185）は、省略を勧めずに model を
/// 必ず書かせ、使えるモデルを並べる。使えるモデルも無ければ、モデルを使うノードは保存できないと言う（どちらも保存時の
/// 照合と同じ結果になることは、api の workflow_model_restriction_it が確かめる）。
#[test]
fn description_asks_for_a_model_when_the_default_is_unavailable() {
    let ids: Vec<String> = vec!["visible".into()];
    let d = emit_description_for(Some(&ids), None, Some("hidden"));
    assert!(
        d.contains("【llm.invoke のモデル】model は必ず書く（省略すると組織の既定のモデル（hidden）で動くが、このモデルはこのユーザーのアカウントでは使えないので、省略すると保存できない。instructions だけのスキルを呼ぶ skill.invoke もこの既定で動くので保存できない）。次の id のどれかを書く（このユーザーが使えるもの）: visible。"),
        "{d}"
    );
    assert!(!d.contains("model はふつう省略する"), "{d}");
    let d = emit_description_for(Some(&[]), None, Some("hidden"));
    assert!(
        d.contains("このユーザーが使えるモデルは無い（省略すると組織の既定のモデル（hidden）で動くが、このモデルはこのユーザーのアカウントでは使えない）ので、llm.invoke・agent.invoke と、instructions だけのスキルを呼ぶ skill.invoke（組織の既定のモデルで動く）は保存できない。")
            && !d.contains("model は書かない"),
        "{d}"
    );
    // 既定が使える人には今までどおり省略を勧める。
    assert_eq!(
        emit_description_for(Some(&ids), None, None),
        emit_description(Some(&ids), None)
    );
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

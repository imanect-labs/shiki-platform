use super::*;

fn def(name: &str, description: &str) -> ToolDef {
    ToolDef::new(
        name,
        description,
        json!({"type": "object", "properties": {"path": {"type": "string", "description": "対象"}}}),
    )
}

/// 実ツールに近い構成（常時ロード 3 ＋遅延 6）。説明は実装の言い回しに寄せる。
fn catalog() -> Vec<ToolDef> {
    vec![
        def("doc_search", "社内文書を検索し、引用付きで抜粋を返す。"),
        def("web_search", "Web を検索する。"),
        def("fs_read", "ワークスペースのファイルを読む。"),
        def(
            "csv.query",
            "CSV ファイルに読み取り専用の SQL を発行し、結果の行を返す。",
        ),
        def(
            "csv.patch",
            "CSV の行を追加・更新・削除して新しいバージョンを保存する。",
        ),
        def(
            "office.edit",
            "Office ファイル（docx/xlsx/pptx）を AI が編集し、新しいバージョンにする。",
        ),
        def("slide.edit", "スライドの内容を共同編集で書き換える。"),
        def(
            "save_note",
            "AI が書いた Markdown を新規ノートの下書きにする。",
        ),
        def(
            "document.edit",
            "ノートの本文をアンカー指定で書き換える（共同編集参加者として反映）。",
        ),
    ]
}

/// 閾値を外して必ず有効にする（中身の検証用）。
fn enabled() -> ToolSearchOptions {
    ToolSearchOptions {
        min_deferred_tokens: 0,
        ..ToolSearchOptions::default()
    }
}

fn search() -> ToolSearch {
    prepare(catalog(), &enabled()).1.expect("有効になる")
}

fn query(s: &ToolSearch, q: &str) -> Vec<String> {
    s.handle_lexical(&json!({ "query": q })).references
}

#[test]
fn prepare_defers_vocab_deferred_tools_and_appends_tool_search_last() {
    let (defs, search) = prepare(catalog(), &enabled());
    assert!(search.is_some());
    let deferred: Vec<&str> = defs
        .iter()
        .filter(|d| d.defer_loading)
        .map(|d| d.name.as_str())
        .collect();
    assert_eq!(
        deferred,
        [
            "csv.query",
            "csv.patch",
            "office.edit",
            "slide.edit",
            "save_note",
            "document.edit"
        ]
    );
    let last = defs.last().unwrap();
    assert_eq!(last.name, TOOL_SEARCH_TOOL);
    assert!(!last.defer_loading);
    // 何が検索できるかは説明に載る。
    assert!(last.description.contains("csv.query"));
    // 先頭（stub が呼ぶツール）と常時ロードの並びは変わらない。
    assert_eq!(defs[0].name, "doc_search");
}

#[test]
fn prepare_is_a_no_op_when_disabled_small_or_unknown_tools_only() {
    let off = ToolSearchOptions {
        enabled: false,
        ..enabled()
    };
    let (defs, s) = prepare(catalog(), &off);
    assert!(s.is_none() && defs.iter().all(|d| !d.defer_loading));
    assert_eq!(defs.len(), catalog().len());

    // 推定トークンが下限未満なら使わない（既定の下限は小構成を素通しする）。
    let (_, s) = prepare(catalog(), &ToolSearchOptions::default());
    assert!(s.is_none());

    // 遅延にできるツールが少なすぎる。
    let few = vec![
        def("doc_search", "d"),
        def("csv.query", "q"),
        def("csv.patch", "p"),
    ];
    assert!(prepare(few, &enabled()).1.is_none());

    // 語彙外（テストのモック・plan メタツール）は常時ロード扱い。
    let mocks = vec![def("mock_a", "a"), def("plan", "p")];
    assert!(prepare(mocks, &enabled()).1.is_none());
}

#[test]
fn prepare_keeps_at_least_one_eager_tool() {
    // 全部が遅延候補なら遅延にしない（会話の基本動作が消える・Anthropic は全遅延を拒む）。
    let all: Vec<ToolDef> = catalog().into_iter().skip(3).collect();
    let (defs, s) = prepare(all, &enabled());
    assert!(s.is_none() && defs.iter().all(|d| !d.defer_loading));
}

#[test]
fn always_load_overrides_vocab_default() {
    let opts = ToolSearchOptions {
        always_load: vec!["csv.query".into()],
        ..enabled()
    };
    let (defs, s) = prepare(catalog(), &opts);
    let csv = defs.iter().find(|d| d.name == "csv.query").unwrap();
    assert!(!csv.defer_loading);
    assert!(!query(&s.unwrap(), "select:csv.query").contains(&"csv.query".to_string()));
}

#[test]
fn japanese_and_english_queries_find_the_right_tool_first() {
    let s = search();
    let cases = [
        ("CSV を SQL で集計したい", "csv.query"),
        ("csv の行を更新", "csv.patch"),
        ("Excel ファイルを編集", "office.edit"),
        ("edit a PowerPoint file", "office.edit"),
        ("スライドを書き換える", "slide.edit"),
        ("ノートの本文を書き換える", "document.edit"),
        ("create a markdown note", "save_note"),
    ];
    for (q, want) in cases {
        let got = query(&s, q);
        assert_eq!(got.first().map(String::as_str), Some(want), "{q}: {got:?}");
    }
}

#[test]
fn exact_name_ranks_first_even_in_wire_form() {
    let s = search();
    assert_eq!(query(&s, "csv_patch")[0], "csv.patch");
    assert_eq!(query(&s, "document.edit")[0], "document.edit");
}

#[test]
fn select_loads_exact_names_and_reports_unknown_ones() {
    let s = search();
    let r = s.handle_lexical(&json!({ "query": "select:csv.query, office_edit, nope, csv.query" }));
    assert_eq!(r.references, ["csv.query", "office.edit"]);
    assert!(r.content.contains("nope"), "{}", r.content);
    assert!(!r.is_error);
}

#[test]
fn select_honours_limit() {
    let s = search();
    let r = s.handle_lexical(&json!({ "query": "select:csv.query,csv.patch", "limit": 1 }));
    assert_eq!(r.references, ["csv.query"]);
}

#[test]
fn select_cannot_reach_tools_outside_the_deferred_catalog() {
    // 提示していないツール（語彙には在る）を名指ししても読み込めない＝検索は認可を広げない。
    let s = search();
    assert!(query(&s, "select:shell,fs_delete").is_empty());
    // 常時ロードのツールは検索対象外（既に載っている）。
    assert!(query(&s, "select:doc_search").is_empty());
}

#[test]
fn required_term_filters_by_name() {
    let s = search();
    let got = query(&s, "+csv 編集");
    assert!(
        !got.is_empty() && got.iter().all(|n| n.starts_with("csv.")),
        "{got:?}"
    );
    // `+語` だけでも名前で絞って返す。
    let only = query(&s, "+csv");
    assert_eq!(only.len(), 2, "{only:?}");
}

#[test]
fn limit_caps_results() {
    let s = search();
    let r = s.handle_lexical(&json!({ "query": "編集", "limit": 1 }));
    assert_eq!(r.references.len(), 1);
    let r = s.handle_lexical(&json!({ "query": "編集", "limit": 0 }));
    assert_eq!(r.references.len(), 1, "下限 1 に丸める");
}

#[test]
fn no_hit_lists_searchable_names_instead_of_failing() {
    let s = search();
    let r = s.handle_lexical(&json!({ "query": "weather forecast" }));
    assert!(r.references.is_empty());
    assert!(!r.is_error);
    assert!(r.content.contains("save_note"), "{}", r.content);
}

#[test]
fn invalid_input_is_an_error_observation() {
    let s = search();
    assert!(s.handle_lexical(&json!({})).is_error);
    assert!(s.handle_lexical(&json!({ "query": "  " })).is_error);
    assert!(
        s.handle_lexical(&json!({ "query": "あ".repeat(501) }))
            .is_error
    );
}

#[test]
fn hit_content_lists_loaded_tools_with_summaries() {
    let s = search();
    let r = s.handle_lexical(&json!({ "query": "select:csv.query" }));
    assert!(r.content.contains("1 件"), "{}", r.content);
    assert!(
        r.content.contains("- csv.query: CSV ファイル"),
        "{}",
        r.content
    );
}

#[test]
fn loaded_message_is_the_production_result_text() {
    let s = CatalogSearch::new(&catalog(), None);
    // 名指しは製品の処理そのもの（見つからなかった名前・重複・wire 名まで同じ）。
    let q = "select:csv_query,csv.query,nope";
    let direct = s.0.handle_lexical(&json!({ "query": q }));
    assert_eq!(s.loaded_message(q, None, &[]), direct.content);
    assert!(direct.content.contains("1 件"), "{}", direct.content);
    assert!(direct.content.contains("nope"), "{}", direct.content);
    // 自然文は渡した名前を製品の文面で（重複・索引に無い名前は除く）。
    let got = s.loaded_message(
        "表を集計",
        None,
        &["csv.query".into(), "csv_query".into(), "nope".into()],
    );
    let csv =
        s.0.tools
            .iter()
            .position(|(n, _)| n == "csv.query")
            .expect("csv.query");
    assert_eq!(got, s.0.render(&[csv], &[]).content);
    assert!(s
        .loaded_message("表を集計", None, &[])
        .contains("見つかりませんでした"));
    // 名指しの件数の上限はモデルが渡した limit に従う（製品と同じ）。
    let two = "select:csv.query,csv.patch";
    assert_eq!(
        s.loaded_message(two, Some(1), &[]),
        s.0.handle_lexical(&json!({ "query": two, "limit": 1 }))
            .content
    );
    assert!(s.loaded_message(two, Some(1), &[]).contains("1 件"));
    // 入力の誤りは製品と同じエラーの本文。
    assert_eq!(
        s.loaded_message(" ", None, &[]),
        s.0.handle_lexical(&json!({ "query": " " })).content
    );
}

#[test]
fn summary_takes_first_sentence_and_truncates() {
    assert_eq!(summary("一文目。二文目。"), "一文目。");
    let long = "あ".repeat(100);
    let s = summary(&long);
    assert_eq!(s.chars().count(), SUMMARY_CHARS + 1);
    assert!(s.ends_with('…'));
}

#[test]
fn eval_catalog_matches_the_production_ranking() {
    // 評価の窓口は製品と同じ索引・同じ順位（別実装を持たない）。
    let deferred: Vec<ToolDef> = catalog().into_iter().skip(3).collect();
    let eval = CatalogSearch::new(&deferred, None);
    let prod = search();
    for q in [
        "CSV を SQL で集計したい",
        "スライドを書き換える",
        "edit a PowerPoint file",
    ] {
        assert_eq!(eval.search_lexical(q, 5), query(&prod, q), "{q}");
        let ranked = eval.ranked(q);
        assert!(
            ranked.windows(2).all(|w| w[0].1 >= w[1].1),
            "スコア降順: {ranked:?}"
        );
        // 打ち切り前の順位は、読み込む順位を先頭に含む。
        let head: Vec<String> = ranked
            .iter()
            .take(eval.search_lexical(q, 5).len())
            .map(|(n, _)| n.clone())
            .collect();
        assert_eq!(head, eval.search_lexical(q, 5));
    }
    assert!(eval.definition().description.contains("csv.query"));
    // `select:` の名指しも製品と同じ経路で読み込む。
    assert_eq!(
        eval.search_lexical("select:office_edit", 5),
        ["office.edit"]
    );
}

/// D-41: 検索用の短い文を持つツール（emit_workflow）は、説明文の全文ではなくその文で索引に入る。
/// 説明文にほかのツールの語（スライド・CSV・ノート）がいくら載っていても、それらの依頼で一緒に読み込まれない。
/// 読み込んだときの要約は説明文の先頭のまま（モデルが読む説明文は変えない）。
#[test]
fn tools_with_a_search_text_are_indexed_by_it_not_by_the_long_description() {
    let long = format!(
        "ワークフロー（自動化フロー）を JSON IR として保存する。{}",
        "スライドを編集する・CSV を集計する・ノートの本文を読むノードを並べられる。".repeat(200)
    );
    let mut defs = catalog();
    defs.push(def("emit_workflow", &long));
    let s = prepare(defs, &enabled()).1.expect("有効になる");
    for q in ["スライドを編集", "CSV を集計", "ノートの本文"] {
        let hits = query(&s, q);
        assert!(!hits.is_empty(), "{q}");
        assert!(
            !hits.contains(&"emit_workflow".to_string()),
            "{q}: {hits:?}"
        );
    }
    for q in [
        "毎朝 決まった時刻に 自動で実行する仕組みを作る",
        "create a scheduled workflow",
    ] {
        assert_eq!(
            query(&s, q).first().map(String::as_str),
            Some("emit_workflow"),
            "{q}"
        );
    }
    let content = s
        .handle_lexical(&json!({ "query": "select:emit_workflow" }))
        .content;
    assert!(
        content.contains("emit_workflow: ワークフロー（自動化フロー）を JSON IR として保存する。"),
        "{content}"
    );
}

/// 既存のワークフローを直す依頼では、探す道具（list_workflows）が保存・読む道具と一緒に読み込まれる（GAP-31）。
/// 新しい会話のモデルは id を知らないので、探す道具が上位に無いと直す流れに入れない。
#[test]
fn fixing_an_existing_workflow_loads_the_list_tool() {
    let mut defs = catalog();
    defs.push(def(
        "emit_workflow",
        "ワークフロー（自動化フロー）を JSON IR として保存する。",
    ));
    defs.push(def(
        "read_workflow",
        "既存ワークフローの最新 IR（JSON）を読む。",
    ));
    defs.push(def(
        "list_workflows",
        "ユーザーが見られる既存のワークフローを、名前・表示名・説明に含む語で探す。",
    ));
    let s = prepare(defs, &enabled()).1.expect("有効になる");
    for q in [
        "既存のワークフローを直す",
        "経費のワークフローを探して修正",
        "find the existing workflow and fix it",
    ] {
        let hits = query(&s, q);
        assert!(
            hits.iter().take(5).any(|h| h == "list_workflows"),
            "{q}: {hits:?}"
        );
        assert!(
            hits.iter().take(5).any(|h| h == "emit_workflow"),
            "{q}: {hits:?}"
        );
    }
}

/// 作ったワークフローを試す依頼では試運転の道具（try_workflow）が上位に入り、作る依頼では保存の道具
/// （emit_workflow）が試運転より上に来る（T04）。どちらも説明文ではなく検索用の短い文で索引に入る（D-41）。
#[test]
fn trying_a_workflow_loads_the_try_tool_and_making_one_prefers_emit() {
    let mut defs = catalog();
    defs.push(def(
        "emit_workflow",
        "ワークフロー（自動化フロー）を JSON IR として保存する。",
    ));
    defs.push(def(
        "read_workflow",
        "既存ワークフローの最新 IR（JSON）を読む。",
    ));
    defs.push(def(
        "list_workflows",
        "ユーザーが見られる既存のワークフローを、名前・表示名・説明に含む語で探す。",
    ));
    defs.push(def(
        "try_workflow",
        "保存したワークフローの版を試運転する。承認カードを出し、結果を <run_output> で返す。",
    ));
    let s = prepare(defs, &enabled()).1.expect("有効になる");
    for q in [
        "作ったワークフローを試して",
        "ワークフローを試運転して動作確認",
        "test run the workflow",
    ] {
        let hits = query(&s, q);
        assert!(
            hits.iter().take(5).any(|h| h == "try_workflow"),
            "{q}: {hits:?}"
        );
    }
    let hits = query(&s, "ワークフローを作って");
    let at = |name: &str| hits.iter().position(|h| h == name);
    let emit = at("emit_workflow").expect("作る依頼で emit_workflow が当たる");
    assert!(
        at("try_workflow").is_none_or(|t| emit < t),
        "emit_workflow が try_workflow より上: {hits:?}"
    );
}

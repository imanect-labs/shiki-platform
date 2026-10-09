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
        enabled: true,
        min_deferred_tokens: 0,
        always_load: Vec::new(),
    }
}

fn search() -> ToolSearch {
    prepare(catalog(), &enabled()).1.expect("有効になる")
}

fn query(s: &ToolSearch, q: &str) -> Vec<String> {
    s.handle(&json!({ "query": q })).references
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
    let r = s.handle(&json!({ "query": "select:csv.query, office_edit, nope, csv.query" }));
    assert_eq!(r.references, ["csv.query", "office.edit"]);
    assert!(r.content.contains("nope"), "{}", r.content);
    assert!(!r.is_error);
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
    let r = s.handle(&json!({ "query": "編集", "limit": 1 }));
    assert_eq!(r.references.len(), 1);
    let r = s.handle(&json!({ "query": "編集", "limit": 0 }));
    assert_eq!(r.references.len(), 1, "下限 1 に丸める");
}

#[test]
fn no_hit_lists_searchable_names_instead_of_failing() {
    let s = search();
    let r = s.handle(&json!({ "query": "weather forecast" }));
    assert!(r.references.is_empty());
    assert!(!r.is_error);
    assert!(r.content.contains("save_note"), "{}", r.content);
}

#[test]
fn invalid_input_is_an_error_observation() {
    let s = search();
    assert!(s.handle(&json!({})).is_error);
    assert!(s.handle(&json!({ "query": "  " })).is_error);
    assert!(s.handle(&json!({ "query": "あ".repeat(501) })).is_error);
}

#[test]
fn hit_content_lists_loaded_tools_with_summaries() {
    let s = search();
    let r = s.handle(&json!({ "query": "select:csv.query" }));
    assert!(r.content.contains("1 件"), "{}", r.content);
    assert!(
        r.content.contains("- csv.query: CSV ファイル"),
        "{}",
        r.content
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

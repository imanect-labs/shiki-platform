use super::*;

fn base() -> Url {
    Url::parse("https://portal.example.com/dir/page").unwrap()
}

fn r(html: &str) -> Rendered {
    render(html, &base(), 1, 12_000, &HashMap::new())
}

#[test]
fn links_inputs_and_headings() {
    let out = r(r#"<html><head><title> 備品申請 </title></head><body>
        <h1>備品申請</h1>
        <form method="post" action="/order">
          <label>部署 <select name="dept"><option value="">選択</option><option value="s2">営業二部</option></select></label>
          <label for="item">品目</label><input id="item" name="item">
          <input type="password" name="pw" value="secret">
          <input type="hidden" name="csrf" value="tok">
          <label><input type="checkbox" name="agree" value="1"> 同意する</label>
          <button>申請する</button>
        </form>
        <ul><li><a href="../docs">文書</a></li></ul></body></html>"#);
    assert_eq!(out.title, "備品申請");
    assert!(out.text.contains("# 備品申請"), "{}", out.text);
    assert!(
        out.text
            .contains(r#"[t1.1 select "部署" value="" options: ""=選択, "s2"=営業二部]"#),
        "{}",
        out.text
    );
    assert!(
        out.text
            .contains(r#"[t1.2 textbox "品目" type=text value=""]"#),
        "{}",
        out.text
    );
    assert!(
        out.text.contains(r#"value="******""#),
        "パスワードは伏せる: {}",
        out.text
    );
    assert!(!out.text.contains("secret") && !out.text.contains("tok"));
    assert!(
        out.text
            .contains(r#"[t1.4 checkbox "同意する" checked=false]"#),
        "{}",
        out.text
    );
    assert!(
        out.text.contains(r#"[t1.5 button "申請する"]"#),
        "{}",
        out.text
    );
    assert!(
        out.text
            .contains("- [t1.6 link \"文書\" → https://portal.example.com/docs]"),
        "{}",
        out.text
    );
    assert_eq!(out.forms.len(), 1);
    assert!(out.forms[0].post);
    assert_eq!(
        out.forms[0].action.as_ref().unwrap().as_str(),
        "https://portal.example.com/order"
    );
    let names: Vec<_> = out.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["dept", "item", "pw", "csrf", "agree", ""]);
    assert!(out.fields.iter().all(|f| f.form == Some(0)));
    assert_eq!(out.needs_js, None);
}

#[test]
fn noscript_content_is_visible_and_hidden_content_is_not() {
    let out = r(r#"<body><noscript><p>JS 無効時の本文</p></noscript>
        <div style="display: none">隠し</div><p hidden>隠し2</p><span aria-hidden="true">隠し3</span>
        <script>document.write("x")</script><style>p{}</style><p>見える</p></body>"#);
    assert!(out.text.contains("JS 無効時の本文"), "{}", out.text);
    assert!(out.text.contains("見える"));
    assert!(!out.text.contains("隠し"), "{}", out.text);
    assert!(!out.text.contains("document.write"));
}

#[test]
fn image_alt_labels_links_and_text() {
    let out = r(
        r#"<body><a href="/x"><img src="a.png" alt="お知らせ一覧"></a><p><img alt="ロゴ"> 本文</p>
        <button aria-label="閉じる"></button></body>"#,
    );
    assert!(out.text.contains(r#"link "お知らせ一覧""#), "{}", out.text);
    assert!(out.text.contains("ロゴ 本文"), "{}", out.text);
    assert!(
        out.text.contains(r#"button "閉じる" (要JS)"#),
        "{}",
        out.text
    );
}

#[test]
fn needs_js_detection() {
    // スクリプトがあり、リンク以外の本文が乏しい → JS 必須。
    let spa = r(
        r#"<html><head><script type="module" src="/app.js"></script></head>
        <body><div id="root"></div><a href="/a">ホーム</a></body></html>"#,
    );
    assert_eq!(spa.needs_js, Some(NeedsJs::ScriptRendered));
    // データ用のスクリプト（JSON）だけなら JS 必須ではない。
    let data = r(r#"<body><script type="application/ld+json">{}</script><p>短い</p></body>"#);
    assert_eq!(data.needs_js, None);
    // 本文が十分あればスクリプトがあっても読める。
    let body = format!(
        "<body><script>x()</script><p>{}</p></body>",
        "本文".repeat(150)
    );
    assert_eq!(r(&body).needs_js, None);
}

#[test]
fn tables_lists_and_details() {
    let out = r("<body><table><tr><th>区分</th><th>国内</th></tr><tr><td>一般職</td><td>4,270円</td></tr></table>
        <dl><dt>品名</dt><dd>ドリル</dd></dl><details><summary>質問</summary><p>答え</p></details></body>");
    assert!(out.text.contains("| 区分 | 国内"), "{}", out.text);
    assert!(out.text.contains("| 一般職 | 4,270円"), "{}", out.text);
    assert!(out.text.contains("- 品名"));
    assert!(out.text.contains("  : ドリル"));
    assert!(out.text.contains("▸ 質問"));
}

#[test]
fn form_attribute_binds_outside_controls_and_buttons_outside_forms_need_js() {
    let out = r(
        r#"<body><input name="q" form="f"><form id="f" action="/search"></form>
        <input type="submit" value="外"><input type="file" name="up" aria-label="添付"></body>"#,
    );
    assert_eq!(out.fields[0].form, Some(0));
    assert!(out.text.contains(r#"button "外" (要JS)"#), "{}", out.text);
    assert!(
        out.text.contains("ファイル選択欄「添付」は扱えません"),
        "{}",
        out.text
    );
    assert!(!out.forms[0].post);
}

#[test]
fn truncates_by_chars_and_ignores_non_http_links() {
    let out = render(
        &format!("<body><p>{}</p><a href=\"javascript:void(0)\">js</a><a href=\"mailto:a@b\">m</a></body>", "あ".repeat(50)),
        &base(),
        2,
        10,
        &HashMap::new(),
    );
    assert!(out.truncated);
    assert!(
        out.text.starts_with("ああああああああああ\n"),
        "{}",
        out.text
    );
    assert!(
        out.elements.is_empty(),
        "javascript: / mailto: はリンクにしない"
    );
}

#[test]
fn caps_number_of_elements() {
    let links = (0..(MAX_ELEMENTS + 10)).fold(String::new(), |mut acc, i| {
        use std::fmt::Write as _;
        let _ = write!(acc, "<a href=\"/{i}\">l{i}</a>");
        acc
    });
    let out = render(
        &format!("<body>{links}</body>"),
        &base(),
        1,
        1_000_000,
        &HashMap::new(),
    );
    assert_eq!(out.elements.len(), MAX_ELEMENTS);
    assert!(
        out.text.contains(&format!("l{}", MAX_ELEMENTS + 5)),
        "上限を超えた分もテキストには残す"
    );
}

#[test]
fn field_state_overrides_values() {
    let html = r#"<body><form><input name="q" value="a"><input type="checkbox" name="c">
        <select name="s"><option value="1">一</option><option value="2">二</option></select></form></body>"#;
    let mut state = HashMap::new();
    state.insert(
        0,
        FieldState {
            value: Some("検索語".into()),
            checked: None,
            selected: None,
        },
    );
    state.insert(
        1,
        FieldState {
            value: None,
            checked: Some(true),
            selected: None,
        },
    );
    state.insert(
        2,
        FieldState {
            value: None,
            checked: None,
            selected: Some(vec![1]),
        },
    );
    let out = render(html, &base(), 4, 12_000, &state);
    assert!(
        out.text
            .contains(r#"[t4.1 textbox "q" type=text value="検索語"]"#),
        "{}",
        out.text
    );
    assert!(out.text.contains("checked=true"), "{}", out.text);
    assert!(out.text.contains(r#"value="2""#), "{}", out.text);
    assert_eq!(
        out.fields[0].kind,
        FieldKind::Text {
            value: "検索語".into(),
            secret: false
        }
    );
}

#[test]
fn deep_nesting_does_not_overflow_the_stack() {
    // 10 万段の入れ子（約 1MB）。深さの上限で打ち切り、スタックを溢れさせない。
    let depth = 100_000;
    let html = format!(
        "<body>{}<p>深い</p>{}</body>",
        "<div>".repeat(depth),
        "</div>".repeat(depth)
    );
    let started = std::time::Instant::now();
    let out = render(&html, &base(), 1, 12_000, &HashMap::new());
    assert!(!out.text.contains("深い"), "上限より深い部分は描かない");
    assert!(out.truncated);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    // 入れ子のラベルも上限で打ち切る（二乗の連結をしない）。
    let labels = format!(
        "<body><input id=a>{}{}</body>",
        "<label for=a>ラベル".repeat(200),
        "</label>".repeat(200)
    );
    let out = render(&labels, &base(), 1, 12_000, &HashMap::new());
    assert!(out.elements[0].label.chars().count() <= MAX_LABEL_CHARS);
}

#[test]
fn base_href_resolves_links_but_not_omitted_actions() {
    let out = render(
        r#"<head><base href="/app/"></head><body><a href="next">次</a>
           <form action="save"><input name="a"></form><form><input name="b"></form></body>"#,
        &base(),
        1,
        12_000,
        &HashMap::new(),
    );
    assert!(
        out.text.contains("→ https://portal.example.com/app/next"),
        "{}",
        out.text
    );
    assert_eq!(
        out.forms[0].action.as_ref().unwrap().as_str(),
        "https://portal.example.com/app/save"
    );
    // action を省いたフォームは文書の URL へ送る。
    assert_eq!(
        out.forms[1].action.as_ref().unwrap().as_str(),
        "https://portal.example.com/dir/page"
    );
}

#[test]
fn disabled_options_are_not_selectable_or_default() {
    let out = r(
        r#"<form><select name="s" aria-label="選択"><option value="" disabled>選んでください</option>
        <optgroup label="g" disabled><option value="x">X</option></optgroup><option value="y">Y</option>
        <option value="z" disabled selected>Z</option></select></form>"#,
    );
    // 無効な選択肢 z が selected なので、選ばれたまま送信からは外れる（y を既定にしない）。
    assert_eq!(out.fields[0].kind, FieldKind::Select { values: vec![] });
    assert!(out.text.contains(r#"value="z""#), "{}", out.text);
    assert_eq!(
        out.elements[0].kind,
        ElementKind::Select {
            options: vec![("y".to_string(), "Y".to_string())],
            multiple: false,
        }
    );
    assert!(out.text.contains(r#""z"=Z(無効)"#), "{}", out.text);
}

#[test]
fn first_legend_of_disabled_fieldset_stays_enabled() {
    let out = r(
        r#"<form><fieldset disabled><legend><input name="in_legend" aria-label="凡例の中"></legend>
        <input name="body" aria-label="本体"><legend><input name="second" aria-label="二つ目の凡例"></legend></fieldset></form>"#,
    );
    let disabled: Vec<_> = out
        .fields
        .iter()
        .map(|f| (f.name.as_str(), f.disabled))
        .collect();
    assert_eq!(
        disabled,
        [("in_legend", false), ("body", true), ("second", true)]
    );
}

#[test]
fn no_selected_option_defaults_to_first_enabled() {
    let out = r(
        r#"<form><select name="s"><option value="" disabled>選んでください</option><option value="a">A</option></select></form>"#,
    );
    assert_eq!(
        out.fields[0].kind,
        FieldKind::Select {
            values: vec!["a".into()]
        }
    );
}

#[test]
fn disabled_and_enabled_options_with_the_same_value_are_told_apart() {
    // 無効な選択肢と同じ値の有効な選択肢が selected なら、その値は送る。
    let out = r(
        r#"<form><select name="s"><option value="x" disabled>無効</option><option value="x" selected>有効</option></select></form>"#,
    );
    assert_eq!(
        out.fields[0].kind,
        FieldKind::Select {
            values: vec!["x".into()]
        }
    );
    // 逆に無効な側が selected なら送らない。
    let out = r(
        r#"<form><select name="s"><option value="x" disabled selected>無効</option><option value="x">有効</option></select></form>"#,
    );
    assert_eq!(out.fields[0].kind, FieldKind::Select { values: vec![] });
}

#[test]
fn preformatted_text_keeps_line_breaks_and_indentation() {
    let out = r("<body><p>前</p><pre>if ready:\n    save()\n\tdone()\nfinish()</pre><p>後  の   文</p></body>");
    assert!(
        out.text
            .contains("if ready:\n    save()\n\tdone()\nfinish()"),
        "{}",
        out.text
    );
    // pre の外は従来どおり空白を詰める。
    assert!(out.text.contains("後 の 文"), "{}", out.text);
}

#[test]
fn radio_groups_keep_only_the_last_checked_and_list_boxes_start_empty() {
    let out = r(
        r#"<form id="f"><input type="radio" name="r" value="a" checked aria-label="A">
        <input type="radio" name="r" value="b" checked aria-label="B">
        <input type="radio" name="other" value="x" checked aria-label="X">
        <select name="list" size="3" aria-label="一覧"><option value="a">A</option><option value="b">B</option></select>
        <select name="one" size="1" aria-label="一つ"><option value="a">A</option></select></form>
        <form><input type="radio" name="r" value="c" checked aria-label="別フォーム"></form>
        <input type="radio" name="r" value="d" checked form="f" aria-label="外から"></body>"#,
    );
    let checked: Vec<_> = out
        .fields
        .iter()
        .filter_map(|f| match &f.kind {
            FieldKind::Check {
                value,
                checked: true,
                ..
            } => Some((f.form, value.as_str())),
            _ => None,
        })
        .collect();
    // フォーム f の r は、form 属性で外から加わった最後の d だけ。別グループ（other）・別フォームはそのまま。
    assert_eq!(
        checked,
        [(Some(0), "x"), (Some(1), "c"), (Some(0), "d")],
        "{}",
        out.text
    );
    assert!(out.text.contains(r#""A" checked=false"#), "{}", out.text);
    // size が 2 以上の単一選択リストは何も選ばない。size=1 は従来どおり先頭。
    assert_eq!(out.fields[3].kind, FieldKind::Select { values: vec![] });
    assert_eq!(
        out.fields[4].kind,
        FieldKind::Select {
            values: vec!["a".into()]
        }
    );
}

#[test]
fn required_select_value_missing_follows_placeholder_rules() {
    let out = r(r#"<form>
        <select name="a" required><option value="">選んでください</option><option value="x">X</option></select>
        <select name="b" required size="3"><option value="" selected>すべて</option><option value="x">X</option></select>
        <select name="c" required size="3"><option value="x">X</option></select>
        <select name="d" required><optgroup label="g"><option value="">空</option></optgroup><option value="x">X</option></select>
        </form>"#);
    let missing: Vec<_> = out
        .fields
        .iter()
        .map(|f| (f.name.as_str(), f.value_missing))
        .collect();
    // a: placeholder だけが選ばれている。b: 表示行数 3 のリストで空文字の選択肢を選ぶのは正当。
    // c: 何も選ばれていない。d: 親が optgroup の選択肢は placeholder ではない。
    assert_eq!(
        missing,
        [("a", true), ("b", false), ("c", true), ("d", false)]
    );
}

#[test]
fn radio_groups_required_by_a_disabled_member_and_charset_fallback() {
    let out = r(r#"<form method="post" action="/x" accept-charset="bogus">
        <input type="radio" name="g" value="a" required disabled aria-label="無効">
        <input type="radio" name="g" value="b" aria-label="有効"><button>送る</button></form>
        <form action="/y"><input name="q"></form>"#);
    // 無効なラジオにだけ required があっても、グループは必須（有効な b が未選択なので送らない）。
    let err = crate::text::submit::build(
        &out.forms,
        &out.fields,
        &base(),
        0,
        None,
        encoding_rs::SHIFT_JIS,
    )
    .unwrap_err();
    assert!(matches!(err, crate::engine::BrowserError::Required(n) if n == "g"));
    // accept-charset があるのに対応する候補が無ければ UTF-8。属性が無ければ文書の文字コード（None）。
    assert_eq!(out.forms[0].accept_charset, Some(encoding_rs::UTF_8));
    assert_eq!(out.forms[1].accept_charset, None);
    // 名前と値の改行は CRLF にそろえる。
    let pairs = vec![("a\nb".to_string(), "x\ry\r\nz".to_string())];
    assert_eq!(
        crate::text::submit::urlencoded(&pairs, encoding_rs::UTF_8),
        "a%0D%0Ab=x%0D%0Ay%0D%0Az"
    );
}

#[test]
fn many_selected_options_render_in_linear_time() {
    let html = format!(
        "<form><select multiple name=\"m\">{}</select></form>",
        "<option selected>x".repeat(50_000)
    );
    let started = std::time::Instant::now();
    let out = r(&html);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert!(matches!(&out.fields[0].kind, FieldKind::Select { values } if values.len() == 50_000));
}

#[test]
fn datalist_controls_are_not_submitted_and_option_labels_are_shown() {
    let out = r(
        r#"<form><input name="city" list="cities" value="Osaka" aria-label="都市">
        <datalist id="cities"><select name="city"><option>Tokyo</option></select></datalist>
        <select name="pref" aria-label="県"><option value="1" label="東京"></option><option value="2" label=" "> 大阪 </option></select></form>"#,
    );
    let names: Vec<_> = out.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["city", "pref"], "datalist の中の部品は送らない");
    // 空でない label 属性が表示名。空白だけの label は使わず中のテキスト。値は value 属性。
    assert_eq!(
        out.elements[1].kind,
        ElementKind::Select {
            options: vec![
                ("1".to_string(), "東京".to_string()),
                ("2".to_string(), "大阪".to_string())
            ],
            multiple: false,
        }
    );
}

#[test]
fn form_attribute_binding_is_linear() {
    let html = format!(
        "<body>{}{}</body>",
        "<form id=f></form>".repeat(20_000),
        "<input form=z>".repeat(20_000)
    );
    let started = std::time::Instant::now();
    let out = r(&html);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert!(out.fields.iter().all(|f| f.form.is_none()));
}

#[test]
fn long_base_href_and_huge_text_do_not_blow_up_memory() {
    // 長い base と多数の相対リンク: 巨大な URL をリンクごとに作らない（リンクにしない）。
    let html = format!(
        "<head><base href=\"https://example.com/{}/\"></head><body>{}</body>",
        "a".repeat(500_000),
        "<a href=\"x\">x</a>".repeat(3_000)
    );
    let out = r(&html);
    assert!(out.elements.is_empty());
    assert!(out.text.len() < 200_000, "{}", out.text.len());
    // 本文は描きながら上限で止める（全部描いてから切らない）。
    let big = format!("<body>{}</body>", "<p>あいうえお</p>".repeat(200_000));
    let out = render(&big, &base(), 1, 1_000, &HashMap::new());
    assert!(out.truncated);
    assert!(out.text.chars().count() <= 1_000 + 20, "{}", out.text.len());
}

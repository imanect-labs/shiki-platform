//! ノートの細かい AI 編集（#886）の検証（DB 不要・LiveDoc 単体）。
//!
//! 検証:
//! - 語句の差し替えは違う所だけを変え、書式と人の並行編集を残す
//! - 一意でない・見つからない語句は理由付きで適用しない
//! - 空にした段落・項目はブロックごと消え、表のセルは残る
//! - リストの途中への追加・項目の置き換え（チェック）
//! - 節を見出しごと消す・移す
//! - 提案モードは消す側に削除の提案を付け、元の本文を消さない
//! - 見出しの段は数値（Number）で書かれ、以前の BigInt は直せる

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::pedantic
)]

use collab::note::{EditMode, EditOp, EditReport};
use collab::{LiveDoc, PersistedDoc};
use uuid::Uuid;
use yrs::types::text::YChange;
use yrs::updates::decoder::Decode;
use yrs::{
    Any, Doc, GetString, Out, ReadTxn, Text, Transact, Update, Xml, XmlElementPrelim, XmlFragment,
    XmlOut, XmlTextPrelim,
};

const BASE: &str = "# 第3四半期 営業会議

## 概要

2026年10月20日に開催した。参加者は田中・鈴木の2名。

売上は**前年同期比で12%増加**した。

## 地域別の状況

| 地域 | 売上 |
| --- | --- |
| 東日本 | 4,200万円 |
| 九州 | 900万円 |

## 課題

- 西日本の代理店との連携が弱い
- 営業資料が古い

## 次のアクション

- [ ] 田中: 代理店を訪問する
- [ ] 鈴木: 紹介資料を作る

## 参考

前回の会議メモは別ノートを参照。
";

fn live(md: &str) -> LiveDoc {
    let live = LiveDoc::restore(
        Uuid::new_v4(),
        "default",
        Some(collab::DocKind::Note),
        &PersistedDoc {
            snapshot: None,
            snapshot_seq: 0,
            next_seq: 1,
            updates: vec![],
            saved_node_version: None,
            pending_save: false,
        },
    )
    .expect("restore");
    live.import_markdown(md).expect("seed");
    live
}

fn edit(live: &LiveDoc, ops: Vec<EditOp>, mode: EditMode) -> EditReport {
    live.apply_ai_edit(&ops, mode).expect("edit").1
}

fn md(live: &LiveDoc) -> String {
    live.to_markdown().expect("md")
}

/// サーバの全状態を持つ別クライアント（人の画面に相当）。
fn client_of(live: &LiveDoc) -> Doc {
    let doc = Doc::new();
    let full = live.full_state().expect("full");
    doc.transact_mut()
        .apply_update(Update::decode_v1(&full).expect("decode"))
        .expect("apply");
    doc
}

fn text(find: &str, replace: &str) -> EditOp {
    EditOp::ReplaceText {
        find: find.into(),
        replace: replace.into(),
        heading: None,
    }
}

#[test]
fn replace_text_changes_only_the_difference_and_keeps_bold() {
    let live = live(BASE);
    let report = edit(
        &live,
        vec![text("前年同期比で12%", "前年同期比で14%")],
        EditMode::Direct,
    );
    assert_eq!(report.applied, 1, "{:?}", report.skipped);
    let out = md(&live);
    assert!(out.contains("売上は**前年同期比で14%増加**した。"), "{out}");
    // 他は 1 文字も変わらない。
    assert_eq!(out.replace("14%", "12%"), md(&self::live(BASE)));
}

#[test]
fn replace_text_keeps_what_a_human_typed_in_the_same_paragraph() {
    let live = live(BASE);
    // 人が同じ段落の末尾に書き足す（AI の編集と同時）。
    let human = client_of(&live);
    let before = human.transact().state_vector();
    {
        let fragment = human.get_or_insert_xml_fragment("default");
        let mut txn = human.transact_mut();
        let Some(XmlOut::Element(para)) = fragment.get(&txn, 3) else {
            panic!("売上の段落");
        };
        let Some(XmlOut::Text(t)) = para.get(&txn, 0) else {
            panic!("テキスト");
        };
        let end = t.len(&txn);
        t.insert(&mut txn, end, "（速報値）");
    }
    let human_update = human.transact().encode_state_as_update_v1(&before);
    let report = edit(&live, vec![text("12%", "14%")], EditMode::Direct);
    assert_eq!(report.applied, 1);
    live.apply_update_bytes(&human_update).expect("merge");
    let out = md(&live);
    assert!(out.contains("前年同期比で14%増加"), "AI の訂正: {out}");
    assert!(out.contains("（速報値）"), "人の書き足しが残る: {out}");
}

#[test]
fn heading_markers_on_both_sides_are_dropped() {
    let live = live(BASE);
    let report = edit(
        &live,
        vec![text("## 課題", "## 主な課題")],
        EditMode::Direct,
    );
    assert_eq!(report.applied, 1, "{:?}", report.skipped);
    let out = md(&live);
    assert!(out.contains("\n## 主な課題\n"), "{out}");
}

#[test]
fn renaming_a_heading_is_a_text_replacement() {
    let live = live(BASE);
    let report = edit(&live, vec![text("課題", "主な課題")], EditMode::Direct);
    assert_eq!(report.applied, 1, "{:?}", report.skipped);
    let out = md(&live);
    assert!(out.contains("## 主な課題\n\n- 西日本の代理店"), "{out}");
}

#[test]
fn ambiguous_or_missing_text_is_not_applied_and_says_why() {
    let live = live(BASE);
    let report = edit(&live, vec![text("代理店", "販売店")], EditMode::Direct);
    assert_eq!(report.applied, 0);
    let why = &report.skipped[0];
    assert!(
        why.contains("1 番目の replace_text") && why.contains("2 箇所"),
        "{why}"
    );
    assert!(
        why.contains("「課題」の節") && why.contains("「次のアクション」の節"),
        "{why}"
    );
    assert_eq!(md(&live), md(&self::live(BASE)), "何も変えない");

    // 節で絞れば一意になる。
    let report = edit(
        &live,
        vec![EditOp::ReplaceText {
            find: "代理店".into(),
            replace: "販売店".into(),
            heading: Some("次のアクション".into()),
        }],
        EditMode::Direct,
    );
    assert_eq!(report.applied, 1, "{:?}", report.skipped);
    assert!(md(&live).contains("田中: 販売店を訪問する"));

    let report = edit(
        &live,
        vec![
            text("存在しない語句", "x"),
            text("2名。\n\n売上は", "x"),
            text("12%", "14%\n15%"),
        ],
        EditMode::Direct,
    );
    assert_eq!(report.applied, 0);
    assert!(
        report.skipped[1].contains("1 つの段落"),
        "{:?}",
        report.skipped
    );
    assert!(report.skipped[2].contains("改行"), "{:?}", report.skipped);
    assert!(
        report.skipped[0].contains("見つからない"),
        "{:?}",
        report.skipped
    );
}

#[test]
fn markdown_symbols_copied_from_document_read_still_match() {
    let live = live(BASE);
    let report = edit(
        &live,
        vec![text(
            "- [ ] 鈴木: 紹介資料を作る",
            "鈴木: 新製品の紹介資料を作る",
        )],
        EditMode::Direct,
    );
    assert_eq!(report.applied, 1, "{:?}", report.skipped);
    assert!(md(&live).contains("- [ ] 鈴木: 新製品の紹介資料を作る"));
}

#[test]
fn emptied_paragraphs_and_items_go_away_but_table_cells_stay() {
    let live = live(BASE);
    let report = edit(
        &live,
        vec![
            text("前回の会議メモは別ノートを参照。", ""),
            text("営業資料が古い", ""),
            text("900万円", ""),
        ],
        EditMode::Direct,
    );
    assert_eq!(report.applied, 3, "{:?}", report.skipped);
    let out = md(&live);
    assert!(out.ends_with("## 参考\n"), "段落ごと消える: {out}");
    assert!(
        out.contains("- 西日本の代理店との連携が弱い\n\n## 次のアクション"),
        "{out}"
    );
    assert!(out.contains("| 九州 |  |"), "セルは空で残る: {out}");
}

#[test]
fn insert_after_a_list_item_joins_the_same_list() {
    let live = live(BASE);
    let report = edit(
        &live,
        vec![
            EditOp::InsertAfter {
                find: "西日本の代理店との連携が弱い".into(),
                markdown: "- 価格競争が激しい\n".into(),
                heading: None,
            },
            EditOp::InsertAfter {
                find: "参加者は田中・鈴木の2名。".into(),
                markdown: "司会は田中が務めた。\n".into(),
                heading: None,
            },
        ],
        EditMode::Direct,
    );
    assert_eq!(report.applied, 2, "{:?}", report.skipped);
    let out = md(&live);
    assert!(
        out.contains("- 西日本の代理店との連携が弱い\n- 価格競争が激しい\n- 営業資料が古い"),
        "{out}"
    );
    assert!(
        out.contains("2名。\n\n司会は田中が務めた。\n\n売上は"),
        "{out}"
    );
}

#[test]
fn replace_block_on_a_task_replaces_only_that_item() {
    let live = live(BASE);
    let report = edit(
        &live,
        vec![EditOp::ReplaceBlock {
            find: "鈴木: 紹介資料を作る".into(),
            markdown: "- [x] 鈴木: 紹介資料を作る\n".into(),
            heading: None,
        }],
        EditMode::Direct,
    );
    assert_eq!(report.applied, 1, "{:?}", report.skipped);
    let out = md(&live);
    assert!(
        out.contains("- [ ] 田中: 代理店を訪問する\n- [x] 鈴木: 紹介資料を作る\n"),
        "{out}"
    );
}

#[test]
fn delete_and_move_sections_with_their_headings() {
    let live = live(BASE);
    let report = edit(
        &live,
        vec![
            EditOp::DeleteSection {
                heading: "参考".into(),
            },
            EditOp::MoveSection {
                heading: "次のアクション".into(),
                before_heading: Some("課題".into()),
            },
        ],
        EditMode::Direct,
    );
    assert_eq!(report.applied, 2, "{:?}", report.skipped);
    let out = md(&live);
    assert!(
        !out.contains("参考") && !out.contains("前回の会議メモ"),
        "{out}"
    );
    let actions = out.find("## 次のアクション").unwrap();
    let issues = out.find("## 課題").unwrap();
    assert!(actions < issues, "{out}");
    assert!(out.trim_end().ends_with("- 営業資料が古い"), "{out}");

    // 末尾へ移す・自分の節の中へは移せない。
    let report = edit(
        &live,
        vec![
            EditOp::MoveSection {
                heading: "概要".into(),
                before_heading: None,
            },
            EditOp::MoveSection {
                heading: "第3四半期 営業会議".into(),
                before_heading: Some("課題".into()),
            },
        ],
        EditMode::Direct,
    );
    // 1 つでも適用できなければ、何も変えない（全部か無しか）。
    let before = md(&live);
    assert_eq!(report.applied, 0);
    assert!(report.skipped[0].contains("2 番目") && report.skipped[0].contains("節の中"));
    assert_eq!(md(&live), before, "1 番目の移動も適用しない");

    let report = edit(
        &live,
        vec![EditOp::MoveSection {
            heading: "概要".into(),
            before_heading: None,
        }],
        EditMode::Direct,
    );
    assert_eq!(report.applied, 1);
    assert!(md(&live)
        .trim_end()
        .ends_with("前年同期比で12%増加**した。"));
}

#[test]
fn duplicate_headings_need_the_parent() {
    let live =
        live("# 10/1\n\n## 決定事項\n\n田中が担当。\n\n# 10/8\n\n## 決定事項\n\n田中が担当。\n");
    let scoped = |heading: &str| EditOp::ReplaceText {
        find: "田中".into(),
        replace: "鈴木".into(),
        heading: Some(heading.into()),
    };
    let report = edit(&live, vec![scoped("決定事項")], EditMode::Direct);
    assert_eq!(report.applied, 0);
    assert!(
        report.skipped[0].contains("2 個ある"),
        "{:?}",
        report.skipped
    );
    let report = edit(&live, vec![scoped("10/8 > 決定事項")], EditMode::Direct);
    assert_eq!(report.applied, 1, "{:?}", report.skipped);
    let out = md(&live);
    assert!(
        out.contains("# 10/1\n\n## 決定事項\n\n田中が担当。"),
        "{out}"
    );
    assert!(
        out.contains("# 10/8\n\n## 決定事項\n\n鈴木が担当。"),
        "{out}"
    );
}

/// 段落の中の、提案の印ごとの文字列。
fn runs(doc: &Doc, index: u32) -> Vec<(String, bool, bool)> {
    let fragment = doc.get_or_insert_xml_fragment("default");
    let txn = doc.transact();
    let Some(XmlOut::Element(el)) = fragment.get(&txn, index) else {
        panic!("{index} 番目のブロック");
    };
    let Some(XmlOut::Text(t)) = el.get(&txn, 0) else {
        panic!("テキスト");
    };
    t.diff(&txn, YChange::identity)
        .into_iter()
        .map(|d| {
            let Out::Any(Any::String(s)) = d.insert else {
                panic!("文字列");
            };
            let has = |k: &str| {
                d.attributes
                    .as_ref()
                    .is_some_and(|a| matches!(a.get(k), Some(Any::Bool(true))))
            };
            (s.to_string(), has("aiDeletion"), has("aiSuggestion"))
        })
        .collect()
}

#[test]
fn suggest_mode_marks_deletions_instead_of_removing() {
    let live = live(BASE);
    let report = edit(&live, vec![text("12%", "14%")], EditMode::Suggest);
    assert_eq!(report.applied, 1);
    let doc = client_of(&live);
    let r = runs(&doc, 3);
    assert!(r.contains(&("12".into(), true, false)), "消す側の印: {r:?}");
    assert!(
        r.contains(&("14".into(), false, true)),
        "入れる側の印: {r:?}"
    );

    // 節の削除・置換も、元の本文に印を付けるだけで消さない。
    let report = edit(
        &live,
        vec![
            EditOp::DeleteSection {
                heading: "参考".into(),
            },
            EditOp::ReplaceSection {
                heading: "課題".into(),
                markdown: "- 新しい課題\n".into(),
            },
        ],
        EditMode::Suggest,
    );
    assert_eq!(report.applied, 2, "{:?}", report.skipped);
    // md（保存・document.read）は「承認したら」の形で、旧文と新文が並ばない。
    let out = md(&live);
    assert!(
        out.contains("前年同期比で14%増加") && !out.contains("1214"),
        "{out}"
    );
    assert!(
        !out.contains("前回の会議メモ")
            && !out.contains("## 参考")
            && !out.contains("営業資料が古い"),
        "{out}"
    );
    assert!(out.contains("新しい課題"), "{out}");
    let doc = client_of(&live);
    let fragment = doc.get_or_insert_xml_fragment("default");
    let txn = doc.transact();
    let last = fragment.len(&txn) - 1;
    let Some(XmlOut::Element(el)) = fragment.get(&txn, last) else {
        panic!("参考の段落");
    };
    let Some(XmlOut::Text(t)) = el.get(&txn, 0) else {
        panic!("テキスト");
    };
    assert!(t.diff(&txn, YChange::identity).iter().all(|d| d
        .attributes
        .as_ref()
        .is_some_and(|a| a.contains_key("aiDeletion"))));
}

#[test]
fn headings_are_written_as_numbers_and_old_bigints_are_repaired() {
    let live = live("## 見出し\n\n3. 三\n4. 四\n");
    let doc = client_of(&live);
    let fragment = doc.get_or_insert_xml_fragment("default");
    {
        let txn = doc.transact();
        let Some(XmlOut::Element(h)) = fragment.get(&txn, 0) else {
            panic!("見出し");
        };
        assert_eq!(
            h.get_attribute(&txn, "level"),
            Some(Out::Any(Any::Number(2.0)))
        );
        let Some(XmlOut::Element(ol)) = fragment.get(&txn, 1) else {
            panic!("リスト");
        };
        assert_eq!(
            ol.get_attribute(&txn, "start"),
            Some(Out::Any(Any::Number(3.0)))
        );
    }

    // 以前の書き込み（BigInt）を直す。
    let old = Doc::new();
    let fragment = old.get_or_insert_xml_fragment("default");
    {
        let mut txn = old.transact_mut();
        let h = fragment.insert(&mut txn, 0, XmlElementPrelim::empty("heading"));
        h.insert_attribute(&mut txn, "level", Any::BigInt(3));
        h.insert(&mut txn, 0, XmlTextPrelim::new("古い見出し"));
        let fixed = collab::note::yjs_write::repair_number_attrs(&mut txn, &fragment);
        assert_eq!(fixed, 1);
        assert_eq!(
            h.get_attribute(&txn, "level"),
            Some(Out::Any(Any::Number(3.0)))
        );
        assert_eq!(
            collab::note::yjs_write::repair_number_attrs(&mut txn, &fragment),
            0
        );
    }
    assert!(fragment.get_string(&old.transact()).contains("古い見出し"));
}

#[test]
fn find_matches_the_text_as_it_reads_with_pending_suggestions() {
    let live = live(BASE);
    edit(&live, vec![text("12%", "14%")], EditMode::Suggest);
    // 見えている文は「14%増加」。消す提案の「12」は探す文字に入らない。
    let report = edit(&live, vec![text("14%増加", "15%増加")], EditMode::Direct);
    assert_eq!(report.applied, 1, "{:?}", report.skipped);
    assert!(md(&live).contains("前年同期比で15%増加"), "{}", md(&live));
}

#[test]
fn textless_blocks_get_the_flag_and_pending_sections_do_not_move() {
    let live = live(&BASE.replace(
        "前回の会議メモは別ノートを参照。",
        "前回の会議メモは別ノートを参照。\n\n---",
    ));
    let report = edit(
        &live,
        vec![EditOp::DeleteSection {
            heading: "参考".into(),
        }],
        EditMode::Suggest,
    );
    assert_eq!(report.applied, 1);
    let doc = client_of(&live);
    {
        let fragment = doc.get_or_insert_xml_fragment("default");
        let txn = doc.transact();
        let last = fragment.len(&txn) - 1;
        let Some(XmlOut::Element(hr)) = fragment.get(&txn, last) else {
            panic!("区切り線");
        };
        assert_eq!(&*hr.tag().to_string(), "horizontalRule");
        assert_eq!(
            hr.get_attribute(&txn, "aiDeletion"),
            Some(Out::Any(Any::Bool(true)))
        );
    }
    let out = md(&live);
    assert!(
        !out.contains("\n---\n") && !out.contains("## 参考"),
        "{out}"
    );

    // 消す提案だけの見出しは見えないので、もう操作の対象にならない。
    let report = edit(
        &live,
        vec![EditOp::MoveSection {
            heading: "参考".into(),
            before_heading: None,
        }],
        EditMode::Direct,
    );
    assert!(
        report.skipped[0].contains("見つからない"),
        "{:?}",
        report.skipped
    );

    // 未決の提案がある節は、移すと印が落ちて黙って確定するので移さない。
    edit(&live, vec![text("12%", "14%")], EditMode::Suggest);
    let report = edit(
        &live,
        vec![EditOp::MoveSection {
            heading: "概要".into(),
            before_heading: None,
        }],
        EditMode::Direct,
    );
    assert_eq!(report.applied, 0);
    assert!(
        report.skipped[0].contains("未決の提案"),
        "{:?}",
        report.skipped
    );
}

/// 人がその段落を編集すると、y-prosemirror は提案の印をマークの属性（`{}`）で書き直す。
/// その形でも削除の提案として扱う（md に旧文を戻さない・検索に旧文を出さない）。
#[test]
fn deletion_marks_rewritten_by_the_editor_still_count() {
    use std::collections::HashMap;
    use std::sync::Arc;
    let live = live(BASE);
    edit(&live, vec![text("12%", "14%")], EditMode::Suggest);
    let human = client_of(&live);
    let before = human.transact().state_vector();
    {
        let fragment = human.get_or_insert_xml_fragment("default");
        let mut txn = human.transact_mut();
        let Some(XmlOut::Element(para)) = fragment.get(&txn, 3) else {
            panic!("売上の段落");
        };
        let Some(XmlOut::Text(t)) = para.get(&txn, 0) else {
            panic!("テキスト");
        };
        // 「12」（削除の提案）の印を `{}` に書き直す。
        let plain: String = t
            .diff(&txn, YChange::identity)
            .into_iter()
            .filter_map(|d| match d.insert {
                Out::Any(Any::String(s)) => Some(s.to_string()),
                _ => None,
            })
            .collect();
        let start = plain.find("1214").unwrap() as u32;
        let mut attrs: yrs::types::Attrs = HashMap::new();
        attrs.insert(
            Arc::from("aiDeletion"),
            Any::from(HashMap::<String, Any>::new()),
        );
        t.format(&mut txn, start, 2, attrs);
    }
    let update = human.transact().encode_state_as_update_v1(&before);
    live.apply_update_bytes(&update).expect("merge");
    let out = md(&live);
    assert!(
        out.contains("前年同期比で14%増加") && !out.contains("1214"),
        "{out}"
    );
    let report = edit(&live, vec![text("1214", "x")], EditMode::Direct);
    assert!(
        report.skipped[0].contains("見つからない"),
        "{:?}",
        report.skipped
    );
}

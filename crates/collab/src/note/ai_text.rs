//! 語句を手がかりにした AI 編集（#886）。
//!
//! 見出しを手がかりにした操作（[`super::ai_edit`]）は節を丸ごと書き直すので、1 語の訂正でも
//! 人がその節で書いている内容・提案マーク・カーソル位置を巻き込む。ここでは本文の語句で
//! 場所を決め、**変わる所だけ**を Yjs で変える:
//!
//! - `replace_text`: 1 ブロック内の語句を差し替える（前後の共通部分は残し、差分だけを消して
//!   入れる。書式は消した位置の書式を引き継ぐ）。空に差し替えて段落・項目が空になるなら
//!   ブロックごと消す。
//! - `replace_block`: 語句を含む最上位のブロック（段落・見出し・リスト全体・表全体など）を
//!   Markdown で置き換える。
//! - `insert_after`: 語句を含むブロックの直後へ Markdown を入れる。リストの項目の後に
//!   同じ種類のリストを入れるなら、同じリストの項目として入れる。
//!
//! 語句は**文書全体（`heading` を渡せばその節）で一意**でなければ適用しない（取り違えを
//! 防ぐ）。提案モードでは消す側に `aiDeletion`、入れる側に `aiSuggestion` の印を付け、
//! 人が承認するまで元の本文を消さない。

use std::collections::HashMap;
use std::sync::Arc;

use yrs::types::text::YChange;
use yrs::types::Attrs;
use yrs::{
    Any, Out, Text, TransactionMut, XmlElementRef, XmlFragment, XmlFragmentRef, XmlOut, XmlTextRef,
};

use super::ai_edit::EditMode;
use super::ast::Block;
use super::md_parse::parse_markdown;
use super::sections::{section, visible_text};
use super::yjs_map::is_on;
use super::yjs_write::{insert_blocks_at, insert_list_items_at, mark_deleted};

/// 提案の印（書式として持つ・md には落とさない）。
pub(super) const SUGGESTION: &str = "aiSuggestion";
pub(super) const DELETION: &str = "aiDeletion";

/// ブロックの親（本文のフラグメントか要素）。
#[derive(Clone)]
enum Parent {
    Root(XmlFragmentRef),
    Element(XmlElementRef),
}

impl Parent {
    fn len(&self, txn: &TransactionMut<'_>) -> u32 {
        match self {
            Parent::Root(f) => f.len(txn),
            Parent::Element(e) => e.len(txn),
        }
    }
    fn remove(&self, txn: &mut TransactionMut<'_>, index: u32) {
        match self {
            Parent::Root(f) => f.remove_range(txn, index, 1),
            Parent::Element(e) => e.remove_range(txn, index, 1),
        }
    }
    fn tag(&self) -> Option<String> {
        match self {
            Parent::Root(_) => None,
            Parent::Element(e) => Some(e.tag().to_string()),
        }
    }
}

/// テキストの 1 区切り（強制改行で分かれる）。
///
/// 探すのは人と AI に見えている文字（`plain`・削除の提案が付いた文字を除く）で、Yjs を
/// 変えるときは実際の位置（`offs` で写す）を使う。書式の切れ目は実際の位置で持つ。
#[derive(Clone)]
struct Segment {
    text: XmlTextRef,
    plain: String,
    /// 見えている文字の n バイト目の、実際の位置（末尾に全体の長さを足して len+1 個）。
    offs: Vec<usize>,
    runs: Vec<(usize, usize, Attrs)>,
}

impl Segment {
    fn read(txn: &TransactionMut<'_>, text: XmlTextRef) -> Option<Self> {
        let mut plain = String::new();
        let mut offs = Vec::new();
        let mut runs = Vec::new();
        let mut actual = 0usize;
        for diff in text.diff(txn, YChange::identity) {
            // 文字列以外（埋め込み）は長さの数え方が違うので、この区切りでは探さない。
            let Out::Any(Any::String(s)) = diff.insert else {
                return None;
            };
            let attrs = diff.attributes.map(|a| *a).unwrap_or_default();
            let deleted = is_on(attrs.get(DELETION));
            if !deleted {
                plain.push_str(&s);
                offs.extend(actual..actual + s.len());
            }
            runs.push((actual, actual + s.len(), attrs));
            actual += s.len();
        }
        offs.push(actual);
        Some(Segment {
            text,
            plain,
            offs,
            runs,
        })
    }

    /// 見えている文字の `v` バイト目から始まる範囲の、実際の始まり。
    fn start_of(&self, v: usize) -> usize {
        self.offs[v]
    }

    /// 見えている文字の `v` バイト目で終わる範囲の、実際の終わり（後ろの削除の提案は含めない）。
    fn end_of(&self, v: usize) -> usize {
        if v == 0 {
            self.offs[0]
        } else {
            self.offs[v - 1] + 1
        }
    }

    /// `at` バイト目の文字の書式（範囲外は空）。
    fn attrs_at(&self, at: usize) -> Attrs {
        self.runs
            .iter()
            .find(|(s, e, _)| *s <= at && at < *e)
            .map(|(_, _, a)| a.clone())
            .unwrap_or_default()
    }
}

/// 語句の一致 1 件。
struct Hit {
    /// フラグメント直下の何番目のブロックの中か。
    top: u32,
    /// フラグメントから、語句を含む段落・見出しまでの（親, 位置）。
    path: Vec<(Parent, u32)>,
    segment: Segment,
    /// 区切りの中の一致の開始（バイト）と、一致した語句。
    start: usize,
    found: String,
    /// 一致がその段落・見出しの文字全体か（強制改行も他の要素も無い）。
    whole_block: bool,
    /// Markdown の記号を外した語句で一致したか（差し替える側も同じく外す）。
    bare: bool,
    /// コードブロックの中か（コードの文字は書式＝提案の印を持てない）。
    in_code: bool,
}

/// 重なりも含めた一致の開始位置（「ababa」の「aba」は 0 と 2 の 2 件）。一意かどうかを
/// 数えるので、`match_indices`（重ならない一致だけ）では足りない。
fn overlapping_matches(hay: &str, needle: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(p) = hay[from..].find(needle) {
        let at = from + p;
        out.push(at);
        from = at + hay[at..].chars().next().map_or(1, char::len_utf8);
    }
    out
}

/// 語句を含む段落・見出し・コードを、文書（または節）から 1 つだけ探す。
fn find_unique(
    txn: &TransactionMut<'_>,
    fragment: &XmlFragmentRef,
    find: &str,
    heading: Option<&str>,
) -> Result<Hit, String> {
    if find.trim().is_empty() {
        return Err("find が空".into());
    }
    if find.trim().contains('\n') {
        return Err(
            "find は 1 つの段落・項目の中の語句にする（複数の段落は replace_block / replace_section）"
                .into(),
        );
    }
    let (from, to) = match heading {
        Some(h) => section(txn, fragment, h)?,
        None => (0, fragment.len(txn)),
    };
    // 本文の文字どおりで探し、無ければ Markdown の記号（`**`・行頭の `- ` 等）を外して探す。
    let mut needles = vec![find.to_string()];
    let bare = strip_markdown(find);
    if !bare.is_empty() && bare != find {
        needles.push(bare);
    }
    // 最上位のブロックは 1 回の走査で辿る（`get(i)` は先頭から数え直すので O(n²) になる）。
    let tops: Vec<(u32, XmlElementRef)> = fragment
        .children(txn)
        .enumerate()
        .skip(from as usize)
        .take((to - from) as usize)
        .filter_map(|(i, c)| match c {
            XmlOut::Element(el) => Some((i as u32, el)),
            _ => None,
        })
        .collect();
    let root = Parent::Root(fragment.clone());
    for (i, needle) in needles.iter().enumerate() {
        let mut found = Found::default();
        for (top, el) in &tops {
            let mut path = vec![(root.clone(), *top)];
            collect(txn, el, &mut path, *top, needle, &mut found);
        }
        match (found.first, found.tops.len()) {
            (None, _) => {}
            (Some(mut hit), 1) => {
                hit.bare = i > 0;
                return Ok(hit);
            }
            (Some(_), n) => {
                let mut places: Vec<String> = sections_of(txn, fragment, &found.tops)
                    .into_iter()
                    .map(|h| match h {
                        Some(s) => format!("「{s}」の節"),
                        None => "最初の見出しより前".into(),
                    })
                    .collect();
                places.dedup();
                return Err(format!(
                    "「{find}」が {n} 箇所にある（{}）。前後の語を足して一意にするか、heading で節を絞る",
                    places.join("・")
                ));
            }
        }
    }
    Err(format!(
        "「{find}」が見つからない（document.read で今の本文を確かめ、本文の文字どおりに指定する）"
    ))
}

/// 一致の集計。中身まで持つのは最初の 1 件だけ（一意でなければ使わないので、残りは
/// 場所だけを数える。一致のたびに段落の全文を複製しない）。
#[derive(Default)]
struct Found {
    first: Option<Hit>,
    /// 一致ごとの、フラグメント直下の位置。
    tops: Vec<u32>,
}

/// 要素の中の段落・見出し・コードを辿り、`needle` の一致を集める。
fn collect(
    txn: &TransactionMut<'_>,
    el: &XmlElementRef,
    path: &mut Vec<(Parent, u32)>,
    top: u32,
    needle: &str,
    found: &mut Found,
) {
    let tag = el.tag().to_string();
    match tag.as_str() {
        "paragraph" | "heading" | "codeBlock" => {
            let in_code = tag == "codeBlock";
            let children: Vec<XmlOut> = el.children(txn).collect();
            let single = children.len() == 1;
            for child in children {
                let XmlOut::Text(text) = child else {
                    continue;
                };
                let Some(segment) = Segment::read(txn, text) else {
                    continue;
                };
                for start in overlapping_matches(&segment.plain, needle) {
                    found.tops.push(top);
                    if found.first.is_none() {
                        found.first = Some(Hit {
                            top,
                            path: path.clone(),
                            whole_block: single && needle.len() == segment.plain.len(),
                            start,
                            found: needle.to_string(),
                            segment: segment.clone(),
                            bare: false,
                            in_code,
                        });
                    }
                }
            }
        }
        _ => {
            let parent = Parent::Element(el.clone());
            for (i, child) in el.children(txn).enumerate() {
                if let XmlOut::Element(inner) = child {
                    path.push((parent.clone(), i as u32));
                    collect(txn, &inner, path, top, needle, found);
                    path.pop();
                }
            }
        }
    }
}

/// 1 ブロック内の語句を差し替える。
pub(super) fn replace_text(
    txn: &mut TransactionMut<'_>,
    fragment: &XmlFragmentRef,
    find: &str,
    replace: &str,
    heading: Option<&str>,
    mode: EditMode,
) -> Result<(), String> {
    if replace.contains('\n') {
        return Err("replace に改行は入れられない（段落を分けるなら replace_block）".into());
    }
    let hit = find_unique(txn, fragment, find, heading)?;
    // `## 課題` → `## 主な課題` のように記号ごと渡されたら、入れる側の記号も外す
    // （外さないと見出しの文字に `## ` が入る）。
    let bare;
    let replace = if hit.bare {
        bare = strip_markdown(replace);
        bare.as_str()
    } else {
        replace
    };
    let old = hit.found.as_str();
    if old == replace {
        return Ok(());
    }
    let suggest = mode == EditMode::Suggest;
    if suggest && hit.in_code {
        return Err(
            "コードブロックの中の語句は提案では差し替えられない（replace_block でブロックごと提案する）"
                .into(),
        );
    }
    if replace.is_empty() && hit.whole_block && !suggest {
        remove_enclosing(txn, &hit);
        return Ok(());
    }
    // 前後の共通部分は残し、違う所だけを消して入れる（書式と並行編集を巻き込まない）。
    // 提案は人が読んで判断するので、数字・英字の途中では切らない（「1~~2~~4%」ではなく
    // 「~~12~~14%」と見せる）。
    let (prefix, suffix) = common_affix(old, replace);
    let (prefix, suffix) = if suggest {
        widen_to_words(old, replace, prefix, suffix)
    } else {
        (prefix, suffix)
    };
    let seg = &hit.segment;
    let del_start = seg.start_of(hit.start + prefix);
    let del_end = seg.end_of(hit.start + old.len() - suffix).max(del_start);
    let del_len = del_end - del_start;
    let ins = &replace[prefix..replace.len() - suffix];
    let mut fmt = if del_len > 0 {
        seg.attrs_at(del_start)
    } else if del_start > 0 {
        seg.attrs_at(del_start - 1)
    } else {
        seg.attrs_at(0)
    };
    fmt.remove(SUGGESTION);
    fmt.remove(DELETION);
    let text = &seg.text;
    let (at, left) = if suggest {
        if del_len > 0 {
            text.format(txn, offset(del_start), offset(del_len), flag(DELETION));
        }
        let at = del_start + del_len;
        let mut left = if at > 0 {
            seg.attrs_at(at - 1)
        } else {
            Attrs::new()
        };
        if del_len > 0 {
            left.insert(Arc::from(DELETION), Any::Bool(true));
        }
        fmt.insert(Arc::from(SUGGESTION), Any::Bool(true));
        (at, left)
    } else {
        if del_len > 0 {
            text.remove_range(txn, offset(del_start), offset(del_len));
        }
        let left = if del_start > 0 {
            seg.attrs_at(del_start - 1)
        } else {
            Attrs::new()
        };
        (del_start, left)
    };
    if !ins.is_empty() {
        // 左隣の書式が引き継がれないよう、入れる書式に無いものは明示的に外す。
        for key in left.keys() {
            fmt.entry(key.clone()).or_insert(Any::Null);
        }
        text.insert_with_attributes(txn, offset(at), ins, fmt);
    }
    Ok(())
}

/// 語句を含む最上位のブロックを Markdown で置き換える。リストの項目の中で同じ種類のリストを
/// 渡したなら、その項目だけを置き換える（チェックを付ける・1 項目を書式付きで直す）。
pub(super) fn replace_block(
    txn: &mut TransactionMut<'_>,
    fragment: &XmlFragmentRef,
    find: &str,
    markdown: &str,
    heading: Option<&str>,
    mode: EditMode,
) -> Result<(), String> {
    let hit = find_unique(txn, fragment, find, heading)?;
    let blocks = parse_markdown(markdown);
    let suggest = mode == EditMode::Suggest;
    if let Some((list, idx)) = same_kind_list(&hit, &blocks) {
        if suggest {
            if let Some(XmlOut::Element(item)) = list.get(txn, idx) {
                mark_deleted(txn, &item);
            }
            insert_list_items_at(txn, &list, idx + 1, &blocks[0], true);
        } else {
            list.remove_range(txn, idx, 1);
            insert_list_items_at(txn, &list, idx, &blocks[0], false);
        }
        return Ok(());
    }
    if suggest {
        if let Some(XmlOut::Element(el)) = fragment.get(txn, hit.top) {
            mark_deleted(txn, &el);
        }
        insert_blocks_at(txn, fragment, hit.top + 1, &blocks, true);
    } else {
        fragment.remove_range(txn, hit.top, 1);
        insert_blocks_at(txn, fragment, hit.top, &blocks, false);
    }
    Ok(())
}

/// 一致が入っている一番内側のリストと項目の位置（`blocks` がそれと同じ種類のリスト 1 つのとき）。
fn same_kind_list(hit: &Hit, blocks: &[Block]) -> Option<(XmlElementRef, u32)> {
    let [block] = blocks else {
        return None;
    };
    let (list, idx) = hit
        .path
        .iter()
        .rev()
        .find_map(|(parent, idx)| match parent {
            Parent::Element(el) if is_list(el.tag()) => Some((el.clone(), *idx)),
            _ => None,
        })?;
    list_kind_matches(list.tag(), block).then_some((list, idx))
}

/// 語句を含むブロックの直後へ Markdown を入れる。
pub(super) fn insert_after(
    txn: &mut TransactionMut<'_>,
    fragment: &XmlFragmentRef,
    find: &str,
    markdown: &str,
    heading: Option<&str>,
    mode: EditMode,
) -> Result<(), String> {
    let hit = find_unique(txn, fragment, find, heading)?;
    let blocks = parse_markdown(markdown);
    if blocks.is_empty() {
        return Err("markdown が空".into());
    }
    let suggest = mode == EditMode::Suggest;
    // 一番内側のリストの項目の中なら、同じ種類のリストは同じリストの項目として続ける。
    if let Some((list, idx)) = same_kind_list(&hit, &blocks) {
        insert_list_items_at(txn, &list, idx + 1, &blocks[0], suggest);
        return Ok(());
    }
    insert_blocks_at(txn, fragment, hit.top + 1, &blocks, suggest);
    Ok(())
}

/// 空になる段落・項目を消す。中身がそれだけの項目・リスト・引用は、入れ物ごと消す。
fn remove_enclosing(txn: &mut TransactionMut<'_>, hit: &Hit) {
    let mut k = hit.path.len() - 1;
    loop {
        let (parent, idx) = &hit.path[k];
        let tag = parent.tag();
        let only_child = parent.len(txn) == 1;
        match tag.as_deref() {
            // 表のセルは空の段落を残す（表の形を崩さない）。
            Some("tableCell" | "tableHeader") => break,
            Some(
                "listItem" | "taskItem" | "blockquote" | "bulletList" | "orderedList" | "taskList",
            ) if only_child && k > 0 => {
                k -= 1;
            }
            // 項目の先頭の段落だけを消すと、入れ子のリストが項目の頭に来て形が崩れる。
            Some("listItem" | "taskItem") if *idx == 0 => break,
            _ => {
                parent.remove(txn, *idx);
                return;
            }
        }
    }
    let len = hit.segment.text.len(txn);
    hit.segment.text.remove_range(txn, 0, len);
}

fn is_list(tag: &str) -> bool {
    matches!(tag, "bulletList" | "orderedList" | "taskList")
}

fn list_kind_matches(tag: &str, block: &Block) -> bool {
    matches!(
        (tag, block),
        ("bulletList", Block::BulletList(_))
            | ("orderedList", Block::OrderedList { .. })
            | ("taskList", Block::TaskList(_))
    )
}

/// 数字・英字の続き（半角・全角）か。
fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || ('０'..='９').contains(&c)
        || c.is_ascii_digit()
        || ('Ａ'..='Ｚ').contains(&c)
        || ('ａ'..='ｚ').contains(&c)
}

/// 共通部分の境目が数字・英字の途中なら、その語の切れ目まで共通部分を縮める。
fn widen_to_words(a: &str, b: &str, mut prefix: usize, mut suffix: usize) -> (usize, usize) {
    let word_at = |s: &str, i: usize| s[i..].chars().next().is_some_and(is_word_char);
    while let Some(c) = a[..prefix].chars().next_back() {
        if !(is_word_char(c) && (word_at(a, prefix) || word_at(b, prefix))) {
            break;
        }
        prefix -= c.len_utf8();
    }
    let word_before = |s: &str, end: usize| s[..end].chars().next_back().is_some_and(is_word_char);
    while let Some(c) = a[a.len() - suffix..].chars().next() {
        if !(is_word_char(c)
            && (word_before(a, a.len() - suffix) || word_before(b, b.len() - suffix)))
        {
            break;
        }
        suffix -= c.len_utf8();
    }
    (prefix, suffix)
}

/// 文字の境界で、前後の共通部分の長さ（バイト）を返す（重ならない）。
fn common_affix(a: &str, b: &str) -> (usize, usize) {
    let prefix: usize = a
        .chars()
        .zip(b.chars())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x.len_utf8())
        .sum();
    let (ra, rb) = (&a[prefix..], &b[prefix..]);
    let suffix: usize = ra
        .chars()
        .rev()
        .zip(rb.chars().rev())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x.len_utf8())
        .sum();
    (prefix, suffix)
}

/// AI が document.read の Markdown から写した記号（行頭の `#`・`- [ ] `・`1. `・`> `、
/// 強調の `**` 等）を外す。本文の文字どおりで見つからないときだけ使う。
fn strip_markdown(s: &str) -> String {
    let mut t = s.trim();
    t = t.trim_start_matches('#').trim_start();
    t = t.strip_prefix("> ").unwrap_or(t);
    for marker in ["- [ ] ", "- [x] ", "- [X] ", "- ", "* ", "+ "] {
        if let Some(rest) = t.strip_prefix(marker) {
            t = rest;
            break;
        }
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 {
        if let Some(rest) = t[digits..].strip_prefix(". ") {
            t = rest;
        }
    }
    t.replace("**", "")
        .replace("__", "")
        .replace("~~", "")
        .replace('`', "")
}

/// 各 `tops`（昇順）より前で一番近い見出しの文字（本文を 1 回だけ前から辿る）。
fn sections_of(
    txn: &TransactionMut<'_>,
    fragment: &XmlFragmentRef,
    tops: &[u32],
) -> Vec<Option<String>> {
    let last = tops.iter().copied().max().unwrap_or(0);
    let mut current: Option<String> = None;
    let mut at: Vec<Option<String>> = Vec::with_capacity(last as usize + 1);
    for child in fragment.children(txn).take(last as usize + 1) {
        if let XmlOut::Element(el) = &child {
            if &**el.tag() == "heading" {
                current = Some(visible_text(txn, el));
            }
        }
        at.push(current.clone());
    }
    tops.iter()
        .map(|t| at.get(*t as usize).cloned().flatten())
        .collect()
}

fn flag(key: &str) -> Attrs {
    let mut attrs: Attrs = HashMap::new();
    attrs.insert(Arc::from(key), Any::Bool(true));
    attrs
}

/// バイト位置を yrs の位置へ（ノートの文書は UTF-8 のバイトで数える `OffsetKind::Bytes`）。
#[allow(clippy::cast_possible_truncation)] // 1 段落の長さは u32 に収まる。
fn offset(n: usize) -> u32 {
    n as u32
}

#[cfg(test)]
mod tests {
    use super::{common_affix, strip_markdown, widen_to_words};

    #[test]
    fn common_affix_keeps_the_shared_parts_on_char_boundaries() {
        assert_eq!(common_affix("前年比で12%増加", "前年比で14%増加"), (13, 7));
        assert_eq!(common_affix("課題", "主な課題"), (0, 6));
        assert_eq!(common_affix("abc", "abc"), (3, 0));
        assert_eq!(common_affix("aa", "a"), (1, 0));
        assert_eq!(common_affix("", "x"), (0, 0));
    }

    #[test]
    fn suggestions_are_cut_at_word_boundaries() {
        let cut = |a: &str, b: &str| {
            let (p, s) = common_affix(a, b);
            let (p, s) = widen_to_words(a, b, p, s);
            (a[p..a.len() - s].to_string(), b[p..b.len() - s].to_string())
        };
        assert_eq!(
            cut("前年比で12%増", "前年比で14%増"),
            ("12".into(), "14".into())
        );
        assert_eq!(
            cut("version 1.20 です", "version 1.25 です"),
            ("20".into(), "25".into())
        );
        assert_eq!(
            cut("田中が担当", "鈴木が担当"),
            ("田中".into(), "鈴木".into())
        );
        assert_eq!(cut("Q3 report", "Q4 report"), ("Q3".into(), "Q4".into()));
    }

    #[test]
    fn overlapping_matches_are_all_counted() {
        use super::overlapping_matches;
        assert_eq!(overlapping_matches("ababa", "aba"), vec![0, 2]);
        assert_eq!(overlapping_matches("ああああ", "ああ"), vec![0, 3, 6]);
        assert_eq!(overlapping_matches("abc", "x"), Vec::<usize>::new());
    }

    #[test]
    fn strip_markdown_removes_what_document_read_adds() {
        assert_eq!(strip_markdown("## 課題"), "課題");
        assert_eq!(strip_markdown("- [ ] 鈴木: 資料を作る"), "鈴木: 資料を作る");
        assert_eq!(strip_markdown("- **田中**：在庫確認"), "田中：在庫確認");
        assert_eq!(strip_markdown("12. 項目"), "項目");
        assert_eq!(strip_markdown("> 引用"), "引用");
        assert_eq!(strip_markdown("2026年"), "2026年");
    }
}

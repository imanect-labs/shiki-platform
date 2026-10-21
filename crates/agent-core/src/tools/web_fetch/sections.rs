//! Markdown の節分割と query 指向の絞り込み（#405 Phase 2）。
//!
//! deep research は 1 回の調査で `web_fetch` を 25〜40 件撃つ。ページ全体を先頭から詰めると、
//! 「調べたい論点」がページ後半にある記事は毎回取りこぼす。**節単位でスコアリングして
//! 関連する節だけ返す**と、同じ予算で拾える情報が桁で変わる。
//!
//! スコアリングは形態素解析を使わない（agent-core に Lindera を持ち込まない）。
//! ASCII は語単位、CJK は 2-gram で照合する。粗いが、節の順位付けには十分効く。

/// Markdown の 1 節（見出しとその配下の本文）。
pub(super) struct Section<'a> {
    /// 見出し行（`## …`）。文書先頭の見出し前ブロックは `None`。
    pub heading: Option<&'a str>,
    /// 見出し行を含む節全体のテキスト。
    pub text: &'a str,
}

impl Section<'_> {
    /// 見出しの表示用テキスト（`#`・装飾・リンク記法を除いたもの）。
    ///
    /// 一覧ページの見出しはリンクであることが多く（`## [ランキング](https://…)`）、
    /// URL をそのままヘッダの節一覧に並べると**地図であるはずの 1 行が URL で埋まる**。
    pub(super) fn title(&self) -> Option<String> {
        let raw = self
            .heading?
            .trim_start_matches('#')
            .trim()
            .trim_matches('*')
            .trim();
        let flat = unlink(raw);
        (!flat.is_empty()).then_some(flat)
    }
}

/// `[text](url)` を `text` へ畳む（見出し 1 行分なので素朴な走査で足りる）。
fn unlink(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find('[') {
        let Some(mid) = rest[open..].find("](").map(|i| i + open) else {
            break;
        };
        let Some(close) = rest[mid + 2..].find(')').map(|i| i + mid + 2) else {
            break;
        };
        out.push_str(&rest[..open]);
        out.push_str(&rest[open + 1..mid]);
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// Markdown を ATX 見出し（`# ` 〜 `###### `）で節へ割る。
///
/// コードフェンス内の `#` は見出しにしない（誤分割するとコードが千切れる）。
pub(super) fn split<'a>(markdown: &'a str) -> Vec<Section<'a>> {
    let mut sections = Vec::new();
    let mut start = 0usize;
    let mut heading: Option<&'a str> = None;
    let mut in_fence = false;
    let mut cursor = 0usize;

    for line in markdown.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
        } else if !in_fence && is_heading(trimmed) && cursor > start {
            sections.push(Section {
                heading,
                text: &markdown[start..cursor],
            });
            start = cursor;
            heading = Some(line.trim_end());
        } else if !in_fence && is_heading(trimmed) {
            heading = Some(line.trim_end());
        }
        cursor += line.len();
    }
    if start < markdown.len() {
        sections.push(Section {
            heading,
            text: &markdown[start..],
        });
    }
    sections
}

fn is_heading(line: &str) -> bool {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&hashes) && line[hashes..].starts_with(' ')
}

/// `query` に関連する節だけを、**文書順のまま**連結して返す。
///
/// 返り値は `(本文, 採用した節数, 全節数)`。関連節が 1 つも無ければ `None`
/// （呼び出し側は先頭から詰める通常経路へ落ちる＝「関連なし」で空を返さない）。
pub(super) fn select(markdown: &str, query: &str, budget_chars: usize) -> Option<Selected> {
    let terms = terms(query);
    if terms.is_empty() {
        return None;
    }
    let sections = split(markdown);
    if sections.len() < 2 {
        return None;
    }
    let mut scored: Vec<(usize, u32)> = sections
        .iter()
        .enumerate()
        .map(|(i, s)| (i, score(s, &terms)))
        .filter(|(_, score)| *score > 0)
        .collect();
    if scored.is_empty() {
        return None;
    }
    // 高スコア順に予算を埋め、最後に文書順へ戻す（読み順を壊さない）。
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut picked = Vec::new();
    let mut used = 0usize;
    for (index, _) in scored {
        let len = sections[index].text.chars().count();
        if used + len > budget_chars && !picked.is_empty() {
            continue;
        }
        picked.push(index);
        used += len;
    }
    picked.sort_unstable();

    let mut body = String::with_capacity(used);
    let mut previous: Option<usize> = None;
    for index in &picked {
        if previous.is_some_and(|p| p + 1 != *index) {
            body.push_str("\n（…関連の低い節は省略…）\n\n");
        }
        body.push_str(sections[*index].text);
        if !body.ends_with('\n') {
            body.push('\n');
        }
        previous = Some(*index);
    }
    Some(Selected {
        body: body.trim().to_string(),
        picked: picked.len(),
        total: sections.len(),
    })
}

/// query 指向の絞り込み結果。
pub(super) struct Selected {
    pub body: String,
    pub picked: usize,
    pub total: usize,
}

/// 節のスコア。見出しでの一致は本文の 3 倍に重み付けする（節の主題を表すため）。
fn score(section: &Section<'_>, terms: &[String]) -> u32 {
    let body = section.text.to_lowercase();
    let heading = section.title().unwrap_or_default().to_lowercase();
    terms
        .iter()
        .map(|t| {
            let in_body = u32::try_from(body.matches(t.as_str()).count().min(8)).unwrap_or(8);
            let in_heading =
                u32::try_from(heading.matches(t.as_str()).count().min(2)).unwrap_or(2) * 3;
            in_body + in_heading
        })
        .sum()
}

/// クエリを照合語へ割る。ASCII は語、CJK は 2-gram。
///
/// 形態素解析なしで日本語を扱うための現実解。「市場規模」は `市場`/`場規`/`規模` に割れ、
/// 部分一致でも加点される（再現率を優先し、順位付けの粗さは許容する）。
fn terms(query: &str) -> Vec<String> {
    let lowered = query.to_lowercase();
    let mut terms = Vec::new();
    let mut ascii = String::new();
    let mut cjk: Vec<char> = Vec::new();

    let flush_ascii = |buf: &mut String, out: &mut Vec<String>| {
        if buf.chars().count() >= 2 {
            out.push(std::mem::take(buf));
        } else {
            buf.clear();
        }
    };
    let flush_cjk = |buf: &mut Vec<char>, out: &mut Vec<String>| {
        match buf.len() {
            0 => {}
            1 => out.push(buf[0].to_string()),
            _ => {
                for pair in buf.windows(2) {
                    out.push(pair.iter().collect());
                }
            }
        }
        buf.clear();
    };

    for ch in lowered.chars() {
        if is_cjk(ch) {
            flush_ascii(&mut ascii, &mut terms);
            cjk.push(ch);
        } else if ch.is_alphanumeric() {
            flush_cjk(&mut cjk, &mut terms);
            ascii.push(ch);
        } else {
            flush_ascii(&mut ascii, &mut terms);
            flush_cjk(&mut cjk, &mut terms);
        }
    }
    flush_ascii(&mut ascii, &mut terms);
    flush_cjk(&mut cjk, &mut terms);
    terms.sort_unstable();
    terms.dedup();
    terms
}

/// CJK（漢字・かな）か。ラテン文字と分けて扱うための粗い判定。
fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{3040}'..='\u{30FF}'      // ひらがな・カタカナ
        | '\u{3400}'..='\u{4DBF}'    // CJK 拡張 A
        | '\u{4E00}'..='\u{9FFF}'    // CJK 統合漢字
        | '\u{F900}'..='\u{FAFF}'    // CJK 互換漢字
        | '\u{FF66}'..='\u{FF9D}'    // 半角カタカナ
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const DOC: &str = "序文の段落。\n\n\
        ## 市場規模\n2026 年の国内市場は 1.2 兆円に達した。\n\n\
        ## 競合状況\n主要ベンダは 3 社である。\n\n\
        ## 規制動向\n個人情報保護法の改正が影響する。\n";

    #[test]
    fn splits_on_headings_only() {
        let sections = split(DOC);
        assert_eq!(sections.len(), 4);
        assert_eq!(sections[0].title(), None);
        assert_eq!(sections[1].title().as_deref(), Some("市場規模"));
        assert_eq!(sections[3].title().as_deref(), Some("規制動向"));
    }

    /// 一覧ページの見出しはリンクであることが多い。節一覧は**地図**なので URL を並べない。
    #[test]
    fn title_flattens_link_syntax() {
        let md = "## [アクセスランキング](https://example.com/ranking)\n本文\n";
        assert_eq!(split(md)[0].title().as_deref(), Some("アクセスランキング"));
    }

    #[test]
    fn does_not_split_inside_code_fence() {
        let md = "# 見出し\n```\n# これはコメント\n```\n本文\n";
        assert_eq!(split(md).len(), 1);
    }

    #[test]
    fn selects_sections_matching_japanese_query() {
        let picked = select(DOC, "市場規模はいくらか", 10_000).unwrap();
        assert!(picked.body.contains("1.2 兆円"), "{}", picked.body);
        assert!(!picked.body.contains("主要ベンダ"), "{}", picked.body);
        assert_eq!(picked.total, 4);
    }

    #[test]
    fn selects_sections_matching_ascii_query() {
        let md = "# Intro\nnothing here\n\n# Pricing\nThe SaaS plan costs $20 per seat.\n";
        let picked = select(md, "pricing per seat", 10_000).unwrap();
        assert!(picked.body.contains("$20"), "{}", picked.body);
        assert!(!picked.body.contains("nothing here"), "{}", picked.body);
    }

    #[test]
    fn marks_gaps_between_non_adjacent_sections() {
        let picked = select(DOC, "市場 規制", 10_000).unwrap();
        assert!(picked.body.contains("省略"), "{}", picked.body);
    }

    #[test]
    fn returns_none_when_nothing_matches() {
        assert!(select(DOC, "まったく無関係な話題", 10_000).is_none());
    }

    #[test]
    fn keeps_at_least_one_section_even_over_budget() {
        // 予算 0 でも空を返さない（「関連なし」と誤解させない）。
        let picked = select(DOC, "市場規模", 0).unwrap();
        assert!(!picked.body.is_empty());
        assert_eq!(picked.picked, 1);
    }

    #[test]
    fn terms_split_ascii_and_cjk() {
        assert_eq!(terms("SaaS 市場"), vec!["saas", "市場"]);
        // 3 文字の CJK は 2-gram に割れる。
        assert_eq!(terms("市場規模"), vec!["場規", "市場", "規模"]);
        // 1 文字語は落とさない。
        assert_eq!(terms("a 円"), vec!["円"]);
    }
}

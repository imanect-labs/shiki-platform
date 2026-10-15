//! ask（質問のカード）と plan（計画のカード）（`@imanect/yui` の kinds/ask.ts と、plan の手順に使う kinds/seq.ts の steps の移植）。

use crate::ir::{AskOption, AskQuestion, Base, Step};
use crate::text::{
    cap, caps, clip, has_pipe, indent_of, is_match, is_separator_row, nfkc, parse_option, re,
    replace_first, split_cells, split_title, strip_list_marker, take_refs, uniq, Diags, Line,
};
use crate::text::{ASK_OPTIONS, ASK_QUESTIONS, PLAN_STEPS, STEPS};

re!(QUESTION, r"(?i)^(?:[?？]|Q\s*[.:：]|質問\s*[:：])\s*(.+)$");
re!(BUTTON, r"(?i)^(?:button|ボタン)\s+(.+)$");
re!(
    OTHER,
    r"(?i)^(?:その他|other|others|そのほか)(?:\s*[（(].*[)）]|\s*[:：].*)?$"
);
re!(
    MULTI,
    r"(?i)(^|\s)(?:multi|複数(?:可|選択)?|\(複数可\)|（複数可）)(\s|$)"
);
re!(OPTIONAL, r"(?i)(^|\s)(?:任意|optional)(\s|$)");
re!(EXAMPLE, r"(?i)(?:^|\s)(?:例|e\.g\.)\s+(.+)$");
re!(QUOTE, r"^>\s?(.*)$");
re!(REVISE, r"(?i)^(?:revise|修正)\s*[:：]\s*(.+)$");
re!(OFF, r"(?i)^(off|false|no|0|なし|しない|不可)$");
re!(
    STEP_HEADER,
    r"(?i)^(手順|ステップ|工程|段階|作業|内容|step|steps|no\.?|#|番号)$"
);
re!(
    STEP_PREFIX,
    r"(?i)^(?:step|ステップ|手順|stage|段階)\s*([0-9]+)"
);
re!(STEP_PREFIX_TAIL, r"^\s*[:：.、)）]?\s*");
re!(NUM_CELL, r"(?i)^(?:#|no\.?\s*)?[0-9]{1,3}[.)]?$");

fn append(desc: Option<&String>, add: &str) -> String {
    match desc {
        Some(d) if !d.is_empty() => format!("{d}\n{add}"),
        _ => add.to_string(),
    }
}

pub(crate) struct AskParts {
    pub(crate) intro: Option<String>,
    pub(crate) questions: Vec<AskQuestion>,
    pub(crate) submit_label: Option<String>,
}

pub(crate) fn parse_ask(base: &mut Base, body: &[Line], diags: &mut Diags) -> AskParts {
    let mut out = AskParts {
        intro: None,
        questions: Vec::new(),
        submit_label: None,
    };
    let mut has_q = false;
    for line in body {
        let trimmed = line.text.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some((k, v)) = parse_option(trimmed) {
            if k == "note" {
                base.note = Some(v);
            } else {
                base.src = Some(v);
            }
            continue;
        }
        if let Some(c) = caps(&QUOTE, trimmed) {
            out.intro = Some(clip(
                &append(out.intro.as_ref(), cap(&c, 1).trim()),
                line.no,
                diags,
            ));
            continue;
        }
        if let Some(c) = caps(&BUTTON, trimmed) {
            out.submit_label = Some(clip(cap(&c, 1).trim(), line.no, diags));
            continue;
        }
        if let Some(c) = caps(&QUESTION, trimmed) {
            if out.questions.len() >= ASK_QUESTIONS {
                diags.add_once(
                    line.no,
                    "warning",
                    "limit_exceeded",
                    format!("質問は {ASK_QUESTIONS} 個までです"),
                );
                has_q = false;
                continue;
            }
            let mut text = format!(" {} ", cap(&c, 1));
            let multi = is_match(&MULTI, &text);
            text = replace_first(&MULTI, &text, |c| format!(" {}", cap(c, 2)));
            let optional = is_match(&OPTIONAL, &text);
            text = replace_first(&OPTIONAL, &text, |c| format!(" {}", cap(c, 2)));
            let trimmed_end = text.trim_end().to_string();
            let ex = caps(&EXAMPLE, &trimmed_end)
                .map(|c| (c.get(0).map_or(0, |m| m.start()), cap(&c, 1)));
            if let Some((at, _)) = &ex {
                text = trimmed_end[..*at].to_string();
            }
            let q = AskQuestion {
                id: format!("q{}", out.questions.len() + 1),
                text: clip(text.trim(), line.no, diags),
                options: Vec::new(),
                multi,
                other: false,
                required: !optional,
                placeholder: ex.map(|(_, p)| clip(p.trim(), line.no, diags)),
            };
            out.questions.push(q);
            has_q = true;
            continue;
        }
        let (text, marked) = strip_list_marker(trimmed);
        if let (true, true, Some(q)) = (has_q, marked, out.questions.last_mut()) {
            if is_match(&OTHER, &nfkc(&text)) {
                q.other = true;
                continue;
            }
            if q.options.len() >= ASK_OPTIONS {
                diags.add_once(
                    line.no,
                    "warning",
                    "limit_exceeded",
                    format!("選択肢は 1 つの質問に {ASK_OPTIONS} 個までです"),
                );
                continue;
            }
            let (title, desc) = split_title(&text);
            let label = clip(&title, line.no, diags);
            let desc = desc.map(|d| clip(&d, line.no, diags));
            q.options.push(AskOption { label, desc });
            continue;
        }
        diags.add(line.no, "warning", "unrecognized_line", format!("解釈できない行を無視しました（質問は「? 質問文」、選択肢は「- 選択肢」）: {trimmed}"));
    }
    // 選択肢が 1 つだけの質問は選べないので、自由記述にする
    for q in &mut out.questions {
        if q.options.len() == 1 {
            diags.add(
                0,
                "warning",
                "unrecognized_line",
                format!("選択肢が 1 つの質問は自由記述にしました: {}", q.text),
            );
            q.options.clear();
        }
        if q.options.is_empty() {
            q.multi = false;
            q.other = false;
        }
    }
    out
}

pub(crate) struct PlanParts {
    pub(crate) intro: Option<String>,
    pub(crate) steps: Vec<Step>,
    pub(crate) revise: bool,
    pub(crate) approve_label: Option<String>,
}

pub(crate) fn parse_plan(base: &mut Base, body: &[Line], diags: &mut Diags) -> PlanParts {
    let mut out = PlanParts {
        intro: None,
        steps: Vec::new(),
        revise: true,
        approve_label: None,
    };
    let mut rest: Vec<Line> = Vec::new();
    for line in body {
        let trimmed = line.text.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(c) = caps(&QUOTE, trimmed) {
            out.intro = Some(clip(
                &append(out.intro.as_ref(), cap(&c, 1).trim()),
                line.no,
                diags,
            ));
            continue;
        }
        if let Some(c) = caps(&BUTTON, trimmed) {
            out.approve_label = Some(clip(cap(&c, 1).trim(), line.no, diags));
            continue;
        }
        if let Some(c) = caps(&REVISE, trimmed) {
            out.revise = !is_match(&OFF, cap(&c, 1).trim());
            continue;
        }
        rest.push(line.clone());
    }
    // 手順は steps と同じ書き方（番号・`見出し: 説明`・字下げの段落）
    let steps = parse_steps(base, &rest, diags);
    if steps.len() > PLAN_STEPS {
        diags.add(
            0,
            "warning",
            "limit_exceeded",
            format!("計画の手順は {PLAN_STEPS} 個までです"),
        );
    }
    out.steps = steps.into_iter().take(PLAN_STEPS).collect();
    out
}

/// 手順の 1 項目（読み途中。字下げの箇条書きは items にためて、最後に説明の段落にする）。
struct Entry {
    title: String,
    desc: Option<String>,
    refs: Vec<u32>,
    items: Vec<String>,
}

fn entry(title_text: &str, desc_text: Option<&str>, line: usize, diags: &mut Diags) -> Entry {
    let (t, refs) = take_refs(title_text);
    let mut e = Entry {
        title: clip(&t, line, diags),
        desc: None,
        refs,
        items: Vec::new(),
    };
    if let Some(d) = desc_text.filter(|d| !d.trim().is_empty()) {
        let (dt, drefs) = take_refs(d);
        e.desc = Some(clip(&dt, line, diags));
        let mut all = e.refs.clone();
        all.extend(drefs);
        e.refs = uniq(&all);
    }
    e
}

/// `STEP1: 申請` の番号を落とす（`Step 1〜3:` のような範囲は落とさない）。
fn strip_step_prefix(text: &str) -> String {
    let Some(c) = caps(&STEP_PREFIX, text) else {
        return text.to_string();
    };
    let Some(digits) = c.get(1) else {
        return text.to_string();
    };
    // 数字の直後が範囲の記号（`Step 1〜3`）なら番号ではない（数字は貪欲に読むので、直後が数字になることはない）
    if text[digits.end()..].starts_with(['〜', '~', '～', '-', '–', '—']) {
        return text.to_string();
    }
    let tail = &text[digits.end()..];
    let skip = STEP_PREFIX_TAIL
        .as_ref()
        .and_then(|r| r.find(tail))
        .map_or(0, |m| m.end());
    tail[skip..].to_string()
}

fn read_step(text: &str, line: usize, first: bool, diags: &mut Diags) -> Result<Option<Entry>, ()> {
    if has_pipe(text) || text.contains('\t') {
        let mut cells = split_cells(text);
        if is_separator_row(&cells) {
            return Ok(None);
        }
        if first && is_match(&STEP_HEADER, &nfkc(&cells[0])) {
            return Ok(None);
        }
        // `1 | 申請 | 説明` の番号の列は落とす
        if cells.len() > 1 && is_match(&NUM_CELL, &nfkc(&cells[0])) {
            cells.remove(0);
        }
        if cells[0].is_empty() {
            return Err(());
        }
        let desc = cells[1..]
            .iter()
            .filter(|c| !c.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" / ");
        return Ok(Some(entry(
            &cells[0],
            if desc.is_empty() { None } else { Some(&desc) },
            line,
            diags,
        )));
    }
    let stripped = strip_step_prefix(text);
    let (title, desc) = split_title(if stripped.is_empty() { text } else { &stripped });
    if title.is_empty() {
        return Err(());
    }
    Ok(Some(entry(&title, desc.as_deref(), line, diags)))
}

/// steps と同じ読み方（readEntries）。字下げした行は直前の手順の説明の段落にする。
fn parse_steps(base: &mut Base, body: &[Line], diags: &mut Diags) -> Vec<Step> {
    let mut out: Vec<Entry> = Vec::new();
    let mut base_indent: Option<usize> = None;
    for line in body {
        let indent = indent_of(&line.text);
        let (text, marked) = strip_list_marker(line.text.trim());
        if text.is_empty() {
            continue;
        }
        if !marked && !has_pipe(&text) {
            if let Some((k, v)) = parse_option(&text) {
                if k == "note" {
                    base.note = Some(v);
                } else {
                    base.src = Some(v);
                }
                continue;
            }
        }
        let b = *base_indent.get_or_insert(indent);
        if indent > b {
            if let Some(prev) = out.last_mut() {
                let (t, refs) = take_refs(&text);
                if marked {
                    prev.items.push(clip(&t, line.no, diags));
                } else {
                    prev.desc = Some(clip(&append(prev.desc.as_ref(), &t), line.no, diags));
                }
                let mut all = prev.refs.clone();
                all.extend(refs);
                prev.refs = uniq(&all);
                continue;
            }
        }
        match read_step(&text, line.no, out.is_empty(), diags) {
            Ok(None) => {}
            Err(()) => diags.add(
                line.no,
                "warning",
                "unrecognized_line",
                format!("解釈できない行を無視しました: {text}"),
            ),
            Ok(Some(e)) => {
                if out.len() >= STEPS {
                    diags.add_once(
                        line.no,
                        "warning",
                        "limit_exceeded",
                        format!("手順は {STEPS} 個までです"),
                    );
                    continue;
                }
                out.push(e);
            }
        }
    }
    out.into_iter()
        .map(|e| {
            // 字下げした箇条書きを、説明の段落として足す
            let mut parts: Vec<String> = e.desc.into_iter().filter(|d| !d.is_empty()).collect();
            parts.extend(e.items);
            Step {
                title: e.title,
                desc: if parts.is_empty() {
                    None
                } else {
                    Some(parts.join("\n"))
                },
                refs: e.refs,
            }
        })
        .collect()
}

#[cfg(test)]
mod pattern_tests {
    use super::*;

    /// 組めないパターンは「一致しない」扱いになり黙って壊れるので、すべて組めることを確かめる。
    #[test]
    fn patterns_compile() {
        for r in [
            &QUESTION,
            &BUTTON,
            &OTHER,
            &MULTI,
            &OPTIONAL,
            &EXAMPLE,
            &QUOTE,
            &REVISE,
            &OFF,
            &STEP_HEADER,
            &STEP_PREFIX,
            &STEP_PREFIX_TAIL,
            &NUM_CELL,
        ] {
            assert!(r.is_some());
        }
    }
}

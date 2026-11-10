//! ④ 分割: 保護済みの本文を予算内のチャンクへ割る（設計メモ §4.2）。
//!
//! チャンクは**原文の連続した区間**で、チャンク間の空白（改行・空行）はそのまま `gap_after` に持つ。
//! 組み立て（⑧）は「訳文 → 元の空白」を順に並べるだけで、段落の切れ目を作り変えない。
//!
//! 割る位置の優先順は 見出し → 空行（段落）→ 表の行 → 文末。表は空行を挟まず続くので、
//! 段落より先に取り出す（取り出さないと文末の分割がセル内の句点で行を千切る）。予算を超える表は
//! 行の境界でだけ割り、後続のチャンクに見出し行（＋区切り行）を複製する。複製分の訳は組み立てで捨てる。

/// 見出しの表示の上限（文脈に載せるだけなので短く切る）。
const MAX_HEADING_CHARS: usize = 60;

/// 1 チャンク。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Chunk {
    pub index: usize,
    /// 「## 背景 > ### 市場規模」の形の見出しの列（文脈に使う）。
    pub heading_path: Vec<String>,
    /// 訳す本文（表の続きでは複製した見出し行を先頭に含む）。
    pub text: String,
    /// 先頭に複製した表の見出しの行数（訳文から同じ行数を捨てる）。
    pub dup_header_lines: usize,
    /// このチャンクの後ろにあった原文の空白。
    pub gap_after: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnitKind {
    Heading,
    Table,
    Text,
}

/// 割る位置の候補の単位（原文の区間）。
#[derive(Debug, Clone)]
struct Unit {
    start: usize,
    end: usize,
    kind: UnitKind,
    /// 表を行で割った続きなら、複製する見出し行。
    table_header: Option<String>,
    /// この単位の先頭時点の見出しの列。
    path: Vec<String>,
}

pub(super) fn is_heading(line: &str) -> Option<usize> {
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    ((1..=6).contains(&hashes) && line[hashes..].starts_with(' ')).then_some(hashes)
}

fn is_table_line(line: &str) -> bool {
    line.trim_start().starts_with('|')
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// 本文を予算 `budget`（文字）前後のチャンクへ割る。`min_merge` 未満の塊は次の見出しで切らない。
pub(super) fn split(text: &str, budget: usize, min_merge: usize) -> Vec<Chunk> {
    // (チャンク, 原文での区間)
    let mut built: Vec<(Chunk, usize, usize)> = Vec::new();
    let mut cur: Option<(usize, usize, Vec<String>)> = None;

    let plain = |start: usize, end: usize, path: Vec<String>| {
        (
            Chunk {
                index: 0,
                heading_path: path,
                text: text[start..end].to_string(),
                dup_header_lines: 0,
                gap_after: String::new(),
            },
            start,
            end,
        )
    };

    for unit in units(text, budget) {
        // 行で割った表の続きは単独のチャンクにし、見出し行を複製する。
        if let Some(header) = &unit.table_header {
            if let Some((s, e, path)) = cur.take() {
                built.push(plain(s, e, path));
            }
            built.push((
                Chunk {
                    index: 0,
                    heading_path: unit.path.clone(),
                    text: format!("{header}\n{}", &text[unit.start..unit.end]),
                    dup_header_lines: header.lines().count(),
                    gap_after: String::new(),
                },
                unit.start,
                unit.end,
            ));
            continue;
        }
        cur = match cur.take() {
            None => Some((unit.start, unit.end, unit.path)),
            Some((s, e, path)) => {
                let grown = char_len(&text[s..unit.end]);
                let cur_len = char_len(&text[s..e]);
                let break_here = grown > budget
                    || (unit.kind == UnitKind::Heading && cur_len >= min_merge)
                    // 表は単独で予算を食うので、表の前で一度切る（小さい前置きだけなら同じチャンクに入る）。
                    || (unit.kind == UnitKind::Table && cur_len >= min_merge);
                if break_here {
                    built.push(plain(s, e, path));
                    Some((unit.start, unit.end, unit.path))
                } else {
                    Some((s, unit.end, path))
                }
            }
        };
    }
    if let Some((s, e, path)) = cur {
        built.push(plain(s, e, path));
    }
    // チャンク間の空白を記録する（区間の終わりから次の区間の始まりまで）。
    let next_starts: Vec<usize> = built
        .iter()
        .skip(1)
        .map(|(_, s, _)| *s)
        .chain(std::iter::once(text.len()))
        .collect();
    built
        .into_iter()
        .zip(next_starts)
        .enumerate()
        .map(|(index, ((mut chunk, _, end), next))| {
            chunk.index = index;
            chunk.gap_after = text[end.min(next)..next].to_string();
            chunk
        })
        .collect()
}

/// 行を単位へまとめる（見出し 1 行・表の連続行・段落）。長い表と段落はさらに割る。
fn units(text: &str, budget: usize) -> Vec<Unit> {
    let mut lines: Vec<(usize, &str)> = Vec::new();
    let mut off = 0;
    for line in text.split('\n') {
        lines.push((off, line));
        off += line.len() + 1;
    }
    let mut out = Vec::new();
    let mut path: Vec<(usize, String)> = Vec::new();
    let mut i = 0;
    let path_now = |p: &Vec<(usize, String)>| p.iter().map(|(_, h)| h.clone()).collect::<Vec<_>>();
    while i < lines.len() {
        let (start, line) = lines[i];
        if line.trim().is_empty() {
            i += 1;
            continue;
        }
        if let Some(level) = is_heading(line) {
            path.retain(|(l, _)| *l < level);
            let title: String = line.trim().chars().take(MAX_HEADING_CHARS).collect();
            path.push((level, title));
            out.push(Unit {
                start,
                end: start + line.len(),
                kind: UnitKind::Heading,
                table_header: None,
                path: path_now(&path),
            });
            i += 1;
            continue;
        }
        let table = is_table_line(line);
        let mut j = i;
        while j < lines.len() {
            let l = lines[j].1;
            if l.trim().is_empty() || is_heading(l).is_some() || is_table_line(l) != table {
                break;
            }
            j += 1;
        }
        let end = lines[j - 1].0 + lines[j - 1].1.len();
        let p = path_now(&path);
        if table {
            out.extend(table_units(text, &lines[i..j], budget, &p));
        } else if char_len(&text[start..end]) > budget {
            for (s, e) in sentence_spans(text, start, end, budget) {
                out.push(Unit {
                    start: s,
                    end: e,
                    kind: UnitKind::Text,
                    table_header: None,
                    path: p.clone(),
                });
            }
        } else {
            out.push(Unit {
                start,
                end,
                kind: UnitKind::Text,
                table_header: None,
                path: p,
            });
        }
        i = j;
    }
    out
}

/// 表を単位へ。予算内なら 1 つ、超えるなら行の境界で割り、続きに見出し行を持たせる。
fn table_units(text: &str, rows: &[(usize, &str)], budget: usize, path: &[String]) -> Vec<Unit> {
    let start = rows[0].0;
    let end = rows[rows.len() - 1].0 + rows[rows.len() - 1].1.len();
    let whole = Unit {
        start,
        end,
        kind: UnitKind::Table,
        table_header: None,
        path: path.to_vec(),
    };
    // 見出し行＋区切り行が無い表（htmd は付けるが念のため）は割らない。
    let has_header = rows.len() >= 3 && rows[1].1.contains("---");
    if char_len(&text[start..end]) <= budget || !has_header {
        return vec![whole];
    }
    let header = format!("{}\n{}", rows[0].1, rows[1].1);
    let header_len = char_len(&header) + 1;
    let mut out = Vec::new();
    let mut piece_start = start;
    let mut piece_rows = 0usize;
    let mut first = true;
    for (k, (off, row)) in rows.iter().enumerate().skip(2) {
        let row_end = off + row.len();
        let extra = if first { 0 } else { header_len };
        if piece_rows > 0 && char_len(&text[piece_start..row_end]) + extra > budget {
            let piece_end = rows[k - 1].0 + rows[k - 1].1.len();
            out.push(Unit {
                start: piece_start,
                end: piece_end,
                kind: UnitKind::Table,
                table_header: (!first).then(|| header.clone()),
                path: path.to_vec(),
            });
            first = false;
            piece_start = *off;
            piece_rows = 0;
        }
        piece_rows += 1;
    }
    out.push(Unit {
        start: piece_start,
        end,
        kind: UnitKind::Table,
        table_header: (!first).then(|| header.clone()),
        path: path.to_vec(),
    });
    out
}

/// 段落 `[start, end)` を文末で予算内の区間へ割る。1 文で予算を超えるものはそのまま 1 区間にする。
fn sentence_spans(text: &str, start: usize, end: usize, budget: usize) -> Vec<(usize, usize)> {
    let para = &text[start..end];
    // 文の終わり（区切りの直後の位置）。
    let mut cuts = Vec::new();
    let chars: Vec<(usize, char)> = para.char_indices().collect();
    for (k, &(pos, c)) in chars.iter().enumerate() {
        let next = chars.get(k + 1).map(|&(_, n)| n);
        let after = pos + c.len_utf8();
        let ends = match c {
            '。' | '！' | '？' | '．' => true,
            '.' | '!' | '?' => next.is_some_and(char::is_whitespace),
            _ => false,
        };
        if ends && after < para.len() {
            cuts.push(after);
        }
    }
    let mut spans = Vec::new();
    let mut piece_start = 0usize;
    let mut last_cut = None;
    for &cut in &cuts {
        if char_len(&para[piece_start..cut]) > budget {
            if let Some(lc) = last_cut.filter(|&lc| lc > piece_start) {
                spans.push((piece_start, lc));
                piece_start = skip_ws(para, lc);
            }
        }
        last_cut = Some(cut);
    }
    if char_len(&para[piece_start..]) > budget {
        if let Some(lc) = last_cut.filter(|&lc| lc > piece_start) {
            spans.push((piece_start, lc));
            piece_start = skip_ws(para, lc);
        }
    }
    spans.push((piece_start, para.len()));
    spans
        .into_iter()
        .filter(|(a, b)| b > a)
        .map(|(a, b)| (start + a, start + b))
        .collect()
}

fn skip_ws(s: &str, from: usize) -> usize {
    from + (s[from..].len() - s[from..].trim_start().len())
}

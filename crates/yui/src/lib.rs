//! 回答の本文の ```ui ブロック（`@imanect/yui`）のうち、操作を持つ種類（app・ask・plan）をサーバー側で読む。
//!
//! - ボタン（id・文言・実行先）を取り出し、ホストが束縛（ワークフローの版の固定など）を宣言する材料にする。
//! - ボタンが送る値を、画面（TS 版の `validateActionValues`）と同じ規則で検証し直す（画面の検証は信用しない）。
//!
//! 表示だけの種類（bar など）はここでは読まない（本文と一緒に文字のまま保存・再送する）。
//! TS 版と同じ結果になることを `tests/fixtures/app.json`（y-ui の `spec-tests/app.json`）で確かめる。

mod app;
mod ask;
mod ir;
mod text;
mod values;

pub use ir::{
    ActionTarget, AppAction, AppField, AppSection, AskOption, AskQuestion, Base, ChoiceOption,
    FieldKind, Figure, Kind, Step,
};
pub use text::Diagnostic;
pub use values::{figure_actions, validate_action_values, Check};

use sha2::{Digest, Sha256};
use text::{cap, caps, nfkc, re, take_refs, Diags, Line};

/// 解析の結果。操作を持たない種類・種類の分からないブロックは `figure` が `None`。
#[derive(Debug, Clone)]
pub struct Parsed {
    pub figure: Option<Figure>,
    pub diagnostics: Vec<Diagnostic>,
}

re!(HEADER, r"^\s*([^\s|:：]+)\s*(?:[|:：]\s*)?(.*)$");
re!(TITLE_HASH, r"^#+\s*");
re!(NOT_TITLE, r"[|\t]|->|→|^\s*[-*+]\s");

/// 種類の名前と別名（操作を持つ種類だけ）。
pub fn resolve_kind(token: &str) -> Option<Kind> {
    match nfkc(token).to_lowercase().as_str() {
        "app" | "form" | "アプリ" | "フォーム" => Some(Kind::App),
        "ask" | "question" | "questions" | "質問" | "確認" => Some(Kind::Ask),
        "plan" | "計画" => Some(Kind::Plan),
        _ => None,
    }
}

/// ` ```ui ` ブロック 1 つ分のソースを読む（確定したブロックとして）。`hint` はフェンスの情報文字列の種類（`ui:app`）。
pub fn parse_block(source: &str, hint: Option<Kind>) -> Parsed {
    let mut diags = Diags::default();
    let lines: Vec<Line> = source
        .split('\n')
        .enumerate()
        .map(|(i, t)| Line {
            no: i + 1,
            text: t.strip_suffix('\r').unwrap_or(t).to_string(),
        })
        .filter(|l| !l.text.trim().is_empty())
        .collect();
    let Some(first) = lines.first() else {
        return Parsed {
            figure: None,
            diagnostics: Vec::new(),
        };
    };
    let header = caps(&HEADER, &first.text);
    let header_kind = header.as_ref().and_then(|c| resolve_kind(&cap(c, 1)));
    let (kind, title, body) = match (header_kind, hint) {
        (Some(k), h) if h.is_none_or(|h| h == k) => (
            k,
            header.as_ref().map(|c| cap(c, 2)).unwrap_or_default(),
            &lines[1..],
        ),
        (_, Some(h)) if !text::is_match(&NOT_TITLE, &first.text) => {
            (h, first.text.trim().to_string(), &lines[1..])
        }
        (_, Some(h)) => (h, String::new(), &lines[..]),
        (_, None) => {
            return Parsed {
                figure: None,
                diagnostics: Vec::new(),
            }
        }
    };
    let title = text::replace_first(&TITLE_HASH, title.trim(), |_| String::new());
    let (title, refs) = take_refs(&title);
    let mut base = Base {
        title,
        refs,
        note: None,
        src: None,
    };
    let figure = match kind {
        Kind::App => {
            let sections = app::parse_app(&mut base, body, &mut diags);
            if sections.is_empty() {
                diags.add(0, "error", "empty", "表示できるデータがありません");
            }
            Figure::App { base, sections }
        }
        Kind::Ask => {
            let p = ask::parse_ask(&mut base, body, &mut diags);
            if p.questions.is_empty() {
                diags.add(0, "error", "empty", "表示できるデータがありません");
            }
            Figure::Ask {
                base,
                intro: p.intro,
                questions: p.questions,
                submit_label: p.submit_label,
            }
        }
        Kind::Plan => {
            let p = ask::parse_plan(&mut base, body, &mut diags);
            if p.steps.is_empty() {
                diags.add(0, "error", "empty", "表示できるデータがありません");
            }
            Figure::Plan {
                base,
                intro: p.intro,
                steps: p.steps,
                revise: p.revise,
                approve_label: p.approve_label,
            }
        }
    };
    Parsed {
        figure: Some(figure),
        diagnostics: diags.list,
    }
}

/// 本文の中の、操作を持つ ```ui ブロック 1 つ。
#[derive(Debug, Clone)]
pub struct ActionBlock {
    /// ブロックの識別子（ソースの SHA-256 の先頭 16 桁）。ボタンの id は `{hash}:{a1}`。
    pub hash: String,
    /// ブロックの中身（フェンスを除く）。
    pub source: String,
    pub figure: Figure,
    pub diagnostics: Vec<Diagnostic>,
}

/// ブロックの識別子。CRLF を LF にし、末尾の空白を落としたソースの SHA-256 の先頭 16 桁
/// （画面側も同じ規則で求める。Markdown のレンダラーが末尾の改行を付け外ししても変わらないように）。
pub fn block_hash(source: &str) -> String {
    let normalized = source.replace("\r\n", "\n");
    let digest = Sha256::digest(normalized.trim_end().as_bytes());
    digest
        .iter()
        .take(8)
        .fold(String::with_capacity(16), |mut out, b| {
            use std::fmt::Write as _;
            let _ = write!(out, "{b:02x}");
            out
        })
}

/// ボタンの束縛の id（`{ブロックの識別子}:{ボタンの id}`）。
pub fn action_key(hash: &str, action_id: &str) -> String {
    format!("{hash}:{action_id}")
}

re!(OPEN, r"^( {0,3})(`{3,}|~{3,})(.*)$");

fn info_kind(info: &str) -> (bool, Option<Kind>) {
    let t = info.trim();
    let lang: String = t
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != ':' && *c != '{')
        .collect::<String>()
        .to_lowercase();
    if lang != "ui" && lang != "yui" {
        return (false, None);
    }
    let rest = &t[lang.len()..];
    let kind = rest
        .strip_prefix(':')
        .or_else(|| rest.strip_prefix(char::is_whitespace))
        .and_then(|r| r.split_whitespace().next())
        .and_then(resolve_kind);
    (true, kind)
}

/// Markdown から、閉じた ```ui ブロックのうち操作を持つ種類（app・ask・plan）を取り出す。
/// CommonMark のフェンス規則（3 文字以上の ``` か ~~~、行頭の空白 3 つまで、閉じは同じ文字で同じ長さ以上）に従い、
/// 他の言語のコードブロックの中は見ない。閉じていないブロック（生成の途中）は読まない。
pub fn extract_action_blocks(markdown: &str) -> Vec<ActionBlock> {
    let lines: Vec<&str> = markdown
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let Some(open) = caps(&OPEN, lines[i]) else {
            i += 1;
            continue;
        };
        let fence = cap(&open, 2);
        let info = cap(&open, 3);
        // バッククォートのフェンスの情報文字列にはバッククォートを含められない
        if fence.starts_with('`') && info.contains('`') {
            i += 1;
            continue;
        }
        let ch = fence.chars().next().unwrap_or('`');
        let len = fence.chars().count();
        let is_close = |l: &str| {
            let t = l.trim_start_matches(' ');
            if l.len() - t.len() > 3 {
                return false;
            }
            let n = t.chars().take_while(|c| *c == ch).count();
            n >= len && t[n * ch.len_utf8()..].trim().is_empty()
        };
        let mut j = i + 1;
        while j < lines.len() && !is_close(lines[j]) {
            j += 1;
        }
        let closed = j < lines.len();
        let (is_ui, hint) = info_kind(&info);
        if is_ui && closed {
            let body = lines[i + 1..j].join("\n");
            let source = if body.is_empty() {
                body
            } else {
                format!("{body}\n")
            };
            let parsed = parse_block(&source, hint);
            if let Some(figure) = parsed.figure {
                out.push(ActionBlock {
                    hash: block_hash(&source),
                    source,
                    figure,
                    diagnostics: parsed.diagnostics,
                });
            }
        }
        i = if closed { j + 1 } else { j };
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_only_closed_action_blocks() {
        let md = "前置き\n\n```ui\nbar x\na | 1\n```\n\n```ui:app\napp 申請\n件名 = text\nbutton 送る -> workflow 申請\n```\n\n````md\n```ui\nask x\n? q\n- a\n- b\n```\n````\n\n```ui\nplan 計画\n1. A\n";
        let blocks = extract_action_blocks(md);
        assert_eq!(blocks.len(), 1);
        assert!(matches!(blocks[0].figure, Figure::App { .. }));
        assert_eq!(
            blocks[0].hash,
            block_hash("app 申請\n件名 = text\nbutton 送る -> workflow 申請")
        );
        assert_eq!(blocks[0].hash.len(), 16);
    }

    #[test]
    fn hash_ignores_trailing_newlines_and_crlf() {
        assert_eq!(
            block_hash("app x\r\nbutton a -> chat\r\n"),
            block_hash("app x\nbutton a -> chat")
        );
        assert_ne!(block_hash("app x"), block_hash("app y"));
    }

    #[test]
    fn patterns_compile() {
        for r in [&HEADER, &TITLE_HASH, &NOT_TITLE, &OPEN] {
            assert!(r.is_some());
        }
    }

    #[test]
    fn non_action_kinds_are_skipped() {
        assert!(parse_block("bar x\na | 1\n", None).figure.is_none());
        assert!(parse_block("? q\n- a\n- b\n", Some(Kind::Ask))
            .figure
            .is_some());
    }
}

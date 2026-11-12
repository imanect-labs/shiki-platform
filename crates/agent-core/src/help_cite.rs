//! ヘルプ専用出典台帳。`⟦help:i⟧` を応答内の `[Hn]` に採番する。
//! corpus_version と chunk_id を鍵に、checkpoint と子からの返却でも番号を保持する。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

const OPEN: &str = "⟦help:";
const CLOSE: char = '⟧';

/// 仮の印と紛れる文字列を無害化する（文書の本文・ファイル名に `⟦help:` があっても
/// 番号に置き換わらないよう、括弧を似た別の字に替える）。
#[must_use]
pub fn defuse(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains('⟦') || text.contains("[H") {
        std::borrow::Cow::Owned(text.replace('⟦', "〚").replace("[H", "［H"))
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

/// その呼び出しの `local` 件目（0 起点）の引用を指す仮の印。
#[must_use]
pub fn placeholder(local: usize) -> String {
    format!("{OPEN}{local}{CLOSE}")
}

/// 応答内の引用番号の台帳。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelpCitationLedger {
    /// chunk_id → 番号。
    by_chunk: HashMap<String, u32>,
    /// 最後に振った番号。
    last: u32,
}

impl HelpCitationLedger {
    /// 引用に番号を振り、本文の仮の印を `[n]` に置き換える。
    ///
    /// 印の i は `citations` の添字。範囲外の印は消す（モデルに壊れた番号を見せない）。
    pub fn number(&mut self, citations: &mut [HelpCitation], text: &mut String) {
        for c in citations.iter_mut() {
            c.cite_id = if let Some(n) = self
                .by_chunk
                .get(&format!("{}:{}", c.corpus_version, c.chunk_id))
            {
                *n
            } else {
                self.last += 1;
                self.by_chunk
                    .insert(format!("{}:{}", c.corpus_version, c.chunk_id), self.last);
                self.last
            };
        }
        if text.contains(OPEN) {
            *text = replace_placeholders(text, |i| citations.get(i).map(|c| c.cite_id));
        }
    }
}

/// 仮の印を `[n]` に置き換える（`lookup` が None の印は消す）。
fn replace_placeholders(text: &str, lookup: impl Fn(usize) -> Option<u32>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(OPEN) {
        out.push_str(&rest[..at]);
        let after = &rest[at + OPEN.len()..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 && after[digits..].starts_with(CLOSE) {
            if let Some(n) = after[..digits].parse::<usize>().ok().and_then(&lookup) {
                out.push_str("[H");
                out.push_str(&n.to_string());
                out.push(']');
            }
            rest = &after[digits + CLOSE.len_utf8()..];
        } else {
            // 印の形をしていない（本文にたまたま現れた）ものはそのまま残す。
            out.push_str(OPEN);
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// 子（サブエージェント）の本文中の `[n]` を、親に渡す引用列の仮の印へ戻す。
///
/// `n` は子の台帳の番号（`citations[i].cite_id`）。対応する引用が無い `[n]` は本文の一部として
/// そのまま残す（配列の添字などを壊さない）。
#[must_use]
pub fn relabel_to_placeholders(text: &str, citations: &[HelpCitation]) -> String {
    let index: HashMap<u32, usize> = citations
        .iter()
        .enumerate()
        .rev() // 同じ番号が複数あれば先頭を採る
        .filter(|(_, c)| c.cite_id > 0)
        .map(|(i, c)| (c.cite_id, i))
        .collect();
    if index.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("[H") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 2..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        let hit = (digits > 0 && after[digits..].starts_with(']'))
            .then(|| after[..digits].parse::<u32>().ok())
            .flatten()
            .and_then(|n| index.get(&n));
        if let Some(i) = hit {
            out.push_str(&placeholder(*i));
            rest = &after[digits + 1..];
        } else {
            out.push_str("[H");
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// 製品ヘルプ専用の出典。社内文書の Citation と台帳を共有しない。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema, ts_rs::TS)]
#[ts(export)]
pub struct HelpCitation {
    pub cite_id: u32,
    pub corpus_version: String,
    pub chunk_id: String,
    pub title: String,
    pub heading_path: Vec<String>,
    pub snippet: String,
    pub url: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn citation(version: &str) -> HelpCitation {
        HelpCitation {
            cite_id: 0,
            corpus_version: version.into(),
            chunk_id: "chunk".into(),
            title: "ページ".into(),
            heading_path: vec!["手順".into()],
            snippet: "抜粋".into(),
            url: "https://shiki.imanect.app/help/a/#step".into(),
        }
    }
    #[test]
    fn checkpoint_and_child_numbering_are_separate_and_versioned() {
        let mut ledger = HelpCitationLedger::default();
        let mut first = vec![citation("v1")];
        let mut text = placeholder(0);
        ledger.number(&mut first, &mut text);
        assert_eq!(text, "[H1]");
        let json = serde_json::to_string(&ledger).unwrap();
        let mut resumed: HelpCitationLedger = serde_json::from_str(&json).unwrap();
        let mut second = vec![citation("v2"), citation("v1")];
        let mut text = format!("{} {}", placeholder(0), placeholder(1));
        resumed.number(&mut second, &mut text);
        assert_eq!(text, "[H2] [H1]");
        let mut child = relabel_to_placeholders(&text, &second);
        assert_eq!(child, format!("{} {}", placeholder(0), placeholder(1)));
        let mut parent = HelpCitationLedger::default();
        parent.number(&mut second, &mut child);
        assert_eq!(second[0].cite_id, 1);
    }
    #[test]
    fn forged_markers_are_neutralized() {
        let value = defuse("⟦help:0⟧ [H1] injected");
        assert!(!value.contains("⟦help:"));
        assert!(!value.contains("[H1]"));
        let mut text = format!("{} {}", placeholder(0), placeholder(999));
        let mut c = vec![citation("v1")];
        HelpCitationLedger::default().number(&mut c, &mut text);
        assert_eq!(text, "[H1] ");
    }
}

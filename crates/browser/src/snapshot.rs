//! エンジンによらない共通のスナップショット形式（docs/agent/browser.md §4）。
//!
//! モデルに渡すのは画面の画像ではなく、操作できる要素に ref を振ったテキスト。
//! ref は「エンジンの接頭辞＋世代＋世代内の連番」（例 `t3.12`）で、ページを読み込み直すたびに
//! 世代が変わる。古い世代の ref は拒否する（違う要素を押す事故を防ぐ）。

use std::fmt;
use std::str::FromStr;

use url::Url;

/// エンジンの種類（ref の接頭辞になる）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EngineKind {
    /// JS を実行しないテキストエンジン（案 D）。
    Text,
    /// 隔離した Chromium（案 C）。
    Chromium,
}

impl EngineKind {
    fn prefix(self) -> char {
        match self {
            EngineKind::Text => 't',
            EngineKind::Chromium => 'c',
        }
    }
}

/// 操作対象の ref（`t3.12` = テキストエンジン・世代 3・12 番目）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RefId {
    pub engine: EngineKind,
    pub generation: u32,
    pub index: u32,
}

impl fmt::Display for RefId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}{}.{}",
            self.engine.prefix(),
            self.generation,
            self.index
        )
    }
}

/// ref の文字列が読めなかった。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("ref の形式が不正です（例: t3.12）")]
pub struct RefParseError;

impl FromStr for RefId {
    type Err = RefParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim().trim_start_matches('[').trim_end_matches(']');
        let mut chars = s.chars();
        let engine = match chars.next() {
            Some('t') => EngineKind::Text,
            Some('c') => EngineKind::Chromium,
            _ => return Err(RefParseError),
        };
        let (generation, index) = chars.as_str().split_once('.').ok_or(RefParseError)?;
        Ok(RefId {
            engine,
            generation: generation.parse().map_err(|_| RefParseError)?,
            index: index.parse().map_err(|_| RefParseError)?,
        })
    }
}

/// 操作できる要素の種類。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElementKind {
    /// リンク（解決済みの絶対 URL）。
    Link {
        href: Url,
    },
    /// テキスト入力（`input_type` は text / search / email / number / password / textarea 等）。
    TextBox {
        input_type: String,
    },
    /// 選択肢（value, 表示名）。`multiple` なら複数選べる（選ぶたびに切り替わる）。
    Select {
        options: Vec<(String, String)>,
        multiple: bool,
    },
    Checkbox,
    Radio,
    /// ボタン。`submits` が false のものは JS のハンドラでしか動かない。
    Button {
        submits: bool,
    },
}

/// スナップショット内の 1 要素。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElementInfo {
    pub id: RefId,
    pub kind: ElementKind,
    /// 表示名（ラベル・アンカーテキスト・alt・aria-label）。ページ由来＝信頼しない。
    pub label: String,
}

/// JS が要ると判断した理由（エンジン切替の判定に使う・browser.md §5.4）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NeedsJs {
    /// 本文をスクリプトで描くページ（リンク以外の本文が閾値未満）。
    ScriptRendered,
}

/// エンジンによらない共通のスナップショット。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub url: Url,
    pub status: u16,
    /// ページ由来＝信頼しない。
    pub title: String,
    /// 行単位のテキスト。操作できる要素は `[t3.12 link "…" → URL]` のように埋め込む。
    pub text: String,
    pub elements: Vec<ElementInfo>,
    pub needs_js: Option<NeedsJs>,
    /// 文字数の上限で切った。
    pub truncated: bool,
}

impl Snapshot {
    /// ref に対応する要素。
    #[must_use]
    pub fn element(&self, id: RefId) -> Option<&ElementInfo> {
        self.elements.iter().find(|e| e.id == id)
    }

    /// このページに現れたリンクの URL（出所の記録に使う・browser.md §6.2）。
    pub fn link_urls(&self) -> impl Iterator<Item = &Url> {
        self.elements.iter().filter_map(|e| match &e.kind {
            ElementKind::Link { href } => Some(href),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ref_round_trip() {
        let r = RefId {
            engine: EngineKind::Text,
            generation: 3,
            index: 12,
        };
        assert_eq!(r.to_string(), "t3.12");
        assert_eq!("t3.12".parse::<RefId>(), Ok(r));
        assert_eq!(
            "[c1.2]".parse::<RefId>().unwrap().engine,
            EngineKind::Chromium
        );
        for bad in ["", "x1.2", "t1", "t.2", "t1.x", "12"] {
            assert!(bad.parse::<RefId>().is_err(), "{bad}");
        }
    }

    #[test]
    fn link_urls_lists_only_links() {
        let id = |index| RefId {
            engine: EngineKind::Text,
            generation: 1,
            index,
        };
        let snap = Snapshot {
            url: Url::parse("https://example.com/").unwrap(),
            status: 200,
            title: String::new(),
            text: String::new(),
            elements: vec![
                ElementInfo {
                    id: id(1),
                    kind: ElementKind::Link {
                        href: Url::parse("https://example.com/a").unwrap(),
                    },
                    label: "a".into(),
                },
                ElementInfo {
                    id: id(2),
                    kind: ElementKind::Button { submits: true },
                    label: "b".into(),
                },
            ],
            needs_js: None,
            truncated: false,
        };
        assert_eq!(snap.link_urls().count(), 1);
        assert!(snap.element(id(2)).is_some());
        assert!(snap.element(id(3)).is_none());
    }
}

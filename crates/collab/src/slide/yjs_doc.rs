//! スライド構造 ⇔ Yjs（Array "slides" ＋ Map "meta"）の相互変換（Task 11.1・design §4.8.3）。
//!
//! Yjs 側の構造:
//! - スライド列は Array **"slides"**。要素は Map `{id: 文字列, html: Y.Text,
//!   notes: Y.Text, bg: JSON 文字列}`。スライドの追加/削除/並べ替えは Y.Array の
//!   要素単位で収束し、同一スライド内の並行編集は Y.Text の文字粒度でマージされる
//!   （壊れた HTML は取り込み側の正規化で自己修復する・PIT-41）。
//! - メタデータはノートと同じ Map **"meta"**（[`crate::note::yjs_meta`] を共用）。

use uuid::Uuid;
use yrs::{Any, Array, ArrayRef, GetString, In, Map, Out, ReadTxn, TextPrelim, TransactionMut};

use super::model::Slide;

/// スライド列の Y.Array 名（本プラットフォームの規約）。
pub const SLIDES_ARRAY_NAME: &str = "slides";

/// Y.Array "slides" からスライド列を読む（未知の要素形は落とさず可能な範囲で読む）。
pub fn read_slides<T: ReadTxn>(txn: &T, array: &ArrayRef) -> Vec<Slide> {
    array
        .iter(txn)
        .filter_map(|out| {
            let Out::YMap(map) = out else { return None };
            let id = map_string(txn, &map, "id").unwrap_or_default();
            let html = map_text(txn, &map, "html").unwrap_or_default();
            let notes = map_text(txn, &map, "notes").unwrap_or_default();
            let bg = map_string(txn, &map, "bg")
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
                .filter(|v| !v.is_null());
            Some(Slide {
                id,
                html,
                notes,
                bg,
            })
        })
        .collect()
}

/// スライド列を Y.Array へ**全置換**で書く（インポート経路・Task 11.1 単方向規約）。
///
/// ID が空のスライドには安定 ID を採番する（AI 編集・選択コンテキストの参照キー）。
pub fn write_slides(txn: &mut TransactionMut<'_>, array: &ArrayRef, slides: &[Slide]) {
    let existing = array.len(txn);
    if existing > 0 {
        array.remove_range(txn, 0, existing);
    }
    for slide in slides {
        array.push_back(txn, slide_prelim(slide));
    }
}

/// スライド 1 枚の Yjs 挿入表現（Map prelim）。
pub(crate) fn slide_prelim(slide: &Slide) -> yrs::MapPrelim {
    let id = if slide.id.is_empty() {
        Uuid::new_v4().to_string()
    } else {
        slide.id.clone()
    };
    let mut entries: Vec<(&str, In)> = vec![
        ("id", In::from(id)),
        ("html", In::Text(TextPrelim::new(slide.html.clone()).into())),
        (
            "notes",
            In::Text(TextPrelim::new(slide.notes.clone()).into()),
        ),
    ];
    if let Some(bg) = &slide.bg {
        entries.push(("bg", In::from(bg.to_string())));
    }
    entries.into_iter().collect()
}

fn map_string<T: ReadTxn>(txn: &T, map: &yrs::MapRef, key: &str) -> Option<String> {
    match map.get(txn, key)? {
        Out::Any(Any::String(s)) => Some(s.to_string()),
        _ => None,
    }
}

/// Y.Text または素の文字列として格納された値を読む（クライアント実装差への寛容）。
fn map_text<T: ReadTxn>(txn: &T, map: &yrs::MapRef, key: &str) -> Option<String> {
    match map.get(txn, key)? {
        Out::YText(text) => Some(text.get_string(txn)),
        Out::Any(Any::String(s)) => Some(s.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yrs::{Doc, Transact};

    #[test]
    fn 書いて読むと同じスライド列が返る() {
        let doc = Doc::new();
        let array = doc.get_or_insert_array(SLIDES_ARRAY_NAME);
        let slides = vec![
            Slide {
                id: "s1".into(),
                html: "<h1>表紙</h1>".into(),
                notes: "挨拶".into(),
                bg: Some(serde_json::json!({"color": "#fff"})),
            },
            Slide {
                id: "s2".into(),
                html: "<p>本文</p>".into(),
                notes: String::new(),
                bg: None,
            },
        ];
        {
            let mut txn = doc.transact_mut();
            write_slides(&mut txn, &array, &slides);
        }
        let txn = doc.transact();
        assert_eq!(read_slides(&txn, &array), slides);
    }

    #[test]
    fn 空idは採番される() {
        let doc = Doc::new();
        let array = doc.get_or_insert_array(SLIDES_ARRAY_NAME);
        let slides = vec![Slide {
            id: String::new(),
            html: "<p>x</p>".into(),
            notes: String::new(),
            bg: None,
        }];
        {
            let mut txn = doc.transact_mut();
            write_slides(&mut txn, &array, &slides);
        }
        let txn = doc.transact();
        let read = read_slides(&txn, &array);
        assert_eq!(read.len(), 1);
        assert!(!read[0].id.is_empty());
    }

    #[test]
    fn 全置換で古いスライドが残らない() {
        let doc = Doc::new();
        let array = doc.get_or_insert_array(SLIDES_ARRAY_NAME);
        let first = vec![Slide {
            id: "old".into(),
            html: "<p>旧</p>".into(),
            notes: String::new(),
            bg: None,
        }];
        let second = vec![Slide {
            id: "new".into(),
            html: "<p>新</p>".into(),
            notes: String::new(),
            bg: None,
        }];
        {
            let mut txn = doc.transact_mut();
            write_slides(&mut txn, &array, &first);
        }
        {
            let mut txn = doc.transact_mut();
            write_slides(&mut txn, &array, &second);
        }
        let txn = doc.transact();
        assert_eq!(read_slides(&txn, &array), second);
    }
}

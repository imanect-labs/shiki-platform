//! ノートメタデータ（Yjs Map "meta" ⇔ [`NoteMeta`]）の変換（Task 11P.2）。
//!
//! Map 名は [`super::yjs_map::META_MAP_NAME`]。値は文字列（tags のみ文字列配列）で、
//! frontmatter（[`super::frontmatter`]）と往復可能な平坦構造に限定する。

use yrs::{Any, Map, MapRef, Out, ReadTxn, TransactionMut};

use super::frontmatter::NoteMeta;

pub fn read_meta<T: ReadTxn>(txn: &T, map: &MapRef) -> NoteMeta {
    let mut meta = NoteMeta::default();
    for (key, value) in map.iter(txn) {
        let Out::Any(any) = value else { continue };
        match key {
            "title" => meta.title = any_string(&any),
            "icon" => meta.icon = any_string(&any),
            "thread_id" => meta.thread_id = any_string(&any),
            "tags" => {
                if let Any::Array(items) = any {
                    meta.tags = items.iter().filter_map(any_string_ref).collect();
                }
            }
            _ => {
                if let Some(s) = any_string(&any) {
                    meta.extra.insert(key.to_string(), s);
                }
            }
        }
    }
    meta
}

/// メタを Map へ全置換で書く（インポート経路）。
pub fn write_meta(txn: &mut TransactionMut<'_>, map: &MapRef, meta: &NoteMeta) {
    let existing: Vec<String> = map.keys(txn).map(str::to_string).collect();
    for key in existing {
        map.remove(txn, &key);
    }
    if let Some(title) = &meta.title {
        map.insert(txn, "title", Any::from(title.as_str()));
    }
    if let Some(icon) = &meta.icon {
        map.insert(txn, "icon", Any::from(icon.as_str()));
    }
    if !meta.tags.is_empty() {
        let tags: Vec<Any> = meta.tags.iter().map(|t| Any::from(t.as_str())).collect();
        map.insert(txn, "tags", Any::from(tags));
    }
    if let Some(thread_id) = &meta.thread_id {
        map.insert(txn, "thread_id", Any::from(thread_id.as_str()));
    }
    for (key, value) in &meta.extra {
        map.insert(txn, key.as_str(), Any::from(value.as_str()));
    }
}

fn any_string(any: &Any) -> Option<String> {
    match any {
        Any::String(s) => Some(s.to_string()),
        _ => None,
    }
}

fn any_string_ref(any: &Any) -> Option<String> {
    any_string(any)
}

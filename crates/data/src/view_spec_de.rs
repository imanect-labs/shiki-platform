//! [`ViewSpec`] の読み込み（未知のキーを拒否するタグ付き enum・検証 C-P2-8）。
//!
//! `#[serde(tag = "kind")]` の派生と種別の構造体の `deny_unknown_fields` を併用すると、utoipa が種別の
//! スキーマに `additionalProperties: false` を付け、`allOf` で足した `kind` が「余りのキー」になって
//! 実際の本文が厳密な JSON Schema 検証に通らない。そこで種別の構造体からは `deny_unknown_fields` を外し、
//! 未知のキーの拒否はここで行う（入れ子の列設定・グループ化・条件付き書式は従来どおり serde が拒否する）。

use serde::de::Error as _;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::view_spec::{CalendarView, GalleryView, GridView, KanbanView, ViewSpec};

/// 種別ごとに受け付けるキー（`kind` を除く）。構造体の項目と一致すること（単体テストで固定）。
const GRID_KEYS: &[&str] = &["columns", "row_height", "group_by", "conditional_format"];
const KANBAN_KEYS: &[&str] = &["group_field", "card_fields", "hide_empty"];
const CALENDAR_KEYS: &[&str] = &["date_field", "title_field"];
const GALLERY_KEYS: &[&str] = &["title_field", "card_fields"];
const KINDS: &[&str] = &["grid", "kanban", "calendar", "gallery"];

fn keys_of(kind: &str) -> Option<&'static [&'static str]> {
    Some(match kind {
        "grid" => GRID_KEYS,
        "kanban" => KANBAN_KEYS,
        "calendar" => CALENDAR_KEYS,
        "gallery" => GALLERY_KEYS,
        _ => return None,
    })
}

impl<'de> Deserialize<'de> for ViewSpec {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let Value::Object(mut obj) = Value::deserialize(d)? else {
            return Err(D::Error::custom(
                "表示設定は JSON オブジェクトで指定してください",
            ));
        };
        let kind = match obj.remove("kind") {
            Some(Value::String(k)) => k,
            Some(_) => return Err(D::Error::custom("kind は文字列で指定してください")),
            None => return Err(D::Error::missing_field("kind")),
        };
        let allowed = keys_of(&kind).ok_or_else(|| D::Error::unknown_variant(&kind, KINDS))?;
        if let Some(k) = obj.keys().find(|k| !allowed.contains(&k.as_str())) {
            return Err(D::Error::unknown_field(k, allowed));
        }
        let inner = Value::Object(obj);
        let r = match kind.as_str() {
            "grid" => serde_json::from_value::<GridView>(inner).map(ViewSpec::Grid),
            "kanban" => serde_json::from_value::<KanbanView>(inner).map(ViewSpec::Kanban),
            "calendar" => serde_json::from_value::<CalendarView>(inner).map(ViewSpec::Calendar),
            _ => serde_json::from_value::<GalleryView>(inner).map(ViewSpec::Gallery),
        };
        r.map_err(D::Error::custom)
    }
}

/// 種別の構造体を直列化したときのキー（テスト用）。
#[cfg(test)]
fn serialized_keys(v: &ViewSpec) -> Vec<String> {
    match serde_json::to_value(v) {
        Ok(Value::Object(m)) => m.keys().filter(|k| *k != "kind").cloned().collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::view_spec::{ConditionalFormat, FormatColor, GroupConfig};
    use serde_json::json;

    #[test]
    fn allowed_keys_match_struct_fields() {
        let grid = ViewSpec::Grid(GridView {
            group_by: vec![GroupConfig {
                field: "a".into(),
                descending: true,
                collapsed: true,
            }],
            conditional_format: vec![ConditionalFormat {
                condition: crate::query::ir::Condition::All(vec![]),
                color: FormatColor::Red,
                fields: vec![],
                bold: false,
            }],
            ..GridView::default()
        });
        let kanban = ViewSpec::Kanban(KanbanView {
            group_field: "s".into(),
            card_fields: vec!["a".into()],
            hide_empty: true,
        });
        let cal = ViewSpec::Calendar(CalendarView {
            date_field: "d".into(),
            title_field: Some("t".into()),
        });
        let gal = ViewSpec::Gallery(GalleryView {
            title_field: Some("t".into()),
            card_fields: vec!["a".into()],
        });
        for (v, keys) in [
            (&grid, GRID_KEYS),
            (&kanban, KANBAN_KEYS),
            (&cal, CALENDAR_KEYS),
            (&gal, GALLERY_KEYS),
        ] {
            let mut got = serialized_keys(v);
            got.sort();
            let mut want: Vec<String> = keys.iter().map(|k| (*k).to_string()).collect();
            want.sort();
            assert_eq!(got, want);
            // 直列化した本文を読み戻せる。
            let back: ViewSpec = serde_json::from_value(serde_json::to_value(v).unwrap()).unwrap();
            assert_eq!(&back, v);
        }
    }

    #[test]
    fn unknown_keys_and_kinds_are_rejected() {
        assert!(
            serde_json::from_value::<ViewSpec>(json!({"kind": "grid", "colour": "red"})).is_err()
        );
        assert!(serde_json::from_value::<ViewSpec>(json!({"kind": "list"})).is_err());
        assert!(serde_json::from_value::<ViewSpec>(json!({"columns": []})).is_err());
        assert!(
            serde_json::from_value::<ViewSpec>(json!({"kind": "kanban", "columns": []})).is_err()
        );
        // 入れ子の未知のキーも拒否する。
        assert!(serde_json::from_value::<ViewSpec>(
            json!({"kind": "grid", "columns": [{"field": "a", "colour": 1}]})
        )
        .is_err());
        let g: ViewSpec = serde_json::from_value(json!({"kind": "grid"})).unwrap();
        assert_eq!(g, ViewSpec::default());
    }
}

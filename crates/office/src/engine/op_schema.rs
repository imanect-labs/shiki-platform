//! 操作列（`ops`）の各要素の入力スキーマと、その受け付け前の掃除（#859）。
//!
//! 操作ごとの項目の**形**は説明文（`describe.rs`）に書き、スキーマには**項目名だけ**を型を付けずに
//! 並べる。
//!
//! - union 型（`oneOf`）の詳細スキーマは、一部のプロバイダ（DeepSeek 等）の function 検証が拒否する。
//! - 一方で、スキーマに無い項目を出力しないモデルがある。GPT-5.6 Luna は `{"op":"add_chart"}` しか
//!   書けず、「`type` が無い」と返しても同じ形を繰り返した（説明文に項目を書いても効かない）。
//!
//! そこで全操作の項目の和集合を、型を付けない任意項目として載せる。どの項目をどの操作が使うかは
//! 項目ごとの description に書く。表（[`OpFields`]）と serde の enum が食い違わないことは、各
//! `describe.rs` のテストが serde の誤りの文面から突き合わせる。

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

/// 操作名と、その操作が受け付ける項目（`op` 以外）。並びは説明文の順と揃える。
pub type OpFields = &'static [(&'static str, &'static [&'static str])];

/// `ops` の各要素のスキーマ。`op` は列挙、それ以外の項目は型なしの任意項目。
///
/// `shared` は、どの操作にも書ける項目と、その説明（Excel の `sheet` など）。
#[must_use]
pub fn ops_items_schema(table: OpFields, shared: &[(&str, &str)]) -> Value {
    let ops: Vec<&str> = table.iter().map(|(op, _)| *op).collect();
    let mut used: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (op, fields) in table {
        for f in *fields {
            used.entry(f).or_default().push(op);
        }
    }
    let mut props = Map::new();
    props.insert("op".into(), json!({ "type": "string", "enum": ops }));
    for (field, desc) in shared {
        props.insert((*field).into(), json!({ "description": desc }));
    }
    for (field, ops) in used {
        if props.contains_key(field) {
            continue;
        }
        let desc = format!("{} で使う（形は説明文）", ops.join("・"));
        props.insert(field.into(), json!({ "description": desc }));
    }
    json!({ "type": "object", "properties": props, "required": ["op"] })
}

/// その操作が受け付けない項目のうち、**値が空のもの**（`""`・`null`・`[]`・`{}`）を落とす。
///
/// スキーマに全操作の項目を並べると、使わない項目まで空で埋めてくるモデルがある。操作の型は未知の
/// 項目を拒否する（`deny_unknown_fields`）ため、空の埋め草だけを先に取り除く。値の入った未知の
/// 項目は残し、従来どおり誤りとしてモデルへ返す（黙って捨てると書き間違いに気づけない）。
/// 未知の操作名は触らない（後段が「不明な操作」として返す）。
pub fn drop_blank_foreign(v: &mut Value, table: OpFields, shared: &[&str]) {
    let Some(obj) = v.as_object_mut() else {
        return;
    };
    let Some(op) = obj.get("op").and_then(Value::as_str) else {
        return;
    };
    let Some((_, fields)) = table.iter().find(|(name, _)| *name == op) else {
        return;
    };
    obj.retain(|k, x| {
        k == "op" || fields.contains(&k.as_str()) || shared.contains(&k.as_str()) || !is_blank(x)
    });
}

fn is_blank(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.trim().is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

/// serde の誤りの文面から、enum の操作名・操作ごとの項目を取り出す（表との突き合わせ用）。
#[cfg(test)]
pub(crate) mod probe {
    use serde::de::DeserializeOwned;
    use serde_json::json;

    /// 操作名の一覧（「unknown variant `x`, expected one of `a`, `b`」）。
    pub(crate) fn variants<T: DeserializeOwned>() -> Vec<String> {
        let msg = serde_json::from_value::<T>(json!({ "op": "zz_probe" }))
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        ticked(msg.split_once("expected").map_or("", |(_, rest)| rest))
    }

    /// 操作が受け付ける項目。`#[serde(flatten)]` を含む操作は列挙できないので `None`。
    pub(crate) fn fields<T: DeserializeOwned>(op: &str) -> Option<Vec<String>> {
        let msg = serde_json::from_value::<T>(json!({ "op": op, "zz_probe": 0 }))
            .err()?
            .to_string();
        let rest = msg.strip_prefix("unknown field `zz_probe`, ")?;
        Some(ticked(rest))
    }

    /// 項目が受け付けられる（「unknown field」と言われない）か。
    pub(crate) fn accepts<T: DeserializeOwned>(op: &str, field: &str) -> bool {
        let mut v = json!({ "op": op });
        v[field] = json!(null);
        serde_json::from_value::<T>(v)
            .err()
            .is_none_or(|e| !e.to_string().contains(&format!("unknown field `{field}`")))
    }

    fn ticked(s: &str) -> Vec<String> {
        s.split('`')
            .skip(1)
            .step_by(2)
            .map(str::to_string)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: OpFields = &[("a", &["x", "y"]), ("b", &["y", "z"]), ("c", &[])];

    #[test]
    fn schema_lists_ops_and_the_union_of_fields_without_types() {
        let s = ops_items_schema(TABLE, &[("sheet", "既定のシート")]);
        assert_eq!(s["properties"]["op"]["enum"], json!(["a", "b", "c"]));
        assert_eq!(s["required"], json!(["op"]));
        for f in ["x", "y", "z", "sheet"] {
            assert!(s["properties"][f].get("type").is_none(), "{f} は型なし");
        }
        assert_eq!(
            s["properties"]["y"]["description"],
            "a・b で使う（形は説明文）"
        );
        assert_eq!(s["properties"]["sheet"]["description"], "既定のシート");
    }

    #[test]
    fn blank_fields_of_other_ops_are_dropped_but_values_are_kept() {
        let mut v =
            json!({ "op": "a", "x": "", "y": 1, "z": "", "w": null, "q": [], "r": {}, "s": "値" });
        drop_blank_foreign(&mut v, TABLE, &[]);
        // a が受け付ける x は空でも残す（空文字に意味がある操作がある）。値の入った未知の s も残す。
        assert_eq!(v, json!({ "op": "a", "x": "", "y": 1, "s": "値" }));
    }

    #[test]
    fn shared_fields_and_unknown_ops_are_left_alone() {
        let mut v = json!({ "op": "c", "sheet": "" });
        drop_blank_foreign(&mut v, TABLE, &["sheet"]);
        assert_eq!(v, json!({ "op": "c", "sheet": "" }));
        let mut v = json!({ "op": "zz", "x": "" });
        drop_blank_foreign(&mut v, TABLE, &[]);
        assert_eq!(v, json!({ "op": "zz", "x": "" }));
    }
}

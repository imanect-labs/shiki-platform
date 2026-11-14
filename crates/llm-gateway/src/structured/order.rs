//! 出力の形（JSON Schema）を、作者が並べた項目の順でプロバイダへ書く（BUG-123・D-103・PIT-82）。
//!
//! workspace の serde_json は `preserve_order` を有効にしていない（決定キャッシュのキーなど、Map の順に頼る正規化に
//! 効くので有効にしない・PIT-82）。そのため `serde_json::Map` の値をそのまま書くと、`properties` のキーは辞書順に
//! 並ぶ。制約付きデコードは送った形の項目の順に値を書き、指示だけのモードでもモデルは示された順に書きがちなので、
//! 作者が「理由を先に、結論を後に」と並べても効かない。
//!
//! ここでは書くときだけ順を決める（値の Map は辞書順のまま）。各オブジェクトの `properties` は、同じオブジェクトの
//! `x-order`（D-60 の項目の並びの注記）に書かれたキーをその順に先に置き、書かれていないキーを後ろに今の順（保存
//! された順・辞書順）で置く（画面の並べ方と同じ）。`x-order` そのものは書かない。`x-order` の形の誤り（文字列で
//! ない・`properties` に無い・重なり）は読み飛ばす（ワークフローは保存時に止めるが、ミニアプリは任意の形を送れる）。
//! `x-order` の無い形（llm_decider の形など）は、今までと同じバイト列になる。

use std::collections::BTreeSet;

use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};
use serde_json::{Map, Value};

/// 項目の並びの注記（workflow-engine の `input_schema::ORDER` と同じ語）。プロバイダへは送らない。
pub(super) const ORDER: &str = "x-order";

/// スキーマ 1 つ。`properties` を `x-order` の順で書き、子のスキーマ（`properties` と `$defs` の値・`items`・
/// `anyOf` などの枝）にも同じ規則を当てる。ほかの値（`enum`・`const`・`required` など）はそのまま書く。
pub(super) struct OrderedSchema<'a>(pub(super) &'a Value);

impl Serialize for OrderedSchema<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let Value::Object(map) = self.0 else {
            return self.0.serialize(s);
        };
        let mut m = s.serialize_map(None)?;
        for (key, val) in map {
            match (key.as_str(), val) {
                (ORDER, _) => {}
                ("properties", Value::Object(props)) => m.serialize_entry(
                    key,
                    &SchemaMap {
                        map: props,
                        order: map.get(ORDER),
                    },
                )?,
                ("$defs" | "definitions", Value::Object(defs)) => m.serialize_entry(
                    key,
                    &SchemaMap {
                        map: defs,
                        order: None,
                    },
                )?,
                ("items" | "anyOf" | "allOf" | "oneOf" | "prefixItems", Value::Array(xs)) => {
                    m.serialize_entry(key, &SchemaList(xs))?;
                }
                ("items", v @ Value::Object(_)) => m.serialize_entry(key, &OrderedSchema(v))?,
                _ => m.serialize_entry(key, val)?,
            }
        }
        m.end()
    }
}

/// スキーマを値に持つマップ（`properties`・`$defs`）。
struct SchemaMap<'a> {
    map: &'a Map<String, Value>,
    /// 並びの注記（`x-order`）。
    order: Option<&'a Value>,
}

impl Serialize for SchemaMap<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(self.map.len()))?;
        for (key, val) in ordered_entries(self.map, self.order) {
            m.serialize_entry(key, &OrderedSchema(val))?;
        }
        m.end()
    }
}

/// スキーマの配列（`items` の配列・`anyOf` などの枝）。
struct SchemaList<'a>(&'a [Value]);

impl Serialize for SchemaList<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(self.0.len()))?;
        for v in self.0 {
            seq.serialize_element(&OrderedSchema(v))?;
        }
        seq.end()
    }
}

/// `order` に書かれたキーをその順に先に、残りを今の順に並べた項目（どのキーも 1 回だけ）。
///
/// 出したキーは集合で持つ（項目の数 n に対して O(n log n)）。要求のたびに async のタスクの中で呼ばれ、ミニアプリは
/// 任意の大きさの形を送れるので、並べ直しの手間を項目の数の 2 乗にしない。
fn ordered_entries<'a>(
    map: &'a Map<String, Value>,
    order: Option<&'a Value>,
) -> Vec<(&'a String, &'a Value)> {
    let Some(named) = order.and_then(Value::as_array) else {
        return map.iter().collect();
    };
    let mut out: Vec<(&String, &Value)> = Vec::with_capacity(map.len());
    let mut placed: BTreeSet<&str> = BTreeSet::new();
    for name in named.iter().filter_map(Value::as_str) {
        if let Some(entry) = map.get_key_value(name) {
            if placed.insert(entry.0.as_str()) {
                out.push(entry);
            }
        }
    }
    out.extend(map.iter().filter(|(k, _)| !placed.contains(k.as_str())));
    out
}

/// スキーマを作者の並びで書いた JSON の文字列（`x-order` は書かない）。
pub(super) fn to_text(schema: &Value) -> String {
    serde_json::to_string(&OrderedSchema(schema)).unwrap_or_else(|_| schema.to_string())
}

/// 本文を書く値。`at` の位置（本文の中の出力の形）だけを [`OrderedSchema`] の `schema` に置き換えて書く。
pub(super) struct Spliced<'a> {
    pub(super) value: &'a Value,
    pub(super) at: &'a [&'a str],
    pub(super) schema: &'a Value,
}

impl Serialize for Spliced<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let Some((head, rest)) = self.at.split_first() else {
            return OrderedSchema(self.schema).serialize(s);
        };
        let Value::Object(map) = self.value else {
            return self.value.serialize(s);
        };
        let mut m = s.serialize_map(Some(map.len()))?;
        for (key, val) in map {
            if key == head {
                m.serialize_entry(
                    key,
                    &Spliced {
                        value: val,
                        at: rest,
                        schema: self.schema,
                    },
                )?;
            } else {
                m.serialize_entry(key, val)?;
            }
        }
        m.end()
    }
}

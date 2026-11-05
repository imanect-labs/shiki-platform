//! serde で読む型（IR の全体・ブロック・線・きっかけ・ブロックの設定）の誤りを、できるだけまとめて位置つきで集める
//! （保存時検証 V1・V1b・GAP-35）。
//!
//! serde は最初の誤りで止まるので、誤りの場所を直して読み直すことを、読めるまで（上限まで）繰り返す。
//! - 使えない項目・型の違う項目は、その項目を外す（配列の要素は外すと後ろの番号がずれるので、配列を持つ項目ごと外す）。
//! - 項目が無い誤りは、その項目に仮の値を入れて読み続ける（入れられる仮の値が無ければ、入れ物ごと外す）。
//!   外した項目が必須だったときに起きる「項目が無い」誤りは、外したことで起きたものなので数えない。
//!
//! 誤りの無い値は最初の 1 回で読めるので、読み方も結果も変わらない（集めるのは読めなかったときだけ）。

use serde::de::DeserializeOwned;
use serde_json::Value;

/// 位置の 1 段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Seg {
    Key(String),
    Index(usize),
}

/// 誤り 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Fault {
    /// 誤りの位置（値の根からの段）。項目が無い誤りは、その項目を入れる入れ物の位置。
    pub(crate) at: Vec<Seg>,
    /// serde の文言（英語の原文・呼ぶ側が利用者向けの文に直す）。
    pub(crate) raw: String,
}

/// 読み直しの回数の上限（1 つの値あたり）。
const MAX_ROUNDS: usize = 64;

/// serde を通す回数の残り（読み直しと、仮の値を試すたびに 1 つ使う）。尽きたら集めるのを止める。
///
/// 1 つの値の中では [`MAX_ROUNDS`] が上限だが、値がたくさんある（ブロックを何十万個も並べた IR）と合計が膨らむ。
/// 呼ぶ側が全体で 1 つの残りを渡し、合計を抑える（保存時検証 V1・GAP-35）。
#[derive(Debug)]
pub(crate) struct Budget(usize);

impl Budget {
    pub(crate) fn new(parses: usize) -> Self {
        Budget(parses)
    }

    /// 1 回使う（残りが無ければ false）。
    fn take(&mut self) -> bool {
        if self.0 == 0 {
            return false;
        }
        self.0 -= 1;
        true
    }

    /// 残りが無い。
    pub(crate) fn exhausted(&self) -> bool {
        self.0 == 0
    }
}

/// 仮の値（項目が無い誤りのあと読み続けるため・型の合うものを前から試す）。
fn placeholders() -> [Value; 6] {
    [
        Value::String(String::new()),
        Value::from(1),
        Value::Array(Vec::new()),
        Value::Object(serde_json::Map::new()),
        Value::Bool(false),
        Value::Null,
    ]
}

/// 位置を JSON Pointer（`/a/0/b`・RFC 6901 の `~0` `~1`）にする。根は空文字。
pub(crate) fn pointer(at: &[Seg]) -> String {
    let mut out = String::new();
    for seg in at {
        out.push('/');
        match seg {
            Seg::Key(k) => out.push_str(&k.replace('~', "~0").replace('/', "~1")),
            Seg::Index(i) => out.push_str(&i.to_string()),
        }
    }
    out
}

/// `value` を `T` として読み、誤りを最大 `max` 件集める（読めれば空）。
pub(crate) fn faults<T: DeserializeOwned>(value: &Value, max: usize) -> Vec<Fault> {
    // 1 回の読み直しで、読むのに 1 回と仮の値を試すのに最大 placeholders の数だけ使う。
    let mut budget = Budget::new(MAX_ROUNDS * (1 + placeholders().len()));
    faults_within::<T>(value, max, &mut budget)
}

/// [`faults`] と同じだが、serde を通す回数を `budget` から使う（尽きたらそこまでの誤りを返す）。
pub(crate) fn faults_within<T: DeserializeOwned>(
    value: &Value,
    max: usize,
    budget: &mut Budget,
) -> Vec<Fault> {
    let mut doc = value.clone();
    let mut out: Vec<Fault> = Vec::new();
    // 外した項目の位置（外したことで起きる「項目が無い」誤りを数えないため）。
    let mut removed: Vec<Vec<Seg>> = Vec::new();
    for _ in 0..MAX_ROUNDS {
        if !budget.take() {
            break;
        }
        let Some(f) = first::<T>(&doc) else { break };
        if let Some(name) = field_of(&f.raw, "missing field ") {
            let mut full = f.at.clone();
            full.push(Seg::Key(name.clone()));
            let consequence = removed.contains(&full);
            if !consequence {
                out.push(f.clone());
                if out.len() >= max {
                    break;
                }
            }
            // 仮の値で埋めて読み続ける（仮の値は、その項目の誤りが消えるものだけを使う）。仮の値の入らない項目は
            // 入れ物ごと外して、後ろの項目を読み続ける。
            if !fill::<T>(&mut doc, &f.at, &name, budget) {
                match remove(&mut doc, &f.at) {
                    Some(r) => removed.push(r),
                    None => break,
                }
            }
            continue;
        }
        let mut at = f.at.clone();
        // 使えない項目の名前を位置が含まない（タグで分ける型は中の位置を追えない）ときは、名前を足す。
        if let Some(name) = field_of(&f.raw, "unknown field ") {
            if at.last() != Some(&Seg::Key(name.clone())) {
                at.push(Seg::Key(name));
            }
        }
        out.push(Fault {
            at: at.clone(),
            raw: f.raw,
        });
        if out.len() >= max {
            break;
        }
        match remove(&mut doc, &at) {
            Some(r) => removed.push(r),
            None => break,
        }
    }
    out
}

/// 最初の誤り（読めれば `None`）。
fn first<T: DeserializeOwned>(doc: &Value) -> Option<Fault> {
    let mut track = serde_path_to_error::Track::new();
    let de = serde_path_to_error::Deserializer::new(doc, &mut track);
    match T::deserialize(de) {
        Ok(_) => None,
        Err(e) => Some(Fault {
            at: segs(&track.path()),
            raw: e.to_string(),
        }),
    }
}

/// serde_path_to_error の位置を段の列へ（分からない段から先は捨てる）。
fn segs(path: &serde_path_to_error::Path) -> Vec<Seg> {
    use serde_path_to_error::Segment;
    let mut out = Vec::new();
    for seg in path {
        match seg {
            Segment::Map { key } => out.push(Seg::Key(key.clone())),
            Segment::Enum { variant } => out.push(Seg::Key(variant.clone())),
            Segment::Seq { index } => out.push(Seg::Index(*index)),
            Segment::Unknown => break,
        }
    }
    out
}

/// serde の文言の `` `name` ``（`prefix` で始まるときだけ）。
pub(crate) fn field_of(raw: &str, prefix: &str) -> Option<String> {
    let rest = raw.strip_prefix(prefix)?;
    let start = rest.find('`')? + 1;
    let end = start + rest[start..].find('`')?;
    Some(rest[start..end].to_string())
}

fn get_mut<'a>(doc: &'a mut Value, at: &[Seg]) -> Option<&'a mut Value> {
    let mut cur = doc;
    for seg in at {
        cur = match (seg, cur) {
            (Seg::Key(k), Value::Object(m)) => m.get_mut(k)?,
            (Seg::Index(i), Value::Array(a)) => a.get_mut(*i)?,
            _ => return None,
        };
    }
    Some(cur)
}

/// 位置の項目を外す（いちばん深い「オブジェクトの項目」の段で外す）。外した項目の位置を返す。
fn remove(doc: &mut Value, at: &[Seg]) -> Option<Vec<Seg>> {
    for k in (0..at.len()).rev() {
        let Seg::Key(key) = &at[k] else { continue };
        if let Some(Value::Object(m)) = get_mut(doc, &at[..k]) {
            if m.remove(key).is_some() {
                return Some(at[..=k].to_vec());
            }
        }
    }
    None
}

/// 無い項目に仮の値を入れる（その項目の誤りが消える仮の値が見つかれば true）。`budget` が尽きたら false。
fn fill<T: DeserializeOwned>(
    doc: &mut Value,
    container: &[Seg],
    name: &str,
    budget: &mut Budget,
) -> bool {
    let mut full = container.to_vec();
    full.push(Seg::Key(name.to_string()));
    for candidate in placeholders() {
        if !budget.take() {
            break;
        }
        let Some(Value::Object(m)) = get_mut(doc, container) else {
            return false;
        };
        m.insert(name.to_string(), candidate);
        let still = first::<T>(doc).is_some_and(|f| {
            f.at.starts_with(&full)
                || (f.at == container
                    && field_of(&f.raw, "missing field ").as_deref() == Some(name))
        });
        if !still {
            return true;
        }
    }
    if let Some(Value::Object(m)) = get_mut(doc, container) {
        m.remove(name);
    }
    false
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use serde_json::json;

    use super::*;

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct Inner {
        key: String,
        #[serde(default)]
        n: u32,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct Outer {
        name: String,
        #[serde(default)]
        count: u32,
        #[serde(default)]
        items: Vec<Inner>,
        #[serde(default)]
        tags: Vec<String>,
    }

    fn found(v: &Value) -> Vec<(String, String)> {
        faults::<Outer>(v, 20)
            .into_iter()
            .map(|f| (pointer(&f.at), f.raw))
            .collect()
    }

    #[test]
    fn a_readable_value_has_no_faults() {
        assert!(found(&json!({ "name": "a", "items": [{ "key": "k" }] })).is_empty());
    }

    /// 使えない項目・型の違う項目・無い必須の項目を、位置つきでまとめて返す。
    #[test]
    fn faults_are_collected_with_their_positions() {
        let got = found(&json!({
            "bogus": 1, "count": "x", "extra": true,
            "items": [{ "key": "k", "m": 1 }]
        }));
        let at: std::collections::BTreeSet<&str> = got.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(got.len(), 5, "{got:?}");
        assert_eq!(
            at,
            ["", "/bogus", "/count", "/extra", "/items/0/m"]
                .into_iter()
                .collect(),
            "{got:?}"
        );
        // 項目が無い誤りは、ほかの項目を読み終えてから出る（入れ物の位置・項目の名前は文言に）。
        assert_eq!(got[4].0, "");
        assert!(got[4].1.starts_with("missing field `name`"), "{got:?}");
    }

    /// 外した必須の項目の「項目が無い」誤りは数えない（型の違う name は 1 件だけ）。
    #[test]
    fn removing_a_required_field_is_not_reported_twice() {
        let got = found(&json!({ "name": 5, "bogus": 1 }));
        let mut at: Vec<&str> = got.iter().map(|(p, _)| p.as_str()).collect();
        at.sort_unstable();
        assert_eq!(at, vec!["/bogus", "/name"], "{got:?}");
    }

    /// 配列の要素の誤りは、配列を持つ項目ごと外して先へ進む（後ろの要素の番号をずらさない）。
    #[test]
    fn a_fault_in_an_array_element_drops_the_array() {
        let got = found(&json!({ "name": "a", "tags": ["a", 1, 2], "zzz": 1 }));
        let mut at: Vec<&str> = got.iter().map(|(p, _)| p.as_str()).collect();
        at.sort_unstable();
        assert_eq!(at, vec!["/tags/1", "/zzz"], "{got:?}");
    }

    #[test]
    fn the_limit_is_kept() {
        let v = json!({ "a": 1, "b": 2, "c": 3, "d": 4, "name": "x" });
        assert_eq!(faults::<Outer>(&v, 2).len(), 2);
    }

    /// 呼ぶ側が渡した回数の残りが尽きたら、そこまでの誤りで止める（値がたくさんあっても合計を抑える）。
    #[test]
    fn the_shared_budget_stops_collecting() {
        let v = json!({ "a": 1, "b": 2, "c": 3, "d": 4, "name": "x" });
        let mut budget = Budget::new(3);
        assert_eq!(faults_within::<Outer>(&v, 20, &mut budget).len(), 3);
        assert!(budget.exhausted());
        // 尽きた残りでは 1 回も読まない。
        assert!(faults_within::<Outer>(&v, 20, &mut budget).is_empty());
        // 無い項目に仮の値を試す回数も数える（読む 1 回・name に "" を試す 1 回・読み直す 1 回）。
        let mut budget = Budget::new(100);
        let got = faults_within::<Outer>(&json!({}), 20, &mut budget);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(budget.0, 97);
    }

    #[test]
    fn pointers_escape_keys() {
        assert_eq!(
            pointer(&[
                Seg::Key("a/b".into()),
                Seg::Index(2),
                Seg::Key("c~d".into())
            ]),
            "/a~1b/2/c~0d"
        );
        assert_eq!(pointer(&[]), "");
    }
}

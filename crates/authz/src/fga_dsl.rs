//! `.fga` DSL ↔ `.json` の **userset 構造まで含めた等価比較**（テスト専用）。
//!
//! `authorization-model.fga`（人がレビューする正本）と `authorization-model.json`
//! （OpenFGA へ実投入）は二正本構成のため、type 名だけでなく relation の
//! userset ルール本体（`computedUserset` / `tupleToUserset` / 直接付与型・
//! ワイルドカード）が乖離すると authz チョークポイントに穴が空く（#66）。
//!
//! ここでは両者を共通の中間表現 [`Userset`] に正規化し、構造的に完全一致する
//! ことを CI（`cargo test`）で保証する。外部ツール・ネットワークに依存しない
//! hermetic な検査で、`.fga` 側だけに公開ワイルドカード `[user:*]` や余分な
//! 継承が紛れ込む drift を素通りさせない。
//!
//! 対応する OpenFGA schema 1.1 の集合演算: 直接付与(`this`) / `computedUserset` /
//! `tupleToUserset` / `union` / `intersection` / `difference`。本モデルは union と
//! 直接付与のみだが、将来 relation が増えても drift を捕捉できるよう全演算を扱う。
//! （DSL パーサは括弧なし・トップレベル単一演算子の文法を前提とする。現行モデルは
//! この範囲に収まり、逸脱した場合はパースが失敗して気付ける。）

use std::collections::BTreeMap;

use serde_json::Value;

/// 直接付与できる subject 型（DSL の `[...]` / JSON の `directly_related_user_types`）。
///
/// 例: `user` → `{user}` / `role#member` → `{role, member}` / `user:*` → `{user, *}` /
/// `user with is_valid` → 条件付き（条件名 `is_valid`）。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Assignable {
    /// subject の object type 名（例 `user`, `role`）。
    ty: String,
    /// userset 参照の relation（`role#member` の `member`）。直接 subject なら `None`。
    relation: Option<String>,
    /// 公開ワイルドカード `type:*`（`[user:*]`）。誤混入は最も危険な drift。
    wildcard: bool,
    /// 付与に付く条件名（OpenFGA condition・DSL `... with <cond>` / JSON `condition`）。
    /// 条件は実効権限を左右するため、その乖離も drift として捕捉する。無条件なら `None`。
    condition: Option<String>,
}

/// relation の userset ルール本体（`.fga` と `.json` の共通中間表現）。
///
/// 集合演算の子は正規化のためソート済みで保持し、表層順序の差で誤検知しない。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Userset {
    /// 直接付与（DSL の `[...]` / JSON の `this`）。付与可能型はソート済み。
    This(Vec<Assignable>),
    /// 同一オブジェクト上の別 relation を含意（DSL の裸の名前 / JSON の `computedUserset`）。
    Computed(String),
    /// 親経由の継承（DSL `X from Y` / JSON の `tupleToUserset`）。`Y#..` の各対象の `X`。
    Ttu { tupleset: String, computed: String },
    /// 和集合（DSL `A or B` / JSON `union`）。子はソート済み。
    Union(Vec<Userset>),
    /// 積集合（DSL `A and B` / JSON `intersection`）。子はソート済み。
    Intersection(Vec<Userset>),
    /// 差集合（DSL `A but not B` / JSON `difference`）。順序を持つ。
    Exclusion {
        base: Box<Userset>,
        subtract: Box<Userset>,
    },
}

/// モデル全体の正規化表現: `type 名 → (relation 名 → userset)`。
/// relation を持たない type（例 `user`）も空マップとして保持し、type の増減も捕捉する。
type Model = BTreeMap<String, BTreeMap<String, Userset>>;

// ---- .fga DSL パーサ ----------------------------------------------------

/// `.fga` DSL を [`Model`] へパースする。
///
/// 文法（本モデルの範囲）: `type <name>` ブロック内の `relations` 配下に
/// `define <rel>: <expr>` が並ぶ。`<expr>` は括弧なし・トップレベル単一演算子。
fn parse_fga(src: &str) -> Model {
    let mut model = Model::new();
    let mut current: Option<String> = None;
    for raw in src.lines() {
        let line = raw.trim();
        // 全行コメント・空行は捨てる。`role#member` の `#` を切らないため、
        // 行頭が `#` のときのみコメント扱いにする（行内トレイリングコメントは持たない）。
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("type ") {
            let name = rest.split_whitespace().next().unwrap_or("").to_string();
            current = Some(name.clone());
            model.entry(name).or_default();
        } else if let Some(rest) = line.strip_prefix("define ") {
            let (rel, body) = rest
                .split_once(':')
                .unwrap_or_else(|| panic!("define 行に ':' が無い: {line}"));
            let type_name = current
                .as_ref()
                .unwrap_or_else(|| panic!("type ブロック外の define: {line}"));
            let rels = model.entry(type_name.clone()).or_default();
            rels.insert(rel.trim().to_string(), parse_expr(body.trim()));
        }
        // `model` / `schema 1.1` / `relations` などのマーカ行は無視する。
    }
    model
}

/// userset 式（`define` の右辺）をパースする。トップレベルの単一演算子で分岐する。
fn parse_expr(expr: &str) -> Userset {
    let expr = expr.trim();
    if let Some((base, subtract)) = split_top(expr, " but not ") {
        // `but not` は二項の差集合。左右をさらに再帰的にパースする。
        return Userset::Exclusion {
            base: Box::new(parse_expr(base)),
            subtract: Box::new(parse_expr(subtract)),
        };
    }
    if expr.contains(" or ") {
        return Userset::Union(sorted_children(expr, " or "));
    }
    if expr.contains(" and ") {
        return Userset::Intersection(sorted_children(expr, " and "));
    }
    parse_term(expr)
}

/// `sep` で式を分割し、各項を [`parse_term`] してソート済み子リストにする。
fn sorted_children(expr: &str, sep: &str) -> Vec<Userset> {
    let mut children: Vec<Userset> = expr.split(sep).map(|t| parse_term(t.trim())).collect();
    children.sort();
    children
}

/// `sep` の最初の出現で二分割する（`but not` 用）。無ければ `None`。
fn split_top<'a>(expr: &'a str, sep: &str) -> Option<(&'a str, &'a str)> {
    expr.split_once(sep).map(|(l, r)| (l.trim(), r.trim()))
}

/// 単項（`[...]` / `X from Y` / 裸の relation 名）をパースする。
///
/// 括弧なし・トップレベル単一演算子の文法前提のため、ここに残る空白は未対応の
/// 複合式（`viewer and editor` 等）を意味する。裸 relation として黙って受理せず
/// panic し、モジュール契約（逸脱は失敗させる）を守る（drift 見逃し防止）。
fn parse_term(term: &str) -> Userset {
    let term = term.trim();
    if let Some(inner) = term.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        return Userset::This(parse_typelist(inner));
    }
    if let Some((computed, tupleset)) = term.split_once(" from ") {
        return Userset::Ttu {
            tupleset: ident(tupleset),
            computed: ident(computed),
        };
    }
    Userset::Computed(ident(term))
}

/// relation 名が単一トークン（空白なし・非空）であることを確認して返す。
/// 空白が残る＝未対応の複合演算子が紛れているため即 panic する。
fn ident(name: &str) -> String {
    let name = name.trim();
    assert!(
        !name.is_empty() && !name.contains(char::is_whitespace),
        "未対応の userset 式（複合演算子か空 relation）: {name:?}"
    );
    name.to_string()
}

/// `[...]` の中身（`user, role#member, user:*, user with cond`）を
/// 付与可能型のソート済みリストへ。各項の末尾 `with <cond>` は条件名として取り込む。
fn parse_typelist(inner: &str) -> Vec<Assignable> {
    let mut out: Vec<Assignable> = inner
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|entry| {
            // 末尾の `... with <condition>` を先に切り出す（残りが type 部）。
            let (subject, condition) = match entry.split_once(" with ") {
                Some((s, cond)) => (s.trim(), Some(ident(cond))),
                None => (entry, None),
            };
            if let Some((ty, wild)) = subject.split_once(':') {
                assert_eq!(
                    wild.trim(),
                    "*",
                    "`:` を含む付与型はワイルドカードのみ: {entry}"
                );
                Assignable {
                    ty: ident(ty),
                    relation: None,
                    wildcard: true,
                    condition,
                }
            } else if let Some((ty, rel)) = subject.split_once('#') {
                Assignable {
                    ty: ident(ty),
                    relation: Some(ident(rel)),
                    wildcard: false,
                    condition,
                }
            } else {
                Assignable {
                    ty: ident(subject),
                    relation: None,
                    wildcard: false,
                    condition,
                }
            }
        })
        .collect();
    out.sort();
    out
}

// ---- .json 正規化 -------------------------------------------------------

/// OpenFGA authorization model JSON を [`Model`] へ正規化する。
fn normalize_json(model: &Value) -> Model {
    let mut out = Model::new();
    let types = model
        .get("type_definitions")
        .and_then(Value::as_array)
        .expect("type_definitions は配列");
    for t in types {
        let name = t
            .get("type")
            .and_then(Value::as_str)
            .expect("type 名は文字列")
            .to_string();
        let metadata = t.get("metadata");
        let mut rels = BTreeMap::new();
        if let Some(relations) = t.get("relations").and_then(Value::as_object) {
            for (rel, rewrite) in relations {
                rels.insert(rel.clone(), parse_json_rewrite(rewrite, metadata, rel));
            }
        }
        out.insert(name, rels);
    }
    out
}

/// JSON rewrite が持ちうる userset 種別キー（schema 1.1）。ちょうど 1 つだけ持つ。
const USERSET_KINDS: [&str; 6] = [
    "this",
    "computedUserset",
    "tupleToUserset",
    "union",
    "intersection",
    "difference",
];

/// JSON の rewrite ツリーを [`Userset`] へ。`this` の付与型は metadata から引く。
///
/// rewrite は種別キーをちょうど 1 つだけ持つ（one-of）ことを先に検証する。手編集で
/// `this` と `union` を同居させるといった不正 JSON を、最初の分岐だけ見て素通り
/// させない（drift 見逃し防止）。
fn parse_json_rewrite(v: &Value, metadata: Option<&Value>, rel: &str) -> Userset {
    let present = USERSET_KINDS
        .iter()
        .filter(|k| v.get(**k).is_some())
        .count();
    assert_eq!(present, 1, "userset は種別キーをちょうど 1 つ持つこと: {v}");
    if v.get("this").is_some() {
        return Userset::This(assignable_from_metadata(metadata, rel));
    }
    if let Some(cu) = v.get("computedUserset") {
        return Userset::Computed(relation_of(cu));
    }
    if let Some(ttu) = v.get("tupleToUserset") {
        return Userset::Ttu {
            tupleset: relation_of(ttu.get("tupleset").expect("tupleset を持つ")),
            computed: relation_of(ttu.get("computedUserset").expect("computedUserset を持つ")),
        };
    }
    if let Some(u) = v.get("union") {
        return Userset::Union(sorted_json_children(u, metadata, rel));
    }
    if let Some(i) = v.get("intersection") {
        return Userset::Intersection(sorted_json_children(i, metadata, rel));
    }
    if let Some(d) = v.get("difference") {
        return Userset::Exclusion {
            base: Box::new(parse_json_rewrite(
                d.get("base").expect("difference は base を持つ"),
                metadata,
                rel,
            )),
            subtract: Box::new(parse_json_rewrite(
                d.get("subtract").expect("difference は subtract を持つ"),
                metadata,
                rel,
            )),
        };
    }
    panic!("未知の userset 種別: {v}");
}

/// `union`/`intersection` の `child` 配列を再帰パースしてソートする。
fn sorted_json_children(op: &Value, metadata: Option<&Value>, rel: &str) -> Vec<Userset> {
    let mut children: Vec<Userset> = op
        .get("child")
        .and_then(Value::as_array)
        .expect("child は配列")
        .iter()
        .map(|c| parse_json_rewrite(c, metadata, rel))
        .collect();
    children.sort();
    children
}

/// `{"relation": "owner"}` から relation 名を取り出す。
fn relation_of(v: &Value) -> String {
    v.get("relation")
        .and_then(Value::as_str)
        .expect("relation は文字列")
        .to_string()
}

/// metadata の `directly_related_user_types` を付与可能型のソート済みリストへ。
fn assignable_from_metadata(metadata: Option<&Value>, rel: &str) -> Vec<Assignable> {
    let mut out: Vec<Assignable> = metadata
        .and_then(|m| m.get("relations"))
        .and_then(|r| r.get(rel))
        .and_then(|r| r.get("directly_related_user_types"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|e| Assignable {
                    ty: e
                        .get("type")
                        .and_then(Value::as_str)
                        .expect("付与型 type は文字列")
                        .to_string(),
                    relation: e
                        .get("relation")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    wildcard: e.get("wildcard").is_some(),
                    condition: e
                        .get("condition")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

#[cfg(test)]
mod tests;

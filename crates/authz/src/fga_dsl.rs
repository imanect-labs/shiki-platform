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
/// 例: `user` → `{user}` / `role#member` → `{role, member}` / `user:*` → `{user, *}`。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Assignable {
    /// subject の object type 名（例 `user`, `role`）。
    ty: String,
    /// userset 参照の relation（`role#member` の `member`）。直接 subject なら `None`。
    relation: Option<String>,
    /// 公開ワイルドカード `type:*`（`[user:*]`）。誤混入は最も危険な drift。
    wildcard: bool,
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
fn parse_term(term: &str) -> Userset {
    let term = term.trim();
    if let Some(inner) = term.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        return Userset::This(parse_typelist(inner));
    }
    if let Some((computed, tupleset)) = term.split_once(" from ") {
        return Userset::Ttu {
            tupleset: tupleset.trim().to_string(),
            computed: computed.trim().to_string(),
        };
    }
    Userset::Computed(term.to_string())
}

/// `[...]` の中身（`user, role#member, user:*`）を付与可能型のソート済みリストへ。
fn parse_typelist(inner: &str) -> Vec<Assignable> {
    let mut out: Vec<Assignable> = inner
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|entry| {
            if let Some((ty, wild)) = entry.split_once(':') {
                assert_eq!(
                    wild.trim(),
                    "*",
                    "`:` を含む付与型はワイルドカードのみ: {entry}"
                );
                Assignable {
                    ty: ty.trim().to_string(),
                    relation: None,
                    wildcard: true,
                }
            } else if let Some((ty, rel)) = entry.split_once('#') {
                Assignable {
                    ty: ty.trim().to_string(),
                    relation: Some(rel.trim().to_string()),
                    wildcard: false,
                }
            } else {
                Assignable {
                    ty: entry.to_string(),
                    relation: None,
                    wildcard: false,
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

/// JSON の rewrite ツリーを [`Userset`] へ。`this` の付与型は metadata から引く。
fn parse_json_rewrite(v: &Value, metadata: Option<&Value>, rel: &str) -> Userset {
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
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FGA: &str = include_str!("../model/authorization-model.fga");

    /// #66 の本丸: `.fga` と `.json` が **userset ルール本体まで**構造的に一致すること。
    /// type 名の増減だけでなく、`editor`/`viewer` の継承や公開ワイルドカードの
    /// 紛れ込みといった relation 定義の乖離を CI で落とす。
    #[test]
    fn fga_and_json_are_structurally_equivalent() {
        let dsl = parse_fga(FGA);
        let json = normalize_json(&crate::model::default_model());
        assert_eq!(
            dsl, json,
            ".fga と .json の userset 構造が乖離しています（#66 drift 検出）\n\
             左=.fga（人がレビューする正本） 右=.json（OpenFGA へ投入）"
        );
    }

    /// パーサが本モデルの userset 種別を正しく写せることの明示確認（正の足場）。
    #[test]
    fn parses_known_folder_relations() {
        let dsl = parse_fga(FGA);
        let folder = dsl.get("folder").expect("folder 型");
        // parent: [folder]（直接付与のみ）。
        assert_eq!(
            folder.get("parent"),
            Some(&Userset::This(vec![Assignable {
                ty: "folder".into(),
                relation: None,
                wildcard: false,
            }]))
        );
        // editor: [user, role#member] or owner or editor from parent（union の 3 子）。
        let editor = folder.get("editor").expect("folder.editor");
        let Userset::Union(children) = editor else {
            panic!("editor は union: {editor:?}");
        };
        assert_eq!(children.len(), 3);
        assert!(children.contains(&Userset::Computed("owner".into())));
        assert!(children.contains(&Userset::Ttu {
            tupleset: "parent".into(),
            computed: "editor".into(),
        }));
    }

    /// role#member が付与型として双方で一致すること（共有語彙 #76 の要）。
    #[test]
    fn role_member_assignable_matches_on_both_sides() {
        let dsl = parse_fga(FGA);
        let json = normalize_json(&crate::model::default_model());
        for ty in ["folder", "file"] {
            for rel in ["editor", "viewer"] {
                let d = &dsl[ty][rel];
                let j = &json[ty][rel];
                assert_eq!(d, j, "{ty}.{rel} の付与型/継承が一致すること");
            }
        }
    }

    /// 負例: `.json` の `file.viewer` に公開ワイルドカード `[user:*]` が紛れ込むと
    /// 検査が乖離を検出すること（現状の type 名・relation 名検査は素通りする穴）。
    #[test]
    fn detects_injected_public_wildcard_in_json() {
        let mut model = crate::model::default_model();
        // file.viewer の directly_related_user_types に user:* を追加する。
        let types = model
            .get_mut("type_definitions")
            .and_then(Value::as_array_mut)
            .unwrap();
        let file = types
            .iter_mut()
            .find(|t| t.get("type").and_then(Value::as_str) == Some("file"))
            .unwrap();
        let drut = file
            .get_mut("metadata")
            .and_then(|m| m.get_mut("relations"))
            .and_then(|r| r.get_mut("viewer"))
            .and_then(|r| r.get_mut("directly_related_user_types"))
            .and_then(Value::as_array_mut)
            .unwrap();
        drut.push(serde_json::json!({ "type": "user", "wildcard": {} }));

        let dsl = parse_fga(FGA);
        let json = normalize_json(&model);
        assert_ne!(dsl, json, "公開ワイルドカードの紛れ込みを検出すること");
    }

    /// 負例: `.json` の継承 relation を差し替えると検出すること
    /// （`viewer from parent` を `editor from parent` へ改竄）。
    #[test]
    fn detects_altered_inheritance_in_json() {
        let mut model = crate::model::default_model();
        let types = model
            .get_mut("type_definitions")
            .and_then(Value::as_array_mut)
            .unwrap();
        let folder = types
            .iter_mut()
            .find(|t| t.get("type").and_then(Value::as_str) == Some("folder"))
            .unwrap();
        // folder.viewer の union 内 tupleToUserset.computedUserset を editor へ改竄。
        let children = folder
            .get_mut("relations")
            .and_then(|r| r.get_mut("viewer"))
            .and_then(|v| v.get_mut("union"))
            .and_then(|u| u.get_mut("child"))
            .and_then(Value::as_array_mut)
            .unwrap();
        for child in children.iter_mut() {
            if let Some(ttu) = child.get_mut("tupleToUserset") {
                ttu["computedUserset"]["relation"] = Value::String("editor".into());
            }
        }
        let dsl = parse_fga(FGA);
        let json = normalize_json(&model);
        assert_ne!(dsl, json, "継承 relation の改竄を検出すること");
    }

    /// パーサ単体: union の子は表層順序に依らず正規化（ソート）されること。
    #[test]
    fn union_children_are_order_independent() {
        let a = parse_expr("owner or viewer or editor from parent");
        let b = parse_expr("editor from parent or viewer or owner");
        assert_eq!(a, b, "union は順序非依存で等価");
    }

    /// パーサ単体: intersection / but not（差集合）を正しく写すこと。
    #[test]
    fn parses_intersection_and_exclusion() {
        assert_eq!(
            parse_expr("editor and viewer"),
            Userset::Intersection(vec![
                Userset::Computed("editor".into()),
                Userset::Computed("viewer".into()),
            ])
        );
        assert_eq!(
            parse_expr("viewer but not owner"),
            Userset::Exclusion {
                base: Box::new(Userset::Computed("viewer".into())),
                subtract: Box::new(Userset::Computed("owner".into())),
            }
        );
    }

    /// パーサ単体: ワイルドカードと userset 参照の付与型を区別して写すこと。
    #[test]
    fn parses_wildcard_and_userset_assignables() {
        let Userset::This(types) = parse_term("[user, role#member, user:*]") else {
            panic!("This を期待");
        };
        assert!(types.contains(&Assignable {
            ty: "user".into(),
            relation: None,
            wildcard: false,
        }));
        assert!(types.contains(&Assignable {
            ty: "role".into(),
            relation: Some("member".into()),
            wildcard: false,
        }));
        assert!(types.contains(&Assignable {
            ty: "user".into(),
            relation: None,
            wildcard: true,
        }));
    }
}

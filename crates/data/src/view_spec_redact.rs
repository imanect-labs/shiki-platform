//! 閲覧者に返す表示設定の絞り込み（検証 2-B-P2-1）。
//!
//! 保存ビューの表示設定には、**保存者**が読める列の参照名が入っている。閲覧者が読めない（マスクされた）
//! 列の名前をそのまま返すと、列の存在と名前が分かる（列の存在オラクル・data-platform §16.1）。
//! 実行結果（`DataViewRun`・ミニアプリの ```data_grid）に添える表示設定は、閲覧者に読めない列を指す
//! 箇所を外してから返す。行・列の値そのものは `run_query` が閲覧者本人の権限で既に絞っている。

use std::collections::HashSet;

use crate::query::ir::Condition;
use crate::view_spec::ViewSpec;

/// 条件が読めない列を参照するか。
fn condition_touches(c: &Condition, masked: &HashSet<String>) -> bool {
    match c {
        Condition::All(v) | Condition::Any(v) => v.iter().any(|x| condition_touches(x, masked)),
        Condition::Not(x) => condition_touches(x, masked),
        Condition::Cmp(cmp) => masked.contains(&cmp.field),
        Condition::Link(l) => masked.contains(&l.field),
    }
}

impl ViewSpec {
    /// 閲覧者に読めない列を指す箇所を外した表示設定。
    ///
    /// 列設定・並べ替えの代替列・グループ化・カードの列は外し、条件付き書式は読めない列を条件に使う規則を
    /// 落とす（塗る列だけが読めない規則は、その列を外す。全部外れたら規則ごと落とす）。カンバンの分け方・
    /// カレンダーの日付・見出しの列が読めないときは空にする（その場合の実行は読めない列の並べ替えとして
    /// 拒否されるので、通常は返らない）。
    #[must_use]
    pub fn redact_for(mut self, masked: &HashSet<String>) -> ViewSpec {
        if masked.is_empty() {
            return self;
        }
        let keep = |k: &String| !masked.contains(k);
        match &mut self {
            ViewSpec::Grid(g) => {
                g.columns.retain(|c| keep(&c.field));
                for c in &mut g.columns {
                    if c.sort_by.as_ref().is_some_and(|k| !keep(k)) {
                        c.sort_by = None;
                    }
                }
                g.group_by.retain(|x| keep(&x.field));
                g.conditional_format.retain_mut(|r| {
                    if condition_touches(&r.condition, masked) {
                        return false;
                    }
                    if r.fields.is_empty() {
                        return true;
                    }
                    r.fields.retain(keep);
                    !r.fields.is_empty()
                });
            }
            ViewSpec::Kanban(k) => {
                if !keep(&k.group_field) {
                    k.group_field.clear();
                }
                k.card_fields.retain(keep);
            }
            ViewSpec::Calendar(c) => {
                if !keep(&c.date_field) {
                    c.date_field.clear();
                }
                if c.title_field.as_ref().is_some_and(|k| !keep(k)) {
                    c.title_field = None;
                }
            }
            ViewSpec::Gallery(g) => {
                if g.title_field.as_ref().is_some_and(|k| !keep(k)) {
                    g.title_field = None;
                }
                g.card_fields.retain(keep);
            }
        }
        self
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn masked_names_never_survive() {
        let spec: ViewSpec = serde_json::from_value(json!({
            "kind": "grid",
            "columns": [
                {"field": "title"},
                {"field": "salary", "label": "給与"},
                {"field": "name", "sort_by": "salary"}
            ],
            "group_by": [{"field": "salary"}, {"field": "dept"}],
            "conditional_format": [
                {"condition": {"cmp": {"field": "salary", "op": "gt", "value": 1}}, "color": "red"},
                {"condition": {"cmp": {"field": "dept", "op": "eq", "value": "a"}}, "color": "blue", "fields": ["salary"]},
                {"condition": {"cmp": {"field": "dept", "op": "eq", "value": "b"}}, "color": "green", "fields": ["salary", "title"]},
                {"condition": {"not": {"cmp": {"field": "dept", "op": "exists"}}}, "color": "gray"}
            ]
        }))
        .unwrap();
        let masked: HashSet<String> = ["salary".to_string()].into();
        let out = spec.redact_for(&masked);
        let text = serde_json::to_string(&out).unwrap();
        assert!(!text.contains("salary"), "{text}");
        let ViewSpec::Grid(g) = out else { panic!() };
        assert_eq!(g.columns.len(), 2);
        assert_eq!(g.group_by.len(), 1);
        assert_eq!(g.conditional_format.len(), 2);
        let gal: ViewSpec = serde_json::from_value(json!({
            "kind": "gallery", "title_field": "salary", "card_fields": ["salary", "title"]
        }))
        .unwrap();
        assert!(!serde_json::to_string(&gal.redact_for(&masked))
            .unwrap()
            .contains("salary"));
    }
}

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashSet;

use serde_json::json;

use super::*;
use crate::model::{FieldDef, LookupDef, TableSchema};
use crate::query::ir::{CondCmp, LinkCond, SortKey};

fn f(key: &str, ty: FieldType) -> FieldDef {
    FieldDef::new(key, ty)
}

fn schema() -> TableSchema {
    let mut status = f("status", FieldType::Select);
    status.options = vec!["open".into(), "done".into()];
    let mut total = f("total", FieldType::Rollup);
    total.materialization = Some(Materialization::PerViewer);
    let mut age = f("age", FieldType::Formula);
    age.materialization = Some(Materialization::Volatile);
    let mut lk = f("cust_name", FieldType::Lookup);
    lk.lookup = Some(LookupDef {
        via_field: "cust".into(),
        target_field: "name".into(),
    });
    TableSchema {
        fields: vec![
            f("name", FieldType::Text),
            f("kana", FieldType::Text),
            status,
            f("due", FieldType::Date),
            f("salary", FieldType::Number),
            f("tags", FieldType::MultiSelect),
            total,
            age,
            lk,
        ],
        status_field: None,
        row_policy: None,
        field_policy: vec![],
        aggregate_min_rows: None,
        fsm_ref: None,
        rag: None,
    }
}

fn masked() -> HashSet<String> {
    HashSet::from(["salary".to_string()])
}

fn check(spec: &ViewSpec) -> Result<(), DataError> {
    let s = schema();
    let m = masked();
    ViewCheck {
        schema: &s,
        masked: &m,
        max_lookup_columns: DEFAULT_MAX_VIEW_LOOKUP_COLUMNS,
    }
    .check(spec)
}

fn col(field: &str) -> ColumnConfig {
    ColumnConfig {
        field: field.into(),
        label: None,
        width: None,
        hidden: false,
        frozen: false,
        sort_by: None,
    }
}

fn grid(columns: Vec<ColumnConfig>) -> ViewSpec {
    ViewSpec::Grid(GridView {
        columns,
        ..GridView::default()
    })
}

#[test]
fn json_shape_is_tagged_by_kind_and_defaults_to_grid() {
    let spec: ViewSpec = serde_json::from_value(json!({
        "kind": "grid",
        "columns": [{ "field": "name", "label": "氏名", "width": 200, "frozen": true }],
        "row_height": "tall",
    }))
    .unwrap();
    let ViewSpec::Grid(g) = &spec else {
        panic!("grid");
    };
    assert_eq!(g.columns[0].label.as_deref(), Some("氏名"));
    assert_eq!(g.row_height, RowHeight::Tall);
    // 往復しても同じ（kind タグで種別が決まる）。
    let back = serde_json::to_value(&spec).unwrap();
    assert_eq!(back["kind"], "grid");
    assert_eq!(serde_json::from_value::<ViewSpec>(back).unwrap(), spec);
    // 未知の種別・未知のキーは型で弾く（サーバ非解釈の JSON を受けない）。
    assert!(serde_json::from_value::<ViewSpec>(json!({"kind": "form"})).is_err());
    assert!(serde_json::from_value::<ViewSpec>(json!({"kind": "list"})).is_err());
    assert!(serde_json::from_value::<ViewSpec>(json!({"kind": "grid", "colour": "red"})).is_err());
    let k: ViewSpec = serde_json::from_value(json!({
        "kind": "kanban", "group_field": "status", "card_fields": ["name"]
    }))
    .unwrap();
    assert!(matches!(k, ViewSpec::Kanban(_)));
    assert_eq!(ViewSpec::default(), ViewSpec::Grid(GridView::default()));
}

#[test]
fn unknown_fields_are_rejected_everywhere() {
    assert!(matches!(
        check(&grid(vec![col("nope")])),
        Err(DataError::FieldUnavailable)
    ));
    let mut c = col("name");
    c.sort_by = Some("nope".into());
    assert!(matches!(
        check(&grid(vec![c])),
        Err(DataError::FieldUnavailable)
    ));
    let spec = ViewSpec::Kanban(KanbanView {
        group_field: "status".into(),
        card_fields: vec!["nope".into()],
        hide_empty: false,
    });
    assert!(matches!(check(&spec), Err(DataError::FieldUnavailable)));
    // 擬似列は指せる。
    assert!(check(&grid(vec![col("_created_at"), col("_owner")])).is_ok());
    // 非表示の存在しない列はマスク列と同じく許し、見出しを付けたらマスク列と同じ 403（列の存在オラクルを作らない）。
    let mut hidden = col("nope");
    hidden.hidden = true;
    assert!(check(&grid(vec![hidden.clone()])).is_ok());
    hidden.label = Some("x".into());
    assert!(matches!(
        check(&grid(vec![hidden])),
        Err(DataError::FieldUnavailable)
    ));
}

#[test]
fn masked_columns_cannot_be_displayed_sorted_or_used_in_conditions() {
    // 表示は 403（存在しない列と同じ `field_unavailable`）。
    assert!(matches!(
        check(&grid(vec![col("salary")])),
        Err(DataError::FieldUnavailable)
    ));
    // 非表示にした列設定は何も見せないので許す（見出し・並べ替えの代替を付けたら 403）。
    let mut hidden = col("salary");
    hidden.hidden = true;
    assert!(check(&grid(vec![hidden.clone()])).is_ok());
    hidden.label = Some("給与".into());
    assert!(matches!(
        check(&grid(vec![hidden])),
        Err(DataError::FieldUnavailable)
    ));
    // 並べ替えの代替列・グループ化・条件付き書式・カードにも出せない。
    let mut c = col("name");
    c.sort_by = Some("salary".into());
    assert!(matches!(
        check(&grid(vec![c])),
        Err(DataError::FieldUnavailable)
    ));
    let spec = ViewSpec::Grid(GridView {
        group_by: vec![GroupConfig {
            field: "salary".into(),
            descending: false,
            collapsed: false,
        }],
        ..GridView::default()
    });
    assert!(matches!(check(&spec), Err(DataError::FieldUnavailable)));
    let spec = ViewSpec::Grid(GridView {
        conditional_format: vec![ConditionalFormat {
            condition: Condition::cmp("salary", CondOp::Gt, Some(json!(100))),
            color: FormatColor::Red,
            fields: vec![],
            bold: false,
        }],
        ..GridView::default()
    });
    assert!(matches!(check(&spec), Err(DataError::FieldUnavailable)));
    let spec = ViewSpec::Gallery(GalleryView {
        title_field: Some("salary".into()),
        card_fields: vec![],
    });
    assert!(matches!(check(&spec), Err(DataError::FieldUnavailable)));
}

#[test]
fn viewer_dependent_and_volatile_columns_cannot_order_or_group() {
    let s = schema();
    let m = masked();
    let vc = ViewCheck {
        schema: &s,
        masked: &m,
        max_lookup_columns: 12,
    };
    // クエリの並べ替え。
    assert!(matches!(
        vc.check_sort_keys(["total"]),
        Err(DataError::Rejected(_))
    ));
    assert!(matches!(
        vc.check_sort_keys(["age"]),
        Err(DataError::Rejected(_))
    ));
    assert!(matches!(
        vc.check_sort_keys(["tags"]),
        Err(DataError::Rejected(_))
    ));
    assert!(vc.check_sort_keys(["name", "_updated_at"]).is_ok());
    // 並べ替えの代替列・グループ化。
    let mut c = col("name");
    c.sort_by = Some("total".into());
    assert!(matches!(check(&grid(vec![c])), Err(DataError::Rejected(_))));
    let spec = ViewSpec::Grid(GridView {
        group_by: vec![GroupConfig {
            field: "age".into(),
            descending: false,
            collapsed: false,
        }],
        ..GridView::default()
    });
    assert!(matches!(check(&spec), Err(DataError::Rejected(_))));
    // 表示するだけなら閲覧者ごとの列も出せる（値は閲覧者本人の権限で読取時に計算される）。
    assert!(check(&grid(vec![col("total"), col("age")])).is_ok());
    // 読み仮名列での並べ替えの代替は通る。
    let mut c = col("name");
    c.sort_by = Some("kana".into());
    assert!(check(&grid(vec![c])).is_ok());
}

#[test]
fn column_config_shape_is_validated() {
    let mut c = col("name");
    c.width = Some(10);
    assert!(check(&grid(vec![c])).is_err());
    let mut c = col("name");
    c.label = Some("改\n行".into());
    assert!(check(&grid(vec![c])).is_err());
    let mut c = col("name");
    c.label = Some(String::new());
    assert!(check(&grid(vec![c])).is_err());
    assert!(check(&grid(vec![col("name"), col("name")])).is_err());
    // 固定列は先頭から連続（非表示の列は数えない）。
    let mut a = col("name");
    a.frozen = true;
    let b = col("kana");
    let mut c = col("status");
    c.frozen = true;
    assert!(check(&grid(vec![a.clone(), b.clone(), c])).is_err());
    let mut hidden = col("due");
    hidden.hidden = true;
    let mut a2 = col("status");
    a2.frozen = true;
    assert!(check(&grid(vec![a, hidden, a2, b])).is_ok());
}

#[test]
fn conditional_format_is_a_closed_condition_tree() {
    let fmt = |condition| {
        ViewSpec::Grid(GridView {
            conditional_format: vec![ConditionalFormat {
                condition,
                color: FormatColor::Green,
                fields: vec!["status".into()],
                bold: true,
            }],
            ..GridView::default()
        })
    };
    assert!(check(&fmt(Condition::All(vec![
        Condition::cmp("status", CondOp::Eq, Some(json!("done"))),
        Condition::Not(Box::new(Condition::cmp("due", CondOp::IsNull, None))),
    ])))
    .is_ok());
    // matches・リンク条件・値なしの比較・巨大な in は拒否。
    assert!(check(&fmt(Condition::cmp(
        "name",
        CondOp::Matches,
        Some(json!(".*"))
    )))
    .is_err());
    assert!(check(&fmt(Condition::Link(LinkCond {
        field: "cust".into(),
        filter: None,
    })))
    .is_err());
    assert!(check(&fmt(Condition::Cmp(CondCmp {
        field: "name".into(),
        op: CondOp::Eq,
        value: None,
    })))
    .is_err());
    let big: Vec<_> = (0..101).map(|i| json!(i)).collect();
    assert!(check(&fmt(Condition::cmp("name", CondOp::In, Some(json!(big))))).is_err());
    // 深すぎる入れ子は拒否。
    let mut deep = Condition::cmp("name", CondOp::Exists, None);
    for _ in 0..=MAX_CONDITION_DEPTH {
        deep = Condition::Not(Box::new(deep));
    }
    assert!(check(&fmt(deep)).is_err());
}

#[test]
fn kanban_and_calendar_require_typed_columns() {
    let k = |g: &str| {
        ViewSpec::Kanban(KanbanView {
            group_field: g.into(),
            card_fields: vec!["name".into()],
            hide_empty: true,
        })
    };
    assert!(check(&k("status")).is_ok());
    assert!(check(&k("name")).is_err());
    let c = |d: &str| {
        ViewSpec::Calendar(CalendarView {
            date_field: d.into(),
            title_field: Some("name".into()),
        })
    };
    assert!(check(&c("due")).is_ok());
    assert!(check(&c("_created_at")).is_ok());
    assert!(check(&c("_owner")).is_err());
    assert!(check(&c("name")).is_err());
}

#[test]
fn projection_follows_order_and_hidden_columns() {
    let s = schema();
    let mut hidden = col("cust_name");
    hidden.hidden = true;
    let spec = grid(vec![col("status"), hidden, col("_owner")]);
    let p = spec.projection(&s);
    // 設定の順 → 設定に無い列はスキーマの順。非表示と擬似列は取らない。
    assert_eq!(p[0], "status");
    assert!(!p.contains(&"cust_name".to_string()));
    assert!(!p.contains(&"_owner".to_string()));
    assert!(p.contains(&"salary".to_string()));
    let k = ViewSpec::Kanban(KanbanView {
        group_field: "status".into(),
        card_fields: vec!["name".into(), "due".into()],
        hide_empty: false,
    });
    assert_eq!(k.projection(&s), vec!["status", "name", "due"]);
}

#[test]
fn lookup_budget_counts_only_lookup_and_rollup_columns() {
    let mut fields = vec![f("name", FieldType::Text)];
    for i in 0..13 {
        let mut lk = f(&format!("l{i}"), FieldType::Lookup);
        lk.lookup = Some(LookupDef {
            via_field: "cust".into(),
            target_field: "name".into(),
        });
        fields.push(lk);
    }
    let s = TableSchema { fields, ..schema() };
    let m = HashSet::new();
    let vc = |cap| ViewCheck {
        schema: &s,
        masked: &m,
        max_lookup_columns: cap,
    };
    let all: Vec<String> = s.fields.iter().map(|f| f.key.clone()).collect();
    assert!(matches!(
        vc(12).check_lookup_budget(&all),
        Err(DataError::Rejected(_))
    ));
    assert!(vc(13).check_lookup_budget(&all).is_ok());
    assert!(vc(12).check_lookup_budget(&all[..13]).is_ok());
}

#[test]
fn effective_sort_puts_group_keys_first() {
    let spec = ViewSpec::Grid(GridView {
        group_by: vec![GroupConfig {
            field: "status".into(),
            descending: true,
            collapsed: false,
        }],
        ..GridView::default()
    });
    let sort = vec![
        SortKey {
            field: "due".into(),
            descending: false,
        },
        SortKey {
            field: "status".into(),
            descending: false,
        },
    ];
    let got = crate::view::effective_sort(&spec, &sort);
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].field, "status");
    assert!(got[0].descending);
    assert_eq!(got[1].field, "due");
}

#[test]
fn stored_v1_display_reads_as_default_grid() {
    use crate::view::DataViewBody;
    let id = uuid::Uuid::nil();
    // v1 の display（サーバ非解釈の任意 JSON）は既定の表として読む（クエリはそのまま）。
    for legacy in [
        json!({"kind": "list"}),
        json!({}),
        json!(null),
        json!("chart"),
    ] {
        let b = DataViewBody::from_stored(json!({
            "table_id": id, "query": {"limit": 5}, "display": legacy
        }))
        .unwrap();
        assert_eq!(b.display, ViewSpec::default());
        assert_eq!(b.query.limit, Some(5));
    }
    // display が無い本文も既定の表。
    let b = DataViewBody::from_stored(json!({"table_id": id, "query": {}})).unwrap();
    assert_eq!(b.display, ViewSpec::default());
    // 型付きの display はそのまま読む。
    let b = DataViewBody::from_stored(json!({
        "table_id": id, "query": {}, "display": {"kind": "gallery", "card_fields": ["name"]}
    }))
    .unwrap();
    assert!(matches!(b.display, ViewSpec::Gallery(_)));
    // display 以外の破損は誤り。
    assert!(DataViewBody::from_stored(json!({"query": {}, "display": {}})).is_err());
    // 書込の入力は厳密（v1 の display は受けない）。
    assert!(serde_json::from_value::<DataViewBody>(json!({
        "table_id": id, "query": {}, "display": {"kind": "list"}
    }))
    .is_err());
}

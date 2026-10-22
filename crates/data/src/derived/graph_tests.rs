use super::*;
use crate::derived::types::{LinkCardinality, LinkDef, RollupDef};
use crate::model::{FieldPolicy, LookupDef};
use crate::policy::{PolicyExpr, RowPolicy};
use serde_json::json;

fn schema(fields: Vec<FieldDef>) -> TableSchema {
    TableSchema {
        fields,
        status_field: None,
        row_policy: None,
        field_policy: vec![],
        aggregate_min_rows: None,
        fsm_ref: None,
    }
}

fn f(id: u16, key: &str, ty: FieldType) -> FieldDef {
    let mut d = FieldDef::new(key, ty);
    d.id = id;
    d
}

fn link(id: u16, key: &str, to: Uuid) -> FieldDef {
    let mut d = f(id, key, FieldType::RecordRef);
    d.ref_table = Some(to);
    d.link = Some(LinkDef {
        cardinality: LinkCardinality::ManyToMany,
        symmetric_field: None,
        symmetric_key: None,
        reverse: false,
    });
    d
}

fn rollup(id: u16, key: &str, link: &str, target: Option<&str>, op: RollupOp) -> FieldDef {
    let mut d = f(id, key, FieldType::Rollup);
    d.rollup = Some(RollupDef {
        link: link.into(),
        target: target.map(Into::into),
        op,
    });
    d
}

fn formula(id: u16, key: &str, e: serde_json::Value) -> FieldDef {
    let mut d = f(id, key, FieldType::Formula);
    d.formula = Some(serde_json::from_value(e).unwrap());
    d
}

const APP: Uuid = Uuid::from_u128(0xa);

fn entry(s: TableSchema) -> CatalogTable {
    CatalogTable {
        schema: s,
        app_id: Some(APP),
        has_record_shares: false,
        keys: TableKeys::default(),
    }
}

/// 案件（A）→ 明細（B）の多対多。B に金額。
fn fixture() -> (Catalog, Uuid, Uuid) {
    let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
    let mut cat = Catalog::default();
    cat.tables.insert(
        a,
        entry(schema(vec![
            link(16, "items", b),
            rollup(17, "total", "items", Some("amount"), RollupOp::Sum),
            formula(
                18,
                "with_tax",
                json!({"mul": [{"field": "total"}, {"lit": 1.1}]}),
            ),
            formula(
                19,
                "age",
                json!({"date_diff": ["now", {"lit": "2026-01-01"}]}),
            ),
            rollup(20, "n", "items", None, RollupOp::Count),
        ])),
    );
    cat.tables.insert(
        b,
        entry(schema(vec![
            f(16, "amount", FieldType::Number),
            f(17, "name", FieldType::Text),
        ])),
    );
    (cat, a, b)
}

#[test]
fn invariant_requires_all_four_layers() {
    let (mut cat, a, b) = fixture();
    let contained: HashSet<Uuid> = [b].into();
    assert_eq!(
        cat.materialization(a, "total", &contained),
        Materialization::Invariant
    );
    // 数式は依存を引き継ぐ。
    assert_eq!(
        cat.materialization(a, "with_tax", &contained),
        Materialization::Invariant
    );
    // `now` を含めば volatile。
    assert_eq!(
        cat.materialization(a, "age", &contained),
        Materialization::Volatile
    );
    // ① viewer 集合の包含を証明できない（既定）→ per_viewer。
    assert_eq!(
        cat.materialization(a, "total", &HashSet::new()),
        Materialization::PerViewer
    );
    // ① 同一ミニアプリでない。
    cat.tables.get_mut(&b).unwrap().app_id = None;
    assert_eq!(
        cat.materialization(a, "total", &contained),
        Materialization::PerViewer
    );
    cat.tables.get_mut(&b).unwrap().app_id = Some(APP);
    // ② 行述語。
    cat.tables.get_mut(&b).unwrap().schema.row_policy = Some(RowPolicy {
        read: PolicyExpr::IsOwner,
        write: None,
    });
    assert_eq!(
        cat.materialization(a, "total", &contained),
        Materialization::PerViewer
    );
    assert_eq!(
        cat.materialization(a, "with_tax", &contained),
        Materialization::PerViewer,
        "数式は推移的に per_viewer"
    );
    cat.tables.get_mut(&b).unwrap().schema.row_policy = None;
    // ③ 対象列のマスク（count は対象列を読まないので影響しない）。
    cat.tables.get_mut(&b).unwrap().schema.field_policy = vec![FieldPolicy {
        field: "amount".into(),
        readable_by: PolicyExpr::Public,
    }];
    assert_eq!(
        cat.materialization(a, "total", &contained),
        Materialization::PerViewer
    );
    assert_eq!(
        cat.materialization(a, "n", &contained),
        Materialization::Invariant
    );
    cat.tables.get_mut(&b).unwrap().schema.field_policy = vec![];
    // ④ レコード個別共有。
    cat.tables.get_mut(&b).unwrap().has_record_shares = true;
    assert_eq!(
        cat.materialization(a, "n", &contained),
        Materialization::PerViewer
    );
}

#[test]
fn local_mask_and_multi_hop_are_per_viewer() {
    let (mut cat, a, b) = fixture();
    let contained: HashSet<Uuid> = [b].into();
    // 自テーブルのマスク列を材料にする数式は per_viewer。
    let mut s = cat.tables[&a].schema.clone();
    s.fields.push(f(21, "salary", FieldType::Number));
    s.fields.push(formula(
        22,
        "annual",
        json!({"mul": [{"field": "salary"}, {"lit": 12}]}),
    ));
    s.field_policy = vec![FieldPolicy {
        field: "salary".into(),
        readable_by: PolicyExpr::Public,
    }];
    cat.tables.get_mut(&a).unwrap().schema = s;
    assert_eq!(
        cat.materialization(a, "annual", &contained),
        Materialization::PerViewer
    );
    // 参照先の派生列を対象にする（多段）は per_viewer。
    let mut bs = cat.tables[&b].schema.clone();
    bs.fields.push(formula(
        18,
        "double",
        json!({"mul": [{"field": "amount"}, {"lit": 2}]}),
    ));
    cat.tables.get_mut(&b).unwrap().schema = bs;
    let mut s = cat.tables[&a].schema.clone();
    s.fields
        .push(rollup(23, "dsum", "items", Some("double"), RollupOp::Sum));
    cat.tables.get_mut(&a).unwrap().schema = s;
    assert_eq!(
        cat.materialization(a, "dsum", &contained),
        Materialization::PerViewer
    );
    // 対称列（逆リンク）越しは per_viewer。
    let mut s = cat.tables[&a].schema.clone();
    let mut back = link(24, "back", b);
    back.link.as_mut().unwrap().reverse = true;
    s.fields.push(back);
    s.fields
        .push(rollup(25, "bn", "back", None, RollupOp::Count));
    cat.tables.get_mut(&a).unwrap().schema = s;
    assert_eq!(
        cat.materialization(a, "bn", &contained),
        Materialization::PerViewer
    );
}

#[test]
fn cycles_are_rejected_locally_and_across_tables() {
    let (mut cat, a, _) = fixture();
    assert!(cat.check_cycles(a).is_ok());
    // 自テーブル内: x → y → x。
    let mut s = cat.tables[&a].schema.clone();
    s.fields.push(formula(
        30,
        "x",
        json!({"add": [{"field": "y"}, {"lit": 1}]}),
    ));
    s.fields.push(formula(
        31,
        "y",
        json!({"add": [{"field": "x"}, {"lit": 1}]}),
    ));
    cat.tables.get_mut(&a).unwrap().schema = s;
    let err = cat.check_cycles(a).unwrap_err();
    assert!(matches!(err, DataError::Invalid(m) if m.contains("循環")));

    // テーブルを跨ぐ: A.total → B.back_total → A.total。
    let (mut cat, a, b) = fixture();
    let mut bs = cat.tables[&b].schema.clone();
    bs.fields.push(link(18, "projects", a));
    bs.fields.push(rollup(
        19,
        "back_total",
        "projects",
        Some("total"),
        RollupOp::Sum,
    ));
    cat.tables.get_mut(&b).unwrap().schema = bs;
    let mut s = cat.tables[&a].schema.clone();
    s.fields.retain(|f| f.key != "total");
    s.fields.push(rollup(
        17,
        "total",
        "items",
        Some("back_total"),
        RollupOp::Sum,
    ));
    cat.tables.get_mut(&a).unwrap().schema = s;
    assert!(cat.check_cycles(a).is_err());
    // 自己参照の数式。
    let mut cat = Catalog::default();
    cat.tables.insert(
        a,
        entry(schema(vec![formula(16, "me", json!({"field": "me"}))])),
    );
    assert!(cat.check_cycles(a).is_err());
}

#[test]
fn types_are_inferred_and_rollups_checked() {
    let (cat, a, _) = fixture();
    assert_eq!(cat.infer(a, "total"), VType::Number);
    assert_eq!(cat.infer(a, "with_tax"), VType::Number);
    assert_eq!(cat.infer(a, "n"), VType::Number);
    assert!(cat
        .check_rollup(a, "x", "items", Some("amount"), RollupOp::Sum)
        .is_ok());
    // 文字列の合計は 422。
    assert!(cat
        .check_rollup(a, "x", "items", Some("name"), RollupOp::Sum)
        .is_err());
    assert!(cat
        .check_rollup(a, "x", "items", Some("name"), RollupOp::Max)
        .is_ok());
    assert!(cat
        .check_rollup(a, "x", "items", Some("missing"), RollupOp::Max)
        .is_err());
    assert!(cat
        .check_rollup(a, "x", "items", None, RollupOp::Sum)
        .is_err());
    assert!(cat
        .check_rollup(a, "x", "nope", None, RollupOp::Count)
        .is_err());
}

#[test]
fn lookup_type_depends_on_link_cardinality() {
    let (mut cat, a, b) = fixture();
    let mut s = cat.tables[&a].schema.clone();
    let mut one = link(40, "main", b);
    one.link.as_mut().unwrap().cardinality = LinkCardinality::ManyToOne;
    s.fields.push(one);
    let mut lk = f(41, "main_name", FieldType::Lookup);
    lk.lookup = Some(LookupDef {
        via_field: "main".into(),
        target_field: "name".into(),
    });
    s.fields.push(lk);
    let mut lk2 = f(42, "item_names", FieldType::Lookup);
    lk2.lookup = Some(LookupDef {
        via_field: "items".into(),
        target_field: "name".into(),
    });
    s.fields.push(lk2);
    cat.tables.get_mut(&a).unwrap().schema = s;
    assert_eq!(cat.infer(a, "main_name"), VType::Text);
    assert_eq!(cat.infer(a, "item_names"), VType::List);
}

#[test]
fn eval_order_puts_dependencies_first() {
    let s = schema(vec![
        formula(16, "c", json!({"add": [{"field": "b"}, {"lit": 1}]})),
        formula(17, "b", json!({"add": [{"field": "a"}, {"lit": 1}]})),
        f(18, "a", FieldType::Number),
    ]);
    let order: Vec<&str> = eval_order(&s).iter().map(|f| f.key.as_str()).collect();
    assert_eq!(order, vec!["b", "c"]);
}

#[test]
fn materialized_refs_follow_formula_dependencies() {
    let (cat, a, b) = fixture();
    assert_eq!(
        cat.materialized_refs(a, "with_tax"),
        vec![(b, "items".to_string())]
    );
    assert!(cat.materialized_refs(a, "age").is_empty());
}

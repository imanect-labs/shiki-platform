//! 列を書く位置ごとの規則（[`super::check_field_use`]・[`super::check_link_field`]）が、実行時の束縛（`bind_query`）と
//! 同じ答えになることの単体テスト（BUG-244・GAP-129）。
#![allow(clippy::unwrap_used, clippy::panic)]

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::{json, Value};
use uuid::Uuid;

use super::*;
use crate::derived::types::{LinkCardinality, LinkDef};
use crate::model::{DataTable, FieldDef, FieldType, TableKeys};
use crate::policy::material::PolicyMaterial;
use crate::query::bound::{bind_query, bind_query_scoped, BindOpts};
use crate::query::ir::{Condition, DataQuery, LinkCond, Metric, MetricSpec, SortKey};
use crate::query::scopes::{ScopeTable, ScopeTables};

/// 索引あり・重複を許さない・どちらも無し・検索だけ・数・多値・作成者・リンク・対称列・読めない列の表。
fn schema() -> TableSchema {
    let field = |key: &str, ty: FieldType, set: &dyn Fn(&mut FieldDef)| {
        let mut f = FieldDef::new(key, ty);
        set(&mut f);
        f
    };
    let parent = Uuid::from_u128(7);
    let mut s = TableSchema {
        fields: vec![
            field("code", FieldType::Text, &|f| f.unique = true),
            field("name", FieldType::Text, &|f| {
                f.indexed = true;
                f.searchable = true;
            }),
            field("memo", FieldType::Text, &|_| {}),
            field("note", FieldType::Text, &|f| f.searchable = true),
            field("qty", FieldType::Number, &|f| f.indexed = true),
            field("price", FieldType::Number, &|_| {}),
            field("serial", FieldType::Number, &|f| f.unique = true),
            field("tags", FieldType::MultiSelect, &|f| {
                f.indexed = true;
                f.options = vec!["a".into()];
            }),
            field("author", FieldType::CreatedBy, &|_| {}),
            field("parent", FieldType::RecordRef, &|f| {
                f.indexed = true;
                f.ref_table = Some(parent);
            }),
            field("children", FieldType::RecordRef, &|f| {
                f.indexed = true;
                f.ref_table = Some(parent);
                f.link = Some(LinkDef {
                    cardinality: LinkCardinality::OneToMany,
                    symmetric_field: None,
                    symmetric_key: None,
                    symmetric_display_name: None,
                    reverse: true,
                });
            }),
            field("secret", FieldType::Text, &|f| f.indexed = true),
        ],
        status_field: None,
        row_policy: None,
        field_policy: vec![],
        aggregate_min_rows: None,
        fsm_ref: None,
        rag: None,
    };
    for (i, f) in s.fields.iter_mut().enumerate() {
        f.id = 10 + u16::try_from(i).unwrap();
    }
    s
}

/// 比べる値（列の型に合う値・値の誤りで束縛が先に断らないもの）。
fn value_for(s: &TableSchema, key: &str, op: CondOp) -> Option<Value> {
    if matches!(op, CondOp::Exists | CondOp::IsNull) {
        return None;
    }
    let one = match s.field(key).map(|f| f.field_type) {
        Some(FieldType::Number) => json!(1),
        Some(FieldType::RecordRef) => json!(Uuid::from_u128(9).to_string()),
        _ => json!("a"),
    };
    Some(if op == CondOp::In { json!([one]) } else { one })
}

/// 列を `at` に書いたクエリ（実行時の束縛に渡すもの）。
fn query(s: &TableSchema, key: &str, at: FieldUse) -> DataQuery {
    let count = MetricSpec {
        metric: Metric::Count,
        field: None,
    };
    match at {
        FieldUse::Filter(op) => DataQuery {
            filter: Some(Condition::cmp(key, op, value_for(s, key, op))),
            ..DataQuery::default()
        },
        FieldUse::Sort { keys } => DataQuery {
            // 2 つ目からは、ほかのシステムの列（同じ列を重ねると、列に依らない重なりの誤りになる）。
            sort: std::iter::once(key)
                .chain(
                    ["_created_at", "_owner", "_updated_at"]
                        .into_iter()
                        .filter(|k| *k != key),
                )
                .take(keys)
                .map(|k| SortKey {
                    field: k.into(),
                    descending: false,
                })
                .collect(),
            ..DataQuery::default()
        },
        FieldUse::GroupBy => DataQuery {
            group_by: vec![key.into()],
            aggregate: vec![count],
            ..DataQuery::default()
        },
        FieldUse::Metric => DataQuery {
            aggregate: vec![MetricSpec {
                metric: Metric::Sum,
                field: Some(key.into()),
            }],
            ..DataQuery::default()
        },
    }
}

/// 誤りの比べる形（403 か、400 のコード・列・文言）。
fn answer<T>(r: &Result<T, DataError>) -> Option<String> {
    r.as_ref().err().map(|e| match e {
        DataError::FieldUnavailable => "field_unavailable".to_string(),
        DataError::InvalidInput(ie) => {
            format!("{:?} {:?} {}", ie.code(), ie.field_key(), ie.message())
        }
        other => format!("{other:?}"),
    })
}

/// 保存時の照合（読めない列を外した定義で問う）と、実行時の束縛（読めない列をマスクして束縛する）は、どの列・どの
/// 位置・どの演算子でも同じ答え（通すか、同じ誤り）になる。索引あり・重複を許さない・どちらも無し・対称列・
/// システムの列を含む。
#[test]
fn the_save_check_answers_like_the_binder() {
    let full = schema();
    let masked: HashSet<String> = ["secret".to_string()].into();
    let mut viewer = full.clone();
    viewer.fields.retain(|f| !masked.contains(&f.key));
    let keys: Vec<&str> = full
        .fields
        .iter()
        .map(|f| f.key.as_str())
        .chain(["_owner", "_created_at", "_updated_at", "_id", "nope"])
        .collect();
    let ops = [
        CondOp::Eq,
        CondOp::Neq,
        CondOp::In,
        CondOp::Gt,
        CondOp::Lte,
        CondOp::StartsWith,
        CondOp::Contains,
        CondOp::EndsWith,
        CondOp::Exists,
        CondOp::IsNull,
    ];
    let uses: Vec<FieldUse> = ops
        .into_iter()
        .map(FieldUse::Filter)
        .chain([
            FieldUse::Sort { keys: 1 },
            FieldUse::Sort { keys: 2 },
            FieldUse::GroupBy,
            FieldUse::Metric,
        ])
        .collect();
    let mut refused = 0;
    for key in &keys {
        for at in &uses {
            let runtime = bind_query(&full, &masked, &query(&full, key, *at));
            let saved = check_field_use(&viewer, key, *at);
            assert_eq!(answer(&saved), answer(&runtime), "{key} {at:?}");
            refused += usize::from(saved.is_err());
        }
    }
    assert!(refused > 0);
}

/// 代表の答え: 絞り込み・並べ替え・まとめる列・集計に使えるのは、索引のある列か重複を許さない列（と作成者の列・
/// システムの列）だけ。対称列は使えない。部分一致は検索の索引のある列だけ。システムの列はまとめる列に使えない。
#[test]
fn indexed_or_unique_columns_are_the_ones_a_query_can_use() {
    let s = schema();
    let code = |key: &str, at: FieldUse| match check_field_use(&s, key, at) {
        Ok(()) => "ok".to_string(),
        Err(DataError::FieldUnavailable) => "unavailable".to_string(),
        Err(e) => e
            .invalid_code()
            .map_or("?", InvalidCode::as_str)
            .to_string(),
    };
    let eq = FieldUse::Filter(CondOp::Eq);
    for key in [
        "code",
        "name",
        "qty",
        "serial",
        "author",
        "_created_at",
        "_owner",
    ] {
        assert_eq!(code(key, eq), "ok", "{key}");
        assert_eq!(code(key, FieldUse::Sort { keys: 1 }), "ok", "{key}");
    }
    for key in ["memo", "note", "price"] {
        for at in [eq, FieldUse::Sort { keys: 1 }, FieldUse::GroupBy] {
            assert_eq!(code(key, at), "field_not_indexed", "{key} {at:?}");
        }
    }
    assert_eq!(code("price", FieldUse::Metric), "field_not_indexed");
    assert_eq!(code("qty", FieldUse::Metric), "ok");
    assert_eq!(code("serial", FieldUse::Metric), "ok");
    assert_eq!(code("code", FieldUse::Metric), "value_type_mismatch");
    assert_eq!(code("children", eq), "field_not_sortable");
    assert_eq!(code("_created_at", FieldUse::GroupBy), "field_not_sortable");
    assert_eq!(
        code("tags", FieldUse::Sort { keys: 1 }),
        "field_not_sortable"
    );
    assert_eq!(code("note", FieldUse::Filter(CondOp::Contains)), "ok");
    assert_eq!(
        code("code", FieldUse::Filter(CondOp::Contains)),
        "field_not_indexed"
    );
    assert_eq!(code("nope", eq), "unavailable");
}

/// 参照先のテーブル（`schema()` のリンクの列 `parent`・`children` が指す表）。定義は `schema()` と同じで、`secret` を
/// 読む人にマスクして開いたもの（実行時の束縛が使う材料・`load_scope_tables` と同じ形）。
fn scope_tables(target: &TableSchema, masked: &HashSet<String>) -> ScopeTables {
    let ts = chrono::DateTime::from_timestamp(0, 0).unwrap_or_default();
    let mut out = ScopeTables::default();
    out.insert(ScopeTable {
        table: DataTable {
            id: Uuid::from_u128(7),
            name: "parent".into(),
            app_id: None,
            schema: target.clone(),
            schema_version: 1,
            created_by: "u".into(),
            created_at: ts,
            updated_at: ts,
            add_only: false,
            narrowed: None,
            keys: TableKeys::default(),
            has_record_shares: false,
            planner_fixed: false,
        },
        masked: masked.clone(),
        material: Arc::new(PolicyMaterial::default()),
    });
    out
}

/// リンクの条件・結合を含むクエリを、参照先を開いた実行時の束縛に通す。
fn bind_linked(
    full: &TableSchema,
    masked: &HashSet<String>,
    q: &DataQuery,
) -> Result<super::super::bound::BoundQuery, DataError> {
    let tables = scope_tables(full, masked);
    bind_query_scoped(full, masked, q, &tables, BindOpts::default())
}

/// 読む人の見え方の定義（`secret` を外したもの）と、照らす列（定義の列・システムの列・無い列）。
fn viewer_and_keys() -> (TableSchema, HashSet<String>, TableSchema, Vec<String>) {
    let full = schema();
    let masked: HashSet<String> = ["secret".to_string()].into();
    let mut viewer = full.clone();
    viewer.fields.retain(|f| !masked.contains(&f.key));
    let keys = full
        .fields
        .iter()
        .map(|f| f.key.clone())
        .chain(["_owner", "_created_at", "_id", "nope"].map(String::from))
        .collect();
    (full, masked, viewer, keys)
}

/// 辿った先の比較に使う演算子（索引・部分一致・大小・多値の規則が分かれるもの）。
const LINKED_OPS: [CondOp; 6] = [
    CondOp::Eq,
    CondOp::Gt,
    CondOp::In,
    CondOp::StartsWith,
    CondOp::Contains,
    CondOp::Exists,
];

/// リンクの条件（`link`）で辿る列と、その中の比較の列は、保存時の照合（読めない列を外した定義で問う・中の列は参照先の
/// 定義で問う）と、実行時の束縛（参照先を読む人の権限で開いて束縛する）で、どの列・どの演算子でも同じ答え（通すか、
/// 同じ誤り）になる（GAP-129）。
#[test]
fn link_columns_answer_like_the_binder() {
    let (full, masked, viewer, keys) = viewer_and_keys();
    let link = |field: &str, filter: Option<Condition>| DataQuery {
        filter: Some(Condition::Link(LinkCond {
            field: field.into(),
            filter: filter.map(Box::new),
        })),
        ..DataQuery::default()
    };
    let mut refused = 0;
    for key in &keys {
        // 辿る列（リンクの列だけ・無い列と読めない列は同じ答え）。
        let saved = check_link_field(&viewer, key).map(|_| ());
        let runtime = bind_linked(&full, &masked, &link(key, None));
        assert_eq!(answer(&saved), answer(&runtime), "{key}");
        refused += usize::from(saved.is_err());
        // 辿った先の列。
        for op in LINKED_OPS {
            let saved = check_field_use(&viewer, key, FieldUse::Filter(op));
            let cmp = Condition::cmp(key.as_str(), op, value_for(&full, key, op));
            let runtime = bind_linked(&full, &masked, &link("parent", Some(cmp)));
            assert_eq!(answer(&saved), answer(&runtime), "{key} {op:?}");
            refused += usize::from(saved.is_err());
        }
    }
    assert!(refused > 0);
    assert_eq!(
        check_link_field(&viewer, "parent").unwrap(),
        Uuid::from_u128(7)
    );
}

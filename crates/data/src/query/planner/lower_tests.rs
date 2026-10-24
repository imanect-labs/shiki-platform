//! 降下の形（プランナ耐性 SQL・値は全てバインド）の単体テスト。実際の計画の形は
//! `tests/query_plan_it.rs` が EXPLAIN で固定する。
#![allow(clippy::unwrap_used, clippy::panic, clippy::match_same_arms)]

use std::collections::HashSet;

use super::*;
use crate::model::{FieldDef, FieldType, TableSchema};
use crate::policy::compile_index::IndexGuardCtx;
use crate::policy::material::PolicyMaterial;
use crate::query::bound::bind_query;
use crate::query::planner::guard::Guard;
use serde_json::json;

fn bound(v: serde_json::Value) -> BoundQuery {
    let mut status = FieldDef::new("status", FieldType::Select);
    status.indexed = true;
    status.options = vec!["承認待ち".into()];
    let mut amount = FieldDef::new("amount", FieldType::Number);
    amount.indexed = true;
    let mut title = FieldDef::new("title", FieldType::Text);
    title.indexed = true;
    title.searchable = true;
    let mut s = TableSchema {
        fields: vec![status, amount, title],
        status_field: None,
        row_policy: None,
        field_policy: vec![],
        aggregate_min_rows: None,
        fsm_ref: None,
        rag: None,
    };
    crate::field_key::assign_field_ids(None, &mut s).unwrap();
    let q = serde_json::from_value(v).unwrap();
    bind_query(&s, &HashSet::new(), &q).unwrap()
}

fn guard_sql(rec: &str, sql: &mut SqlBuf) -> GuardSql {
    let s = TableSchema {
        fields: vec![],
        status_field: None,
        row_policy: None,
        field_policy: vec![],
        aggregate_min_rows: None,
        fsm_ref: None,
        rag: None,
    };
    let m = PolicyMaterial::default();
    let active = HashSet::new();
    let g = Guard::new(IndexGuardCtx {
        schema: &s,
        material: &m,
        user: "u",
        active: &active,
        shared_ints: &[],
    });
    g.lower(rec, sql).unwrap()
}

#[test]
fn sort_drive_is_budgeted_window_with_point_probes() {
    let q = bound(json!({
        "filter": {"cmp": {"field": "status", "op": "eq", "value": "承認待ち"}},
        "sort": [{"field": "amount", "descending": true}]
    }));
    let links = HashMap::new();
    let low = Lowerer {
        q: &q,
        links: &links,
        scopes: &[],
    };
    let conj = super::super::plans::conjuncts(&q.filter);
    let mut sql = SqlBuf::scoped(1, 2);
    let g = guard_sql("d.record_int", &mut sql);
    let text = low.sort_drive_sql(q.sort[0], None, None, &conj, &g, 2_000, 21, &mut sql);
    assert!(text.contains("FROM data_index_num i"), "{text}");
    assert!(
        text.contains("ORDER BY i.val DESC, i.record_int DESC LIMIT"),
        "{text}"
    );
    assert!(text.contains("WINDOW w AS (ORDER BY d.val DESC, d.record_int DESC)"));
    assert!(text.contains("lead(d.record_int) OVER w IS NULL AS is_last"));
    assert!(text.contains("CASE WHEN EXISTS (SELECT 1 FROM data_index_text x"));
    assert!(text.contains("OFFSET 0"));
    assert!(text.contains("WHERE s.pass OR s.is_last"));
    assert!(!text.contains("承認待ち"), "値はバインド");
}

#[test]
fn candidates_are_materialized_and_keyset_is_total_order() {
    let q = bound(json!({
        "filter": {"cmp": {"field": "status", "op": "eq", "value": "承認待ち"}},
        "sort": [{"field": "amount"}, {"field": "title", "descending": true}]
    }));
    let links = HashMap::new();
    let low = Lowerer {
        q: &q,
        links: &links,
        scopes: &[],
    };
    let conj = super::super::plans::conjuncts(&q.filter);
    let Expr::Atom(a) = conj[0] else {
        panic!("{:?}", conj[0])
    };
    let after = Pos {
        vals: vec![SortVal::Missing, SortVal::Text("x".into())],
        rint: 7,
    };
    let mut sql = SqlBuf::scoped(1, 2);
    let g = guard_sql("c.record_int", &mut sql);
    let text = low.candidates_sql(
        &Source::Atom(a),
        &[],
        &g,
        Some(&after),
        Some(1_000),
        21,
        &mut sql,
    );
    assert!(text.contains("WITH cand AS MATERIALIZED"), "{text}");
    assert!(text.contains("pass AS MATERIALIZED"));
    assert!(
        text.contains(
            "ORDER BY (z.k0 IS NULL), z.k0 ASC, (z.k1 IS NULL), z.k1 DESC, z.record_int ASC"
        ),
        "{text}"
    );
    // 第 1 キーが値なしの位置より後ろ = 第 1 キーも値なし（`NULL` のバインドと IS NOT DISTINCT FROM）で、
    // 第 2 キー以降が後ろ。値なしは型つきの NULL のバインドで、テキストは位置の値の有無に依らない。
    assert!(
        text.contains("z.k0 IS NOT DISTINCT FROM $11::numeric AND ($12::text COLLATE \"C\" IS NOT NULL AND (z.k1 IS NULL OR z.k1 < "),
        "{text}"
    );
    assert_eq!(sql.binds[10], Bind::OptNumeric(None));
    assert!(!text.contains("'x'"));
    let present = Pos {
        vals: vec![
            SortVal::Num(crate::numeric::Dec::parse("3").unwrap()),
            SortVal::Text("y".into()),
        ],
        rint: 9,
    };
    let mut sql2 = SqlBuf::scoped(1, 2);
    let g2 = guard_sql("c.record_int", &mut sql2);
    let text2 = low.candidates_sql(
        &Source::Atom(a),
        &[],
        &g2,
        Some(&present),
        Some(1_000),
        21,
        &mut sql2,
    );
    assert_eq!(text, text2, "位置の値の有無で SQL テキストは変わらない");
}

#[test]
fn contains_reads_the_search_table_with_escaped_pattern() {
    let q = bound(json!({"filter": {"cmp": {"field": "title", "op": "contains", "value": "50%"}}}));
    let links = HashMap::new();
    let low = Lowerer {
        q: &q,
        links: &links,
        scopes: &[],
    };
    let Expr::Atom(a) = &q.filter else { panic!() };
    let mut sql = SqlBuf::scoped(1, 2);
    let text = low.atom_exists(a, "d.record_int", &mut sql);
    assert!(text.contains("FROM data_index_search x"), "{text}");
    assert!(
        sql.binds.contains(&Bind::Text("%50\\%%".into())),
        "{:?}",
        sql.binds
    );
}

/// プランキャッシュの命中時の走査（[`SqlBuf::skip_text`]）: テキストを組まず、組んだときと**同じバインド**を
/// 同じ順で積む（命中時はテキストをキャッシュから引き、値だけをこの走査で集める・監査 B2-1）。
/// 降下の全ての入口（候補・ソート駆動・値なしの区間・件数・集計・件数探索・本体取得）で確かめ、
/// 組む場合と省く場合の所要時間も測る（省くほうが速い）。
/// 文を組む関数（同じ文を組む降下・テスト用）。
type Build<'a> = Box<dyn Fn(&mut SqlBuf) -> String + 'a>;

#[test]
#[allow(clippy::too_many_lines)] // 降下の全ての入口を 1 つの表で確かめる。
fn binds_only_lowering_matches_full_lowering_and_is_cheaper() {
    use crate::policy::ast::{CmpOp, PolicyExpr, PolicyOperand, RowPolicy};
    let q = bound(json!({
        "filter": {"all": [
            {"cmp": {"field": "status", "op": "in", "value": ["承認待ち"]}},
            {"cmp": {"field": "amount", "op": "gte", "value": 10}},
            {"cmp": {"field": "title", "op": "starts_with", "value": "あ".repeat(900)}},
            {"not": {"cmp": {"field": "title", "op": "contains", "value": "a_%"}}},
            {"any": [
                {"cmp": {"field": "status", "op": "eq", "value": "承認待ち"}},
                {"cmp": {"field": "amount", "op": "lt", "value": 3}}
            ]}
        ]},
        "sort": [{"field": "amount", "descending": true}, {"field": "title"}]
    }));
    let mut schema = TableSchema {
        fields: vec![FieldDef::new("status", FieldType::Select)],
        status_field: None,
        row_policy: None,
        field_policy: vec![],
        aggregate_min_rows: None,
        fsm_ref: None,
        rag: None,
    };
    schema.fields[0].indexed = true;
    crate::field_key::assign_field_ids(None, &mut schema).unwrap();
    schema.row_policy = Some(RowPolicy {
        read: PolicyExpr::Any(vec![
            PolicyExpr::IsOwner,
            PolicyExpr::FieldCmp {
                field: "status".into(),
                op: CmpOp::Ne,
                value: PolicyOperand::Lit(json!("却下")),
            },
            PolicyExpr::HasRole {
                role: "keiri".into(),
                subtree: true,
            },
        ]),
        write: None,
    });
    let m = PolicyMaterial::default();
    let active = HashSet::from([(crate::model::SYSTEM_FIELD_OWNER, IndexTable::Text)]);
    let guard = Guard::new(IndexGuardCtx {
        schema: &schema,
        material: &m,
        user: "alice",
        active: &active,
        shared_ints: &[3, 5],
    });
    let links = HashMap::new();
    let low = Lowerer {
        q: &q,
        links: &links,
        scopes: &[],
    };
    let conj = super::super::plans::conjuncts(&q.filter);
    let Expr::Atom(a) = conj[0] else {
        panic!("{:?}", conj[0])
    };
    let after = Pos {
        vals: vec![
            SortVal::Num(crate::numeric::Dec::parse("3").unwrap()),
            SortVal::Missing,
        ],
        rint: 9,
    };
    let start = SortVal::Num(crate::numeric::Dec::parse("5").unwrap());
    let stmts: Vec<Build<'_>> = vec![
        Box::new(|sql| {
            let g = guard.lower("c.record_int", sql).unwrap();
            low.candidates_sql(
                &Source::Atom(a),
                &conj[1..],
                &g,
                Some(&after),
                Some(1_000),
                21,
                sql,
            )
        }),
        Box::new(|sql| {
            let g = guard.lower("d.record_int", sql).unwrap();
            low.sort_drive_sql(
                q.sort[0],
                Some((&start, Some(4))),
                None,
                &conj,
                &g,
                160,
                21,
                sql,
            )
        }),
        Box::new(|sql| {
            let g = guard.lower("d.record_int", sql).unwrap();
            low.missing_drive_sql(q.sort[0], Some(4), &conj, &g, 160, 21, sql)
        }),
        Box::new(|sql| {
            let g = guard.lower("c.record_int", sql).unwrap();
            low.count_sql(&Source::All, &conj, &g, 10_000, sql)
        }),
        Box::new(|sql| {
            let g = guard.lower("c.record_int", sql).unwrap();
            let src = Source::Policy(
                guard
                    .drivers()
                    .unwrap_or_default()
                    .iter()
                    .map(|d| d.source_sql(sql))
                    .collect(),
            );
            low.aggregate_sql(&src, &conj, &g, sql)
        }),
        Box::new(|sql| low.probe_sql(a, 1_000, sql)),
        Box::new(|sql| fetch_sql("t", vec![1, 2], Some(vec!["f16".into()]), sql)),
    ];
    for (i, build) in stmts.iter().enumerate() {
        let mut full = SqlBuf::scoped(1, 2);
        let text = build(&mut full);
        assert!(!text.is_empty(), "#{i}");
        let mut skip = SqlBuf::scoped_binds_only(1, 2);
        let none = build(&mut skip);
        assert!(none.is_empty(), "#{i}: 命中時はテキストを組まない: {none}");
        assert_eq!(
            skip.binds, full.binds,
            "#{i}: 命中時も同じバインドを同じ順で積む"
        );
    }
    // 組む場合と省く場合の所要時間（同じ入口を繰り返す・テストの実行環境の値）。
    let rounds = 300;
    let time = |skip: bool| {
        let t = std::time::Instant::now();
        for _ in 0..rounds {
            for build in &stmts {
                let mut sql = if skip {
                    SqlBuf::scoped_binds_only(1, 2)
                } else {
                    SqlBuf::scoped(1, 2)
                };
                std::hint::black_box(build(&mut sql));
            }
        }
        t.elapsed()
    };
    time(false);
    let full = time(false);
    let skip = time(true);
    #[allow(clippy::print_stderr, clippy::cast_precision_loss)]
    {
        let per = |d: std::time::Duration| d.as_secs_f64() * 1e6 / (rounds * stmts.len()) as f64;
        eprintln!(
            "降下 1 文あたり: テキストを組む {:.1}µs → 命中（バインドだけ） {:.1}µs（{:.1} 倍）",
            per(full),
            per(skip),
            full.as_secs_f64() / skip.as_secs_f64()
        );
    }
    assert!(
        skip < full,
        "命中時の走査は降下より安い: {skip:?} vs {full:?}"
    );
}

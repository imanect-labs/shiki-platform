//! 自前オプティマイザ O2 の結合テスト（Task 15.12・query-optimizer.md §4・§5・§7.3・§8）。
//!
//! - outbox 消費側の行数カウンタと値頻度表（書込 Tx では更新しない・冪等・数え直しとの整合）
//! - 統計収集ジョブ（フィールド統計・リンク統計（対称列を含む）・材料化した派生列・行幅）と起動条件
//! - 相関のあるデータで、2 回目以降は補正係数により正しい戦略が初手で選ばれる
//! - 予算超過の見込みのクエリは実行されず、推定値を含まない構造化エラーで返る（PIT-75）
//! - 機微なテーブルの固定計画（統計・件数探索・コストを使わない）
//! - 較正ベンチで単価が置き換わる
//! - 統計 4 表が purge される

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation
)]

mod common;

use std::collections::BTreeMap;

use common::bulk::{seed, DONE, TODO};
use common::{create_table, ctx, field, keys_of, schema, setup, store_with, unique_tenant};
use data::{
    CondOp, Condition, DataError, DataQuery, DataStore, FieldType, LinkCardinality, LinkDef,
    MetricSpec, QuerySuggestion, QueryTuning, SortKey, UnitCosts,
};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

/// 統計の消費を溜まりきるまで回す（他テストの書込が並行しても有限回で抜ける）。
async fn drain_stats(store: &DataStore) {
    for _ in 0..200 {
        if store.consume_stats_once().await.unwrap() == 0 {
            return;
        }
    }
}

/// 値頻度表（field_id, val → cnt）を DB から直接読む（テスト専用）。
async fn counts_of(pool: &PgPool, ti: i32, tb: i64) -> BTreeMap<(i16, String), i64> {
    let rows: Vec<(i16, String, i64)> = sqlx::query_as(
        "SELECT field_id, val, cnt FROM data_value_count WHERE tenant_int = $1 AND table_int = $2",
    )
    .bind(ti)
    .bind(tb)
    .fetch_all(pool)
    .await
    .unwrap();
    rows.into_iter().map(|(f, v, c)| ((f, v), c)).collect()
}

/// 索引から数えた正解（field_id, val → 行数）。
async fn truth_of(pool: &PgPool, ti: i32, tb: i64, fields: &[i16]) -> BTreeMap<(i16, String), i64> {
    let rows: Vec<(i16, String, i64)> = sqlx::query_as(
        "SELECT field_id, val, count(DISTINCT record_int) FROM data_index_text \
         WHERE tenant_int = $1 AND table_int = $2 AND field_id = ANY($3) GROUP BY field_id, val",
    )
    .bind(ti)
    .bind(tb)
    .bind(fields)
    .fetch_all(pool)
    .await
    .unwrap();
    rows.into_iter().map(|(f, v, c)| ((f, v), c)).collect()
}

async fn table_stats(pool: &PgPool, ti: i32, tb: i64) -> (i64, bool, Vec<i16>, i64) {
    sqlx::query_as(
        "SELECT n_rows, counts_ready, counts_fields, changes_since FROM data_table_stats \
         WHERE tenant_int = $1 AND table_int = $2",
    )
    .bind(ti)
    .bind(tb)
    .fetch_one(pool)
    .await
    .unwrap()
}

fn categorical_schema() -> data::TableSchema {
    let mut status = field("status", FieldType::Select);
    status.indexed = true;
    status.options = vec!["open".into(), "done".into(), "hold".into()];
    let mut tags = field("tags", FieldType::MultiSelect);
    tags.indexed = true;
    tags.options = vec!["a".into(), "b".into(), "c".into()];
    let mut who = field("who", FieldType::UserRef);
    who.indexed = true;
    let mut title = field("title", FieldType::Text);
    title.indexed = true;
    let memo = field("memo", FieldType::Text); // 未索引
    schema(vec![status, tags, who, title, memo])
}

/// outbox 消費側の値頻度表は、更新・削除・多値・数え直しの後も索引から数えた値と一致する。
#[tokio::test]
async fn value_counts_follow_the_outbox_and_survive_recount() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    store.register_stats_consumer().await.unwrap();
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let table = create_table(&store, &c, "counts", categorical_schema()).await;
    let (ti, tb) = keys_of(&pool, &tenant, table.id).await;
    let id = |k: &str| i16::try_from(table.schema.field(k).unwrap().id).unwrap();
    let fields = [0, id("status"), id("tags"), id("who")];
    let mut ids = Vec::new();
    for i in 0..30u32 {
        let st = ["open", "done", "hold"][(i % 3) as usize];
        let r = store
            .create_record(
                &c,
                table.id,
                json!({
                    "status": st,
                    "tags": if i % 2 == 0 { json!(["a", "b"]) } else { json!(["c"]) },
                    "who": if i % 5 == 0 { "bob" } else { "alice" },
                    "title": format!("t{i}"),
                }),
                None,
            )
            .await
            .unwrap();
        ids.push(r.id);
    }
    drain_stats(&store).await;
    // 数え直し前は値頻度表を持たない（変更行数だけ数える）。
    let (_, ready, _, changes) = table_stats(&pool, ti, tb).await;
    assert!(!ready, "数え直し前");
    assert!(changes >= 30, "変更行数は数える: {changes}");
    // 統計収集ジョブ（起動条件を満たしたので積まれている）で数え直す。
    let out = store.analyze_table(&tenant, table.id).await.unwrap();
    assert_eq!(out.counted_fields, 4, "owner と分類列 3 つ: {out:?}");
    assert!(out.fields >= 4, "フィールド統計: {out:?}");
    let (n, ready, counted, changes) = table_stats(&pool, ti, tb).await;
    assert!(ready && n == 30 && changes == 0, "{n} {ready} {changes}");
    assert_eq!(counted.len(), 4);
    assert_eq!(
        counts_of(&pool, ti, tb).await,
        truth_of(&pool, ti, tb, &fields).await
    );

    // 更新（単値・多値・担当の付け替え）と削除を消費側が差分で反映する。
    for (i, rid) in ids.iter().enumerate().take(6) {
        store
            .update_record(
                &c,
                table.id,
                *rid,
                json!({ "status": "hold", "tags": ["a", "c"], "who": "bob" }),
                1,
                None,
            )
            .await
            .unwrap_or_else(|e| panic!("update {i}: {e}"));
    }
    for rid in &ids[25..] {
        store
            .delete_record(&c, table.id, *rid, 1, None)
            .await
            .unwrap();
    }
    // 消費前に数え直しを挟む（数え直し時点で未消費の変更は skip_events に入り、二重に足さない）。
    store
        .update_record(&c, table.id, ids[10], json!({ "status": "open" }), 1, None)
        .await
        .unwrap();
    store.analyze_table(&tenant, table.id).await.unwrap();
    drain_stats(&store).await;
    let (n, ready, _, _) = table_stats(&pool, ti, tb).await;
    assert!(ready);
    assert_eq!(n, 25, "削除の分だけ減る");
    assert_eq!(
        counts_of(&pool, ti, tb).await,
        truth_of(&pool, ti, tb, &fields).await,
        "差分の反映と数え直しが二重にならない"
    );
    // 冪等: もう一度回しても変わらない。
    let before = counts_of(&pool, ti, tb).await;
    drain_stats(&store).await;
    assert_eq!(counts_of(&pool, ti, tb).await, before);
    let skip: Vec<i64> = sqlx::query_scalar(
        "SELECT unnest(skip_events) FROM data_table_stats WHERE tenant_int = $1 AND table_int = $2",
    )
    .bind(ti)
    .bind(tb)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(skip.is_empty(), "消費したスキップは外れる: {skip:?}");
    // 未索引列での絞り込みの要求（自動昇格しない経路）は索引の昇格の候補として管理画面に出す。
    let by_memo = DataQuery {
        filter: Some(Condition::cmp("memo", CondOp::Eq, Some(json!("x")))),
        ..Default::default()
    };
    assert!(matches!(
        store
            .run_query_debug(&c, table.id, &by_memo, &QueryTuning::default())
            .await,
        Err(e) if e.is_invalid()
    ));
    let report = store.table_optimizer_report(&c, table.id).await.unwrap();
    assert!(
        report
            .candidates
            .iter()
            .any(|x| x.kind == data::OptimizerCandidateKind::IndexPromotion
                && x.fields == vec!["memo".to_string()]),
        "{report:?}"
    );
    // 自由文（title）は値頻度表を持たない。
    assert!(counts_of(&pool, ti, tb)
        .await
        .keys()
        .all(|(f, _)| *f != id("title")));
}

/// 統計収集: フィールド統計（ヒストグラム・最頻値・相関）・材料化した派生列・リンク統計（対称列の
/// 逆方向を含む）・行幅。テナント撤去で統計 4 表も消える。
#[tokio::test]
async fn analyze_covers_derived_and_symmetric_columns_and_purge_removes_stats() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let mut name = field("name", FieldType::Text);
    name.indexed = true;
    let parent = create_table(&store, &c, "parents", schema(vec![name])).await;
    let mut amount = field("amount", FieldType::Number);
    amount.indexed = true;
    let mut link = field("parent", FieldType::RecordRef);
    link.ref_table = Some(parent.id);
    link.indexed = true;
    link.link = Some(LinkDef {
        cardinality: LinkCardinality::ManyToOne,
        symmetric_field: None,
        symmetric_key: Some("children".into()),
        reverse: false,
    });
    let mut doubled = field("doubled", FieldType::Formula);
    doubled.indexed = true;
    doubled.formula =
        Some(serde_json::from_value(json!({"mul": [{"field": "amount"}, {"lit": 2}]})).unwrap());
    let child = create_table(&store, &c, "children", schema(vec![amount, link, doubled])).await;
    let mut parents = Vec::new();
    for i in 0..4 {
        let p = store
            .create_record(&c, parent.id, json!({ "name": format!("p{i}") }), None)
            .await
            .unwrap();
        parents.push(p.id);
    }
    for i in 0..40i64 {
        let mut v = json!({ "amount": i });
        if i % 4 != 0 {
            v["parent"] = json!([parents[(i % 3) as usize]]);
        }
        store.create_record(&c, child.id, v, None).await.unwrap();
    }
    let child = store.get_table(&c, child.id, None).await.unwrap();
    let doubled_id = i16::try_from(child.schema.field("doubled").unwrap().id).unwrap();
    let out = store.analyze_table(&tenant, child.id).await.unwrap();
    assert!(out.links >= 1, "{out:?}");
    let (ti, tb) = keys_of(&pool, &tenant, child.id).await;
    let (nv, nd, hist): (i64, i64, serde_json::Value) = sqlx::query_as(
        "SELECT n_values, n_distinct, histogram FROM data_field_stats \
         WHERE tenant_int = $1 AND table_int = $2 AND field_id = $3",
    )
    .bind(ti)
    .bind(tb)
    .bind(doubled_id)
    .fetch_one(&pool)
    .await
    .expect("材料化した派生列にもフィールド統計がある");
    assert_eq!((nv, nd), (40, 40));
    assert_eq!(
        hist.as_array().unwrap().len(),
        101,
        "等深ヒストグラム 100 区間"
    );
    let (n_src, frac): (i64, f32) = sqlx::query_as(
        "SELECT n_src, link_frac FROM data_link_stats \
         WHERE tenant_int = $1 AND table_int = $2 AND NOT is_reverse",
    )
    .bind(ti)
    .bind(tb)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(n_src, 30);
    assert!((frac - 0.75).abs() < 1e-3);
    let width: Option<f32> = sqlx::query_scalar(
        "SELECT avg_width FROM data_table_stats WHERE tenant_int = $1 AND table_int = $2",
    )
    .bind(ti)
    .bind(tb)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(width.unwrap() > 0.0);
    // 対称列（参照先から見た逆方向）のリンク統計。
    store.analyze_table(&tenant, parent.id).await.unwrap();
    let (pti, ptb) = keys_of(&pool, &tenant, parent.id).await;
    let (rev_src, max_fanout): (i64, i32) = sqlx::query_as(
        "SELECT n_src, max_fanout FROM data_link_stats \
         WHERE tenant_int = $1 AND table_int = $2 AND is_reverse",
    )
    .bind(pti)
    .bind(ptb)
    .fetch_one(&pool)
    .await
    .expect("対称列の逆方向のリンク統計");
    assert_eq!(rev_src, 3);
    assert_eq!(max_fanout, 10);

    // テナント撤去（purge）で統計 4 表も残らない（Phase 12.9 のゲートの対象）。
    store.purge_tenant(&tenant).await.unwrap();
    for t in [
        "data_table_stats",
        "data_field_stats",
        "data_value_count",
        "data_link_stats",
    ] {
        let n: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {t} WHERE tenant_int = $1"))
            .bind(ti)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n, 0, "{t} が purge で消えていない");
    }
}

fn todo_by_created() -> DataQuery {
    DataQuery {
        filter: Some(Condition::cmp("status", CondOp::Eq, Some(json!(TODO)))),
        sort: vec![SortKey {
            field: "_created_at".into(),
            descending: false,
        }],
        limit: Some(20),
        ..Default::default()
    }
}

/// 相関（未着手は新しい行に偏る）で独立性の仮定が外れる。1 回目はソート駆動から入って予算切れで
/// 切り替え、2 回目以降は補正係数でフィルタ駆動が初手で選ばれる（§8・§1.2 ④）。
#[tokio::test]
async fn correction_factor_picks_the_right_strategy_on_the_second_run() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let table = seed(&pool, &store, &c, "corr", 20_000, false, false).await;
    store.analyze_table(&tenant, table.id).await.unwrap();
    let q = todo_by_created();
    let auto = QueryTuning::default();
    let (r1, d1) = store
        .run_query_debug(&c, table.id, &q, &auto)
        .await
        .unwrap();
    assert!(
        d1.stats.planned_with_stats,
        "統計で計画する: {:?}",
        d1.stats
    );
    assert_eq!(
        d1.stats.first_strategy, "sort",
        "独立性の仮定ではソート駆動: {:?}",
        d1.stats
    );
    assert!(
        d1.stats.switched && d1.stats.strategy == "filter",
        "{:?}",
        d1.stats
    );
    let (r2, d2) = store
        .run_query_debug(&c, table.id, &q, &auto)
        .await
        .unwrap();
    assert_eq!(
        d2.stats.first_strategy, "filter",
        "補正係数で初手から: {:?}",
        d2.stats
    );
    assert!(!d2.stats.switched, "{:?}", d2.stats);
    assert!(
        d2.stats.scanned < d1.stats.scanned && d2.stats.statements < d1.stats.statements,
        "予算切れの走査と切替が無くなる: {:?} → {:?}",
        d1.stats,
        d2.stats
    );
    let ids = |r: &data::QueryResult| -> Vec<Uuid> {
        r.items.as_ref().unwrap().iter().map(|x| x.id).collect()
    };
    assert_eq!(ids(&r1), ids(&r2), "結果は戦略に依らない");
    assert_eq!(ids(&r1).len(), 20);
    // 相関の無い条件（完了・90%）はソート駆動のまま（補正は (テーブル, フィルタ列, ソート列) ごと）。
    let mut done = q.clone();
    done.filter = Some(Condition::cmp("status", CondOp::Eq, Some(json!(DONE))));
    done.sort[0].field = "amount".into();
    let (_, d3) = store
        .run_query_debug(&c, table.id, &done, &auto)
        .await
        .unwrap();
    assert_eq!(d3.stats.first_strategy, "sort", "{:?}", d3.stats);
    // 予測コストと所要時間が記録され、運用指標に推定値は出ない。
    assert!(d2.stats.predicted_us.is_some() && d2.stats.elapsed_us > 0);
    // 実行ごとの記録（§8・監査 B2-3）: 形状・戦略・推定行数・実測行数・走査・返却・所要時間が
    // リングバッファに残る（新しい順）。同じ形状は同じハッシュ。
    assert!(
        d2.stats.est_rows.is_some() && d2.stats.actual_rows.is_some(),
        "{:?}",
        d2.stats
    );
    let recent = store.recent_executions(3);
    assert_eq!(recent.len(), 3);
    let last = &recent[0];
    assert_eq!(
        (last.table_id, last.strategy, last.shape_hash),
        (table.id, d3.stats.strategy, d3.stats.shape_hash)
    );
    assert!(last.elapsed_us > 0 && last.returned > 0);
    assert_eq!(recent[1].shape_hash, d2.stats.shape_hash);
    assert_eq!(
        (recent[1].est_rows, recent[1].actual_rows),
        (d2.stats.est_rows, d2.stats.actual_rows)
    );
    assert_eq!(d1.stats.shape_hash, d2.stats.shape_hash, "同じ形状");
    assert_ne!(d2.stats.shape_hash, d3.stats.shape_hash);
    let (_, n) = store.optimizer_correlation();
    assert!(n >= 3);
}

/// 予算超過の見込みのクエリは実行されず、推定値を含まない構造化エラーになる（§5.4・PIT-75）。
#[tokio::test]
async fn over_budget_queries_are_rejected_without_estimates() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let table = seed(&pool, &store, &c, "budget", 20_000, false, false).await;
    store.analyze_table(&tenant, table.id).await.unwrap();
    let tight = store.clone().with_read_budget_ms(1);
    let agg = DataQuery {
        group_by: vec!["status".into()],
        aggregate: vec![MetricSpec {
            metric: data::Metric::Count,
            field: None,
        }],
        ..Default::default()
    };
    let err = tight.run_query(&c, table.id, &agg, None).await.unwrap_err();
    let DataError::QueryTooExpensive { suggestions } = &err else {
        panic!("受付制御で断る: {err:?}");
    };
    assert_eq!(suggestions, &vec![QuerySuggestion::NarrowFilter]);
    let msg = format!("{err} {err:?}");
    assert!(
        !msg.chars().any(|ch| ch.is_ascii_digit()),
        "推定値・予測コストを含めない: {msg}"
    );
    // 読める未索引の列があれば「索引の昇格」も提案する（列名・推定は含めない・§5.4）。
    let mut next = table.schema.clone();
    next.fields
        .push(data::FieldDef::new("memo", data::FieldType::Text));
    store
        .update_table_schema(&c, table.id, next, None, None)
        .await
        .unwrap();
    let err = tight.run_query(&c, table.id, &agg, None).await.unwrap_err();
    let DataError::QueryTooExpensive { suggestions } = &err else {
        panic!("受付制御で断る: {err:?}");
    };
    assert_eq!(
        suggestions,
        &vec![QuerySuggestion::NarrowFilter, QuerySuggestion::PromoteIndex]
    );
    // 絞り込めば通る（同じ予算で候補が少ない）。
    let mut narrow = agg.clone();
    narrow.filter = Some(Condition::cmp(
        "status",
        CondOp::Eq,
        Some(json!(common::bulk::PENDING)),
    ));
    let big = store.clone().with_read_budget_ms(50);
    big.run_query(&c, table.id, &narrow, None).await.unwrap();
    // 既定の予算（2 秒）なら全体の集計も実行する。
    store.run_query(&c, table.id, &agg, None).await.unwrap();
    let m = tight.query_metrics(table.id).unwrap();
    assert!(m.rejections >= 1, "断った読取も運用指標に残る: {m:?}");
}

/// 機微なテーブルは固定計画（統計・件数探索・コストを使わず、安全計画で読む）。結果は変わらない。
#[tokio::test]
async fn fixed_plan_tables_ignore_statistics() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let table = seed(&pool, &store, &c, "fixed", 5_000, false, false).await;
    store.analyze_table(&tenant, table.id).await.unwrap();
    let q = todo_by_created();
    let auto = QueryTuning::default();
    let (r_auto, d_auto) = store
        .run_query_debug(&c, table.id, &q, &auto)
        .await
        .unwrap();
    assert!(d_auto.stats.planned_with_stats);
    let report = store
        .set_table_fixed_plan(&c, table.id, true, None)
        .await
        .unwrap();
    assert!(report.fixed_plan);
    let (r_fixed, d_fixed) = store
        .run_query_debug(&c, table.id, &q, &auto)
        .await
        .unwrap();
    assert!(!d_fixed.stats.planned_with_stats, "{:?}", d_fixed.stats);
    assert_eq!(d_fixed.stats.probes, 0, "件数探索もしない");
    assert!(
        d_fixed
            .statements
            .iter()
            .all(|s| s.label != "probe" && s.label != "candidates"),
        "{:?}",
        d_fixed
            .statements
            .iter()
            .map(|s| s.label)
            .collect::<Vec<_>>()
    );
    let ids = |r: &data::QueryResult| -> Vec<Uuid> {
        r.items.as_ref().unwrap().iter().map(|x| x.id).collect()
    };
    assert_eq!(ids(&r_auto), ids(&r_fixed));
    let report = store.table_optimizer_report(&c, table.id).await.unwrap();
    assert!(report.fixed_plan && report.requests >= 2);
    store
        .set_table_fixed_plan(&c, table.id, false, None)
        .await
        .unwrap();
    let (_, d) = store
        .run_query_debug(&c, table.id, &q, &auto)
        .await
        .unwrap();
    assert!(d.stats.planned_with_stats);
    // 複数ソートの同値群が先読みの上限を超えると、複合索引の材料化の候補になる。
    let skew = DataQuery {
        sort: vec![
            SortKey {
                field: "status".into(),
                descending: false,
            },
            SortKey {
                field: "amount".into(),
                descending: true,
            },
        ],
        limit: Some(5),
        ..Default::default()
    };
    store.run_query(&c, table.id, &skew, None).await.unwrap();
    for _ in 0..3 {
        store.run_query(&c, table.id, &q, None).await.unwrap();
    }
    let report = store.table_optimizer_report(&c, table.id).await.unwrap();
    // 所要時間の見込みと実測の一致（テーブル単位・値を含まない・監査 B2-4）。
    assert!(report.timing_samples >= 3, "{report:?}");
    assert!(report.timing_correlation.is_some(), "{report:?}");
    assert!(
        report
            .candidates
            .iter()
            .any(|x| x.kind == data::OptimizerCandidateKind::CompositeIndex
                && x.fields == vec!["status".to_string(), "amount".to_string()]),
        "{report:?}"
    );
    // 他テナントからは見えない（404）。
    let other = ctx(&unique_tenant(), "alice");
    assert!(matches!(
        store.table_optimizer_report(&other, table.id).await,
        Err(DataError::NotFound)
    ));
}

/// 較正ベンチで単価が置き換わる（初期値と違う・正の有限値）。
#[tokio::test]
async fn calibration_replaces_unit_costs() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    assert_eq!(store.unit_costs(), UnitCosts::default());
    let r = store.calibrate_costs().await.unwrap();
    eprintln!("較正: 前 {:?} → 後 {:?}", r.before, r.after);
    assert_eq!(r.before, UnitCosts::default());
    assert_ne!(r.after, r.before);
    for v in [
        r.after.c_tuple,
        r.after.c_probe,
        r.after.c_fetch,
        r.after.c_rtt,
        r.after.c_sort,
    ] {
        assert!(v.is_finite() && v > 0.0, "{:?}", r.after);
    }
    assert!(r.after.c_probe >= r.after.c_tuple);
    assert_eq!(store.unit_costs(), r.after);
    // 較正の前後で、検証用の操作の予測と実測の一致（相関・平均対数誤差）が記録される（監査 B2-4）。
    eprintln!(
        "検証 {} 本: 相関 {:?} → {:?}・平均対数誤差 {:.3} → {:.3}",
        r.validation_samples,
        r.correlation_before,
        r.correlation_after,
        r.log_error_before,
        r.log_error_after
    );
    assert_eq!(r.validation_samples, 9);
    assert!(r.correlation_before.is_some() && r.correlation_after.is_some());
    assert!(
        r.log_error_after < r.log_error_before,
        "較正で予測が実測に近づく: {} → {}",
        r.log_error_before,
        r.log_error_after
    );
    assert_eq!(store.calibration_report(), Some(r.clone()));
    let status = store.optimizer_status();
    assert_eq!(status.unit_costs, r.after);
    assert_eq!(status.calibration, Some(r));
    // 較正は migration の固定表を読むだけで、表も索引も作らない（ランタイム DDL ゼロ・監査 B1-2）。
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM pg_class WHERE relname LIKE 'shiki_calib%'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(n, 0);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM data_planner_calib")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 20_000, "固定表は読むだけ");
}

/// テナント間で統計収集ジョブが公平に取り出される（重いテナントが積んでも軽いテナントが待たない）。
#[tokio::test]
async fn analyze_jobs_are_claimed_fairly_across_tenants() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let heavy = unique_tenant();
    let light = unique_tenant();
    let hc = ctx(&heavy, "alice");
    let lc = ctx(&light, "alice");
    let mut heavy_tables = Vec::new();
    for i in 0..3 {
        heavy_tables.push(create_table(&store, &hc, &format!("h{i}"), categorical_schema()).await);
    }
    let lt = create_table(&store, &lc, "l", categorical_schema()).await;
    let mut conn = pool.acquire().await.unwrap();
    for t in &heavy_tables {
        let payload = json!({ "table_id": t.id });
        jobq::enqueue_on(
            &mut conn,
            jobq::NewJob {
                queue: data::STATS_ANALYZE_QUEUE,
                tenant_id: &heavy,
                payload: &payload,
                trace_id: None,
                max_attempts: 5,
            },
        )
        .await
        .unwrap();
    }
    let payload = json!({ "table_id": lt.id });
    jobq::enqueue_on(
        &mut conn,
        jobq::NewJob {
            queue: data::STATS_ANALYZE_QUEUE,
            tenant_id: &light,
            payload: &payload,
            trace_id: None,
            max_attempts: 5,
        },
    )
    .await
    .unwrap();
    // 他テストのジョブも混じり得るので、軽いテナントのジョブが重いテナントの 3 件を待たずに
    // 処理されることだけを確かめる（テナントごとの先頭が順に回る）。
    let pending = |t: &str| {
        let t = t.to_string();
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM job_queue WHERE queue = $1 AND tenant_id = $2",
            )
            .bind(data::STATS_ANALYZE_QUEUE)
            .bind(&t)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    // 共有 DB には他テストのテナントのジョブもあり、それぞれの先頭も順に回る（それも公平性）。
    for _ in 0..2_000 {
        if pending(&light).await == 0 {
            break;
        }
        let _ = store.run_stats_job_once().await;
    }
    assert_eq!(pending(&light).await, 0);
    assert!(
        pending(&heavy).await >= 2,
        "重いテナントの残りより先に軽いテナントが処理される"
    );
}

/// 運用 CLI（`shiki-admin data-table-shape`）の入口: プールだけでテーブルの形を取り出す。値・列の参照名を
/// 含まず、統計の無いテーブル・別テナントからは取れない（監査 B2-5・query-optimizer §9.3）。
#[tokio::test]
async fn table_shape_is_extracted_without_values_through_the_cli_entry() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let table = seed(&pool, &store, &c, "shape", 3_000, false, false).await;
    let pools: storage::PoolRouter = pool.clone().into();
    assert!(
        data::extract_table_shape(&pools, &tenant, table.id)
            .await
            .unwrap()
            .is_none(),
        "統計がまだ無い"
    );
    store.analyze_table(&tenant, table.id).await.unwrap();
    let shape = data::extract_table_shape(&pools, &tenant, table.id)
        .await
        .unwrap()
        .expect("統計から形を取り出せる");
    assert_eq!(shape.n_rows, 3_000);
    assert_eq!(shape.columns.len(), 3, "status・amount・assignee");
    let text = serde_json::to_string(&shape).unwrap();
    for banned in [
        common::bulk::PENDING,
        TODO,
        DONE,
        "\"status\"",
        "\"amount\"",
        "shape",
    ] {
        assert!(!text.contains(banned), "{banned} が形に含まれる: {text}");
    }
    // JSON を往復できる（合成ベンチの入力・浮動小数点の末尾桁は問わない）。
    let back: data::TableShape = serde_json::from_str(&text).unwrap();
    assert_eq!(back.n_rows, shape.n_rows);
    assert_eq!(
        back.columns
            .iter()
            .map(|c| (c.ty, c.distinct))
            .collect::<Vec<_>>(),
        shape
            .columns
            .iter()
            .map(|c| (c.ty, c.distinct))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        store.extract_table_shape(&tenant, table.id).await.unwrap(),
        Some(shape)
    );
    // 別テナント・存在しないテーブルからは取れない。
    assert!(
        data::extract_table_shape(&pools, &unique_tenant(), table.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(data::extract_table_shape(&pools, &tenant, Uuid::new_v4())
        .await
        .unwrap()
        .is_none());
}

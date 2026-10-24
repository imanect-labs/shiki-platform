//! リンク・ルックアップ・ロールアップ・数式（data-platform §7・Task 15.4・PIT-20・PIT-71）。
//!
//! # 読取（閲覧者本人の権限で毎回計算する）
//!
//! 取得済みの行へ派生フィールドと対称列を埋める（[`DataStore::resolve_derived_fields`]）。
//!
//! 0. **順方向のリンク列**（存在秘匿・§7.2）: 保存値の参照先 id のうち、閲覧者本人の権限で見えない
//!    もの（参照先テーブルの viewer なし・行述語で不可視・削除済み）を落とす。REST・SQL・チャット・SSE・
//!    ビュー・ミニアプリ・ワークフロー出力は全てこの出口を通る（書込側の扱いは [`crate::link_merge`]）。
//! 1. **対称列**（参照先から見た逆リンク・読取専用）: 順方向の書込と同じ Tx で書いた逆方向行
//!    （`data_link`・自分側の `table_int` に載るので 1 パーティション）を引き、参照元テーブルの行を
//!    閲覧者が見えるものだけに絞る。
//! 2. **参照先の行**（リンク列ごとに 1 回のバッチ取得・§7.2）: 参照先テーブルの viewer（第 1 層）・
//!    行述語の SQL 前段（第 2 層）・同じ述語の Rust 事後検査（二段 authz）・参照先のフィールドマスク
//!    （第 3 層）を**閲覧者本人の権限で**適用する。見えない参照先は「無い」のと同じ（null / 件数に入らない）。
//!    認可材料はリクエストごとに OpenFGA から引き、跨いで持たない（PIT-18）。
//! 3. 行ごとの計算（[`compute`]）: lookup / rollup（スモールセル抑制は参照先の `aggregate_min_rows`）/
//!    computed / 数式（[`crate::formula`]）。自テーブルのマスク列は「値なし」として計算に入れない。
//!
//! # 材料化（`invariant` かつ `indexed` の列だけ・[`materialize`]）
//!
//! 材料化した値は索引行としてだけ持ち、フィルタ・ソートに使う。表示は常に 1〜3 の読取時計算。
//! 4 階層のどれかが閲覧者依存になった瞬間に、材料化した行を同一 Tx で破棄する（PIT-71）。

pub(crate) mod compute;
pub(crate) mod graph;
pub(crate) mod links;
pub(crate) mod materialize;
pub(crate) mod schema;
pub(crate) mod types;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use authz::{AuthContext, Relation};
use serde_json::{Map, Value};
use uuid::Uuid;

use self::compute::{linked_ids, RefRows, RowEval};
use crate::field_key::decode_record;
use crate::formula::FormulaExpr;
use crate::model::{DataRecord, DataTable, FieldType};
use crate::policy::compile::{compile_read_predicate, BindSet, RECORD_ALIAS};
use crate::policy::material::{self, PolicyMaterial};
use crate::policy::memo::AuthzMemo;
use crate::query::executor::SCOPE_BINDS;
use crate::query::planner::verify::{PolicyEnv, RowView};
use crate::store::DataStore;
use crate::{map_db, DataError, DEFAULT_AGGREGATE_MIN_ROWS};

/// 参照先の派生フィールドを辿る段数の上限（多段の lookup / rollup）。超えた先は null。
pub(crate) const MAX_RESOLVE_DEPTH: usize = 3;

type BoxFut<'a, T> = Pin<Box<dyn Future<Output = Result<T, DataError>> + Send + 'a>>;

/// リンク列ごとに必要な参照先の列。
#[derive(Debug, Default)]
struct LinkUse {
    ref_table: Option<Uuid>,
    targets: BTreeSet<String>,
}

/// スキーマが読取時の解決を要するか（派生フィールド・対称列・順方向のリンク列がある）。
///
/// 順方向のリンク列も解決の対象: 保存値には書き手が付けた全参照先の id が入っているので、閲覧者に
/// 見えない参照先（行述語で不可視・参照先テーブルの viewer なし）の id と件数を読取の出口で落とす
/// （§7.2 の存在秘匿）。
pub(crate) fn needs_resolution(table: &DataTable) -> bool {
    table.schema.fields.iter().any(resolves_on_read)
}

/// 読取時に閲覧者の権限で値が決まる列か（派生フィールド・リンク列）。
fn resolves_on_read(f: &crate::model::FieldDef) -> bool {
    f.field_type.is_derived() || f.field_type == FieldType::RecordRef
}

/// 順方向のリンク列（値を本体に持つリンク・対称列ではない）。
fn forward_links(table: &DataTable) -> impl Iterator<Item = (&crate::model::FieldDef, Uuid)> {
    table
        .schema
        .fields
        .iter()
        .filter(|f| f.field_type == FieldType::RecordRef && !f.is_reverse_link())
        .filter_map(|f| f.ref_table.map(|t| (f, t)))
}

/// 派生フィールドが辿るリンク列の参照名と、その参照先テーブル（SSE の派生値の再配信・Task 15.6）。
pub(crate) fn derived_links(table: &DataTable) -> Vec<(String, Uuid)> {
    link_uses(table)
        .into_iter()
        .filter_map(|(k, u)| u.ref_table.map(|t| (k, t)))
        .collect()
}

/// 派生フィールドが辿るリンク列と、参照先で必要な列。
fn link_uses(table: &DataTable) -> HashMap<String, LinkUse> {
    let mut uses: HashMap<String, LinkUse> = HashMap::new();
    let schema = &table.schema;
    let mut add = |link: &str, target: Option<&str>| {
        let u = uses.entry(link.to_string()).or_default();
        u.ref_table = schema.field(link).and_then(|f| f.ref_table);
        if let Some(t) = target {
            u.targets.insert(t.to_string());
        }
    };
    for f in &schema.fields {
        match f.field_type {
            FieldType::Lookup => {
                if let Some(d) = &f.lookup {
                    add(&d.via_field, Some(&d.target_field));
                }
            }
            FieldType::Rollup => {
                if let Some(d) = &f.rollup {
                    add(&d.link, d.target.as_deref());
                }
            }
            FieldType::Formula => {
                if let Some(e) = &f.formula {
                    for n in e.rollups() {
                        if let FormulaExpr::Rollup { link, target, .. } = n {
                            add(link, target.as_deref());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    uses
}

/// 読取時の解決が 1 段目に辿る参照先テーブル（派生フィールドのリンク先・対称列の参照元）。
///
/// 読取の入口が参照先テーブル viewer の check を BatchCheck へ束ねるために使う（Task 15.9）。
pub(crate) fn first_hop_tables(table: &DataTable) -> Vec<Uuid> {
    if !needs_resolution(table) {
        return Vec::new();
    }
    let mut out: BTreeSet<Uuid> = link_uses(table)
        .into_values()
        .filter_map(|u| u.ref_table)
        .collect();
    out.extend(
        table
            .schema
            .fields
            .iter()
            .filter(|f| f.field_type == FieldType::RecordRef)
            .filter_map(|f| f.ref_table),
    );
    out.remove(&table.id);
    out.into_iter().collect()
}

/// 行のリンク列 `key` の値を見える参照先だけに絞り（並び順は保つ）、残した参照先の表示名を添える。
fn retain_visible_links(
    rows: &mut [DataRecord],
    key: &str,
    visible: &HashSet<Uuid>,
    labels: &HashMap<Uuid, String>,
) {
    for r in rows.iter_mut() {
        let Some(obj) = r.data.as_object_mut() else {
            continue;
        };
        if !obj.contains_key(key) {
            continue;
        }
        let kept: Vec<Uuid> = linked_ids(obj, key)
            .into_iter()
            .filter(|id| visible.contains(id))
            .collect();
        for id in &kept {
            if let Some(l) = labels.get(id) {
                r.link_labels.insert(id.to_string(), l.clone());
            }
        }
        obj.insert(
            key.to_string(),
            Value::Array(
                kept.iter()
                    .map(|id| Value::String(id.to_string()))
                    .collect(),
            ),
        );
    }
}

impl DataStore {
    /// 取得済み行へ派生フィールドと対称列を埋める（get / list / query の出口で呼ぶ）。
    ///
    /// `masked` は閲覧者に読めない自テーブルの列（計算の材料から外す。応答からの除去は呼び出し側）。
    pub(crate) async fn resolve_derived_fields(
        &self,
        ctx: &AuthContext,
        az: &AuthzMemo<'_>,
        table: &DataTable,
        masked: &HashSet<String>,
        rows: &mut [DataRecord],
    ) -> Result<(), DataError> {
        if rows.is_empty() || !needs_resolution(table) {
            return Ok(());
        }
        // 材料化の前提（4 階層）が今も成り立つかを確かめ、崩れていれば同一 Tx で破棄する（PIT-71）。
        // 表示は下の読取時計算なので、ここでの降格は「索引に残った漏洩源を消す」ためのもの。
        self.verify_materialized(ctx, az, table).await?;
        self.resolve_derived_at(ctx, az, table, masked, rows, 0)
            .await
    }

    fn resolve_derived_at<'a>(
        &'a self,
        ctx: &'a AuthContext,
        az: &'a AuthzMemo<'_>,
        table: &'a DataTable,
        masked: &'a HashSet<String>,
        rows: &'a mut [DataRecord],
        depth: usize,
    ) -> BoxFut<'a, ()> {
        Box::pin(async move {
            if rows.is_empty() || !needs_resolution(table) {
                return Ok(());
            }
            // ⓪ 順方向のリンク列（存在秘匿・§7.2）: 参照先をリンク列ごとに 1 回、閲覧者本人の権限で引き、
            // 見えない参照先の id を値から落とす（並び順は保つ）。派生列が辿るリンクは同じ取得を ② に使う。
            let uses = link_uses(table);
            let mut refs: HashMap<String, RefRows> = HashMap::new();
            let no_targets = BTreeSet::new();
            for (f, rt) in forward_links(table) {
                if masked.contains(&f.key) {
                    continue; // 列ごと応答から落ちる（呼び出し側の投影）。
                }
                let (visible, labels) = if depth >= MAX_RESOLVE_DEPTH {
                    // 辿れる段数を超えた先は「見えない」側に倒す。
                    (HashSet::new(), HashMap::new())
                } else {
                    let ids: Vec<Uuid> = rows
                        .iter()
                        .filter_map(|r| r.data.as_object())
                        .flat_map(|m| linked_ids(m, &f.key))
                        .collect::<HashSet<_>>()
                        .into_iter()
                        .collect();
                    let targets = uses.get(&f.key).map_or(&no_targets, |u| &u.targets);
                    let r = self
                        .visible_ref_rows(ctx, az, rt, &ids, targets, depth + 1)
                        .await?;
                    let out = (r.rows.keys().copied().collect(), r.labels());
                    if uses.contains_key(&f.key) {
                        refs.insert(f.key.clone(), r);
                    }
                    out
                };
                retain_visible_links(rows, &f.key, &visible, &labels);
            }
            // 計算の材料（閲覧者に読める列だけ）。
            let mut work: Vec<Map<String, Value>> = rows
                .iter()
                .map(|r| {
                    let mut m = r.data.as_object().cloned().unwrap_or_default();
                    m.retain(|k, _| !masked.contains(k));
                    m
                })
                .collect();
            // ① 対称列（読取専用の逆リンク）: 見える参照元だけを並び順のまま入れる。
            for f in table.schema.fields.iter().filter(|f| f.is_reverse_link()) {
                let rv = if depth >= MAX_RESOLVE_DEPTH || masked.contains(&f.key) {
                    links::ReverseValues::default()
                } else {
                    self.reverse_link_values(ctx, az, table, f, rows, depth)
                        .await?
                };
                for (i, r) in rows.iter_mut().enumerate() {
                    // 見える参照元の表示名（順方向のリンクと同じ規則・見えない参照元は値にも名前にも出ない）。
                    for u in rv.values.get(&r.id).into_iter().flatten() {
                        if let Some(l) = rv.labels.get(u) {
                            r.link_labels.insert(u.to_string(), l.clone());
                        }
                    }
                    let ids: Vec<Value> = rv
                        .values
                        .get(&r.id)
                        .map(|v| v.iter().map(|u| Value::String(u.to_string())).collect())
                        .unwrap_or_default();
                    // 上限を超えた行は、この列を辿る集計・lookup の材料にしない（部分集合で数えない）。
                    let material = if rv.truncated.contains(&r.id) {
                        Value::Null
                    } else {
                        Value::Array(ids.clone())
                    };
                    work[i].insert(f.key.clone(), material);
                    if let Some(obj) = r.data.as_object_mut() {
                        obj.insert(f.key.clone(), Value::Array(ids));
                    }
                }
            }
            // ② 参照先の行（リンク列ごとに 1 回・⓪ で引いた順方向のリンクは引き直さない）。
            if depth < MAX_RESOLVE_DEPTH {
                for (link, u) in uses {
                    if refs.contains_key(&link) {
                        continue;
                    }
                    let Some(rt) = u.ref_table else { continue };
                    let ids: Vec<Uuid> = work
                        .iter()
                        .flat_map(|m| linked_ids(m, &link))
                        .collect::<HashSet<_>>()
                        .into_iter()
                        .collect();
                    let r = self
                        .visible_ref_rows(ctx, az, rt, &ids, &u.targets, depth + 1)
                        .await?;
                    refs.insert(link, r);
                }
            }
            // ③ 行ごとの計算。
            let eval = RowEval {
                schema: &table.schema,
                refs: &refs,
                now: chrono::Utc::now(),
                only_invariant: false,
                masked,
            };
            for (i, r) in rows.iter_mut().enumerate() {
                let values = eval.evaluate(&work[i]);
                r.suppressed_fields = eval.suppressed(&work[i], &values);
                if let Some(obj) = r.data.as_object_mut() {
                    for (k, v) in values {
                        obj.insert(k, v);
                    }
                }
            }
            Ok(())
        })
    }

    /// 参照先テーブルの行を**閲覧者本人の権限で**バッチ取得する（PIT-20・二段 authz）。
    ///
    /// テーブル viewer が無い・テーブルが無い・行述語で見えない行は結果に含まれない（＝null・件数外）。
    /// 返す行は外部形（key）・参照先のマスク適用済み。`targets` に参照先の派生列があれば、その行の
    /// 派生値も閲覧者の権限で計算する（`depth` が上限を超えたら打ち切る）。
    pub(crate) fn visible_ref_rows<'a>(
        &'a self,
        ctx: &'a AuthContext,
        az: &'a AuthzMemo<'_>,
        ref_table_id: Uuid,
        ids: &'a [Uuid],
        targets: &'a BTreeSet<String>,
        depth: usize,
    ) -> BoxFut<'a, RefRows> {
        Box::pin(async move {
            // 空集合・不可視のときの K は 1（件数 0 は 1 未満なので抑制＝null。K ≥ 1 なら同じ結果）。
            let empty = RefRows {
                min_rows: 1,
                ..RefRows::default()
            };
            if ids.is_empty() {
                return Ok(empty);
            }
            // 第 1 層: 参照先テーブルの viewer（閲覧者本人・剥奪は即時反映・リクエスト内はメモ）。
            let can_view = az
                .check(
                    ctx,
                    Relation::Viewer,
                    &ctx.ns().data_table(&ref_table_id.to_string()),
                )
                .await?;
            if !can_view {
                return Ok(empty);
            }
            let ref_table = match self.fetch_live(ctx, ref_table_id).await {
                Ok(t) => t,
                Err(DataError::NotFound) => return Ok(empty),
                Err(e) => return Err(e),
            };
            let ref_masked = self.masked_fields(ctx, az, &ref_table).await?;
            // 第 2 層: 行述語（材料はリクエストごとに OpenFGA から・跨いで持たない・PIT-18）。
            // SQL 前段 → Rust 事後検査。
            let m = match &ref_table.schema.row_policy {
                Some(p) => material::resolve(ctx, az, &[&p.read]).await?,
                None => Arc::new(PolicyMaterial::default()),
            };
            let fetched = self.select_ref_rows(ctx, &ref_table, &m, ids).await?;
            let shared: HashSet<Uuid> = m.shared_record_ids.iter().copied().collect();
            let env = PolicyEnv {
                schema: &ref_table.schema,
                material: &m,
                user: &ctx.principal.id,
                shared: &shared,
            };
            let mut fetched = fetched;
            crate::query::planner::verify::post_filter(
                &env,
                &mut fetched,
                |(id, owner, data)| RowView {
                    id: *id,
                    owner,
                    data,
                },
                ref_table.id,
            );
            let mut recs: Vec<DataRecord> = fetched
                .into_iter()
                .map(|(id, owner, data)| DataRecord {
                    id,
                    table_id: ref_table.id,
                    data: Value::Object(decode_record(&ref_table.schema, &data)),
                    rev: 0,
                    owner,
                    created_at: chrono::DateTime::UNIX_EPOCH,
                    updated_at: chrono::DateTime::UNIX_EPOCH,
                    suppressed_fields: Vec::new(),
                    link_labels: std::collections::BTreeMap::new(),
                })
                .collect();
            // 参照先の派生列を対象にする場合（多段）は、参照先でも閲覧者の権限で計算する。
            // 参照先のリンク列を射影する lookup も、参照先の参照先の存在秘匿を同じ規則で掛ける。
            let needs_derived = targets
                .iter()
                .any(|t| ref_table.schema.field(t).is_some_and(resolves_on_read));
            if needs_derived {
                self.resolve_derived_at(ctx, az, &ref_table, &ref_masked, &mut recs, depth)
                    .await?;
            }
            let rows = recs
                .into_iter()
                .map(|r| {
                    let mut m = match r.data {
                        Value::Object(m) => m,
                        _ => Map::new(),
                    };
                    // 第 3 層: 参照先のマスク列は値を持ち出さない（PIT-19）。
                    m.retain(|k, _| !ref_masked.contains(k));
                    (r.id, m)
                })
                .collect();
            Ok(RefRows {
                rows,
                types: ref_table
                    .schema
                    .fields
                    .iter()
                    .map(|f| (f.key.clone(), f.field_type))
                    .collect(),
                min_rows: ref_table
                    .schema
                    .aggregate_min_rows
                    .unwrap_or(DEFAULT_AGGREGATE_MIN_ROWS),
                title_key: {
                    let mut text: Vec<&crate::model::FieldDef> = ref_table
                        .schema
                        .fields
                        .iter()
                        .filter(|f| f.field_type == FieldType::Text && !ref_masked.contains(&f.key))
                        .collect();
                    text.sort_by_key(|f| f.order);
                    text.first().map(|f| f.key.clone())
                },
            })
        })
    }

    /// 参照先の行を行述語つきで引く（SQL 前段・値は全てバインド・`(id, owner, 保存形 data)`）。
    async fn select_ref_rows(
        &self,
        ctx: &AuthContext,
        table: &DataTable,
        m: &PolicyMaterial,
        ids: &[Uuid],
    ) -> Result<Vec<(Uuid, String, Value)>, DataError> {
        let mut binds = BindSet::new(SCOPE_BINDS);
        let pred = compile_read_predicate(
            &table.schema,
            m,
            &ctx.principal.id,
            RECORD_ALIAS,
            &mut binds,
        )?;
        let ids_ph = SCOPE_BINDS + 1 + binds.binds.len();
        let sql = format!(
            "SELECT r.id, r.owner, r.data FROM data_record {RECORD_ALIAS} \
             WHERE r.tenant_id = $1 AND r.table_id = $2 \
               AND r.tenant_int = $3 AND r.table_int = $4 AND ({pred}) \
               AND r.id = ANY(${ids_ph}::uuid[])"
        );
        // バッチ駆動の段（query-optimizer.md §6.4）: リンク列ごとに 1 本の一括取得で、配列長は
        // 上限（1,000）ごとに段を分ける（N+1 にしない・1 文の配列を有界にする・data-platform §16.9）。
        let mut out = Vec::with_capacity(ids.len());
        for stage in crate::query::planner::batch::stages(ids) {
            let q = sqlx::query_as::<_, (Uuid, String, sqlx::types::Json<Value>)>(&sql)
                .bind(&ctx.tenant_id)
                .bind(table.id)
                .bind(table.keys.tenant_int)
                .bind(table.keys.table_int);
            let q = crate::query::sql::bind_query_as(q, &binds.binds);
            let rows = q
                .bind(stage.to_vec())
                .fetch_all(self.pools.for_tenant(&ctx.tenant_id))
                .await
                .map_err(map_db)?;
            out.extend(rows.into_iter().map(|(i, o, d)| (i, o, d.0)));
        }
        Ok(out)
    }
}

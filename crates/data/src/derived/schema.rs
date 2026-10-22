//! 派生フィールドとリンク定義のスキーマ検証（保存時・data-platform §7）。
//!
//! - 同期の検証（[`validate_derived_field`]）: 定義の有無・参照先の列の存在・数式の形・宣言フラグ。
//!   `indexed` は材料化可否が `invariant` で、値の型が索引に載るものだけ（`per_viewer` / `volatile` は 422）。
//! - 非同期の準備（[`DataStore::prepare_derived`]）: 参照先テーブルを読み込んで循環を検出し（422）、
//!   値の型と材料化可否（4 階層・OpenFGA のタプル比較で ① を証明）を**サーバが決めて**スキーマへ書く。
//!   入力に書かれた `materialization` / `result_type` / 対称列の情報は信用しない。

use std::collections::{HashMap, HashSet, VecDeque};

use authz::{AuthContext, Consistency, Relation};
use uuid::Uuid;

use super::graph::{Catalog, CatalogTable, MAX_CATALOG_TABLES};
use super::materialize::viewers_contained;
use super::types::{Materialization, ResultType, RollupOp};
use crate::field_key::is_valid_field_key;
use crate::formula::ast::validate_shape;
use crate::formula::FormulaExpr;
use crate::model::{FieldDef, FieldId, FieldType, TableSchema};
use crate::store::{DataStore, TableRow, TABLE_COLS};
use crate::{map_db, DataError};

/// リンク定義・派生フィールドの同期検証（[`crate::schema`] の型別検証から呼ぶ）。
pub(crate) fn validate_derived_field(schema: &TableSchema, f: &FieldDef) -> Result<(), DataError> {
    let name = &f.key;
    let bad = |m: &str| Err(DataError::Invalid(format!("'{name}': {m}")));
    if let Some(link) = &f.link {
        if f.field_type != FieldType::RecordRef {
            return bad("link は record_ref 型にのみ指定できます");
        }
        if f.unique {
            return bad(
                "リンクの一意性は cardinality で宣言してください（unique は指定できません）",
            );
        }
        if link.reverse {
            if link.symmetric_field.is_none() || f.indexed || f.required {
                return bad("対称列は読取専用です（indexed / required は指定できません）");
            }
        } else {
            if let Some(k) = &link.symmetric_key {
                if !is_valid_field_key(k) {
                    return bad("link.symmetric_key が不正です（^[a-z][a-z0-9_]{0,63}$）");
                }
            }
            // dst 側の一意（one_to_one / one_to_many）は逆方向行の部分一意で強制する（§7.1）。
            if link.cardinality.dst_single()
                && link.symmetric_field.is_none()
                && link.symmetric_key.is_none()
            {
                return bad(
                    "one_to_one / one_to_many のリンクは対称列（link.symmetric_key）が必要です（参照先側の一意性を DB で強制するため）",
                );
            }
        }
    }
    match f.field_type {
        FieldType::Rollup => {
            let Some(d) = &f.rollup else {
                return bad("rollup には rollup 定義が必要です");
            };
            check_link_ref(schema, name, &d.link)?;
            check_target(name, d.target.as_deref(), d.op)?;
        }
        FieldType::Formula => {
            let Some(e) = &f.formula else {
                return bad("formula には formula 定義が必要です");
            };
            validate_shape(name, e)?;
            let mut err: Option<DataError> = None;
            e.walk(&mut |n| {
                if err.is_some() {
                    return;
                }
                let r = match n {
                    FormulaExpr::Field(k) if schema.field(k).is_none() => Err(DataError::Invalid(
                        format!("'{name}': 数式の参照 '{k}' が fields にありません"),
                    )),
                    FormulaExpr::Rollup { link, target, op } => check_link_ref(schema, name, link)
                        .and_then(|()| check_target(name, target.as_deref(), *op)),
                    _ => Ok(()),
                };
                if let Err(e) = r {
                    err = Some(e);
                }
            });
            if let Some(e) = err {
                return Err(e);
            }
        }
        _ => {}
    }
    if f.field_type.is_derived() && f.indexed {
        let ok_type = matches!(
            f.result_type,
            Some(ResultType::Number | ResultType::Text | ResultType::Date | ResultType::DateTime)
        );
        if f.materialization != Some(Materialization::Invariant) || !ok_type {
            return bad(
                "閲覧者ごとに値が変わる（per_viewer）・時刻に依存する（volatile）・索引に載らない型の派生フィールドは indexed にできません",
            );
        }
    }
    Ok(())
}

fn check_link_ref(schema: &TableSchema, name: &str, link: &str) -> Result<(), DataError> {
    match schema.field(link) {
        Some(l) if l.field_type == FieldType::RecordRef && l.ref_table.is_some() => Ok(()),
        Some(_) => Err(DataError::Invalid(format!(
            "'{name}': リンク列 '{link}' は record_ref である必要があります"
        ))),
        None => Err(DataError::Invalid(format!(
            "'{name}': リンク列 '{link}' が fields にありません"
        ))),
    }
}

fn check_target(name: &str, target: Option<&str>, op: RollupOp) -> Result<(), DataError> {
    match target {
        Some(t) if !is_valid_field_key(t) => Err(DataError::Invalid(format!(
            "'{name}': ロールアップの target が不正です"
        ))),
        None if op != RollupOp::Count => Err(DataError::Invalid(format!(
            "'{name}': {op:?} には target が必要です"
        ))),
        _ => Ok(()),
    }
}

/// 派生フィールドが参照する (リンク列, 参照先の列) の組。
fn cross_refs(f: &FieldDef) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    if let Some(d) = &f.lookup {
        out.push((d.via_field.clone(), Some(d.target_field.clone())));
    }
    if let Some(d) = &f.rollup {
        out.push((d.link.clone(), d.target.clone()));
    }
    if let Some(e) = &f.formula {
        for n in e.rollups() {
            if let FormulaExpr::Rollup { link, target, .. } = n {
                out.push((link.clone(), target.clone()));
            }
        }
    }
    out
}

/// スキーマ保存の Tx で使う準備結果。
#[derive(Debug, Default)]
pub(crate) struct DerivedSave {
    /// 材料化した列の依存登録（列, 参照先の内部キー, リンク列）。
    pub deps: Vec<(FieldId, i64, FieldId)>,
    /// 対称列を参照先へ作るか。
    pub needs_symmetric: bool,
}

/// 入力のサーバ決定フィールドを捨て、リンク定義の不変条件を確かめる。
fn reset_server_fields(
    schema: &mut TableSchema,
    current: Option<&TableSchema>,
) -> Result<bool, DataError> {
    let mut needs_symmetric = false;
    for f in &mut schema.fields {
        f.materialization = None;
        f.result_type = None;
        let cur = current.and_then(|c| c.field_by_id(f.id));
        match cur {
            Some(c) => {
                // 既存のリンク定義は不変（カーディナリティの変更は既存行の再検証が要る・§7.1）。
                if f.link != c.link {
                    return Err(DataError::Invalid(format!(
                        "フィールド '{}' の link は変更できません",
                        f.key
                    )));
                }
            }
            None => {
                if let Some(l) = &f.link {
                    // 対称列（逆方向）と対称列の ID はサーバだけが作る。
                    if l.reverse || l.symmetric_field.is_some() {
                        return Err(DataError::Invalid(format!(
                            "フィールド '{}': link.reverse / link.symmetric_field は指定できません（symmetric_key で作成します）",
                            f.key
                        )));
                    }
                    needs_symmetric |= l.symmetric_key.is_some();
                }
            }
        }
    }
    Ok(needs_symmetric)
}

impl DataStore {
    /// 派生フィールドの非同期検証と材料化可否の決定（スキーマ保存の前段）。
    ///
    /// `table_id` は改訂なら保存するテーブル、作成なら `None`（作成者が owner になる前提で ① を判定）。
    #[allow(clippy::too_many_lines)]
    pub(crate) async fn prepare_derived(
        &self,
        ctx: &AuthContext,
        table_id: Option<Uuid>,
        app_id: Option<Uuid>,
        has_record_shares: bool,
        schema: &mut TableSchema,
        current: Option<&TableSchema>,
    ) -> Result<DerivedSave, DataError> {
        let needs_symmetric = reset_server_fields(schema, current)?;
        // 対称列は参照先のスキーマを書き換えるので、参照先の owner 権限を要求する。
        for f in &schema.fields {
            let new = current.is_none_or(|c| c.field_by_id(f.id).is_none());
            let wants = f
                .link
                .as_ref()
                .is_some_and(|l| l.symmetric_key.is_some() && !l.reverse);
            if new && wants {
                if let Some(rt) = f.ref_table {
                    self.require_on(ctx, rt, Relation::Owner).await?;
                }
            }
        }
        if !schema.fields.iter().any(|f| f.field_type.is_derived()) {
            return Ok(DerivedSave {
                deps: Vec::new(),
                needs_symmetric,
            });
        }
        let root = table_id.unwrap_or(Uuid::nil());
        // 新しく参照する（定義が変わった）派生フィールドの参照先は、作者が見えるテーブルに限る
        // （他テーブルの列の存在・型を検証エラーから探れないように）。
        let mut must_view: HashSet<Uuid> = HashSet::new();
        for f in schema.fields.iter().filter(|f| f.field_type.is_derived()) {
            let unchanged = current.and_then(|c| c.field_by_id(f.id)).is_some_and(|c| {
                c.lookup == f.lookup && c.rollup == f.rollup && c.formula == f.formula
            });
            if unchanged {
                continue;
            }
            for (link, _) in cross_refs(f) {
                if let Some(rt) = schema.field(&link).and_then(|l| l.ref_table) {
                    must_view.insert(rt);
                }
            }
        }
        for rt in &must_view {
            if *rt != root && !self.can(ctx, *rt, Relation::Viewer).await? {
                return Err(DataError::Invalid(
                    "派生フィールドの参照先テーブルが見つかりません".into(),
                ));
            }
        }
        let cat = self
            .load_catalog(ctx, root, app_id, has_record_shares, schema)
            .await?;
        cat.check_cycles(root)?;
        for f in schema.fields.iter().filter(|f| f.field_type.is_derived()) {
            if let Some(d) = &f.rollup {
                cat.check_rollup(root, &f.key, &d.link, d.target.as_deref(), d.op)?;
            }
            if let Some(e) = &f.formula {
                for n in e.rollups() {
                    if let FormulaExpr::Rollup { link, target, op } = n {
                        cat.check_rollup(root, &f.key, link, target.as_deref(), *op)?;
                    }
                }
            }
        }
        // ① テーブル ReBAC の包含（同一ミニアプリの所有テーブルどうしだけを判定する）。
        let mut contained: HashSet<Uuid> = HashSet::new();
        if app_id.is_some() {
            let refs: HashSet<Uuid> = schema
                .fields
                .iter()
                .filter(|f| f.field_type.is_derived())
                .flat_map(cross_refs)
                .filter_map(|(link, _)| cat.ref_of(root, &link))
                .filter(|rt| cat.tables.get(rt).is_some_and(|t| t.app_id == app_id))
                .collect();
            let owner = ctx.subject();
            for rt in refs {
                let ok = viewers_contained(
                    self.authz.as_ref(),
                    ctx,
                    table_id,
                    table_id.is_none().then_some(&owner),
                    rt,
                )
                .await?;
                if ok {
                    contained.insert(rt);
                }
            }
        }
        let mut deps = Vec::new();
        let decided: Vec<(FieldId, Materialization, Option<ResultType>)> = schema
            .fields
            .iter()
            .filter(|f| f.field_type.is_derived())
            .map(|f| {
                (
                    f.id,
                    cat.materialization(root, &f.key, &contained),
                    cat.infer(root, &f.key).result_type(),
                )
            })
            .collect();
        for (id, m, ty) in decided {
            let Some(f) = schema.fields.iter_mut().find(|f| f.id == id) else {
                continue;
            };
            f.materialization = Some(m);
            f.result_type = ty;
            if f.is_materialized() {
                for (rt, link) in cat.materialized_refs(root, &f.key) {
                    let (Some(t), Some(l)) = (
                        cat.tables.get(&rt),
                        cat.tables
                            .get(&root)
                            .and_then(|r| r.schema.field(&link))
                            .map(|l| l.id),
                    ) else {
                        continue;
                    };
                    deps.push((id, t.keys.table_int, l));
                }
            }
        }
        Ok(DerivedSave {
            deps,
            needs_symmetric,
        })
    }

    /// 保存するスキーマから参照先テーブルを推移的に読み込む（tenant スコープ・上限つき）。
    async fn load_catalog(
        &self,
        ctx: &AuthContext,
        root: Uuid,
        app_id: Option<Uuid>,
        has_record_shares: bool,
        schema: &TableSchema,
    ) -> Result<Catalog, DataError> {
        let mut cat = Catalog::default();
        let root_keys = self
            .table_keys_of(&ctx.tenant_id, root)
            .await?
            .unwrap_or_default();
        cat.tables.insert(
            root,
            CatalogTable {
                schema: schema.clone(),
                app_id,
                has_record_shares,
                keys: root_keys,
            },
        );
        let mut queue: VecDeque<Uuid> = VecDeque::from([root]);
        let mut loaded: HashMap<Uuid, bool> = HashMap::from([(root, true)]);
        while let Some(t) = queue.pop_front() {
            let refs: Vec<Uuid> = cat.tables.get(&t).map_or_else(Vec::new, |ct| {
                ct.schema
                    .fields
                    .iter()
                    .filter(|f| f.field_type.is_derived())
                    .flat_map(cross_refs)
                    .filter_map(|(link, _)| ct.schema.field(&link).and_then(|l| l.ref_table))
                    .collect()
            });
            for rt in refs {
                if loaded.contains_key(&rt) {
                    continue;
                }
                if loaded.len() >= MAX_CATALOG_TABLES {
                    return Err(DataError::Invalid(format!(
                        "派生フィールドが辿るテーブルが多すぎます（最大 {MAX_CATALOG_TABLES}）"
                    )));
                }
                let row: Option<TableRow> = sqlx::query_as(&format!(
                    "SELECT {TABLE_COLS} FROM data_table \
                     WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL"
                ))
                .bind(&ctx.tenant_id)
                .bind(rt)
                .fetch_optional(self.pools.for_tenant(&ctx.tenant_id))
                .await
                .map_err(map_db)?;
                loaded.insert(rt, row.is_some());
                if let Some(row) = row {
                    let t = row.into_table();
                    cat.tables.insert(rt, super::materialize::catalog_entry(&t));
                    queue.push_back(rt);
                }
            }
        }
        Ok(cat)
    }

    /// テーブルへの relation を持つか（監査を残さない内部判定）。
    async fn can(&self, ctx: &AuthContext, id: Uuid, rel: Relation) -> Result<bool, DataError> {
        self.authz
            .check(
                &ctx.subject(),
                rel,
                &ctx.ns().data_table(&id.to_string()),
                Consistency::HigherConsistency,
            )
            .await
            .map_err(|e| DataError::Internal(e.to_string()))
    }

    /// テーブルへの relation を要求する（不足は 403）。
    async fn require_on(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        rel: Relation,
    ) -> Result<(), DataError> {
        if self.can(ctx, id, rel).await? {
            Ok(())
        } else {
            Err(DataError::Forbidden)
        }
    }

    /// スキーマ保存の Tx で、材料化の依存登録・昇格した列の再計算待ち・依存する参照元の降格を行う。
    pub(crate) async fn finish_derived_save(
        conn: &mut sqlx::PgConnection,
        tenant_id: &str,
        table: &crate::model::DataTable,
        current: Option<&TableSchema>,
        prep: &DerivedSave,
        has_rows: bool,
    ) -> Result<(), DataError> {
        super::materialize::sync_deps(conn, table.keys, &prep.deps).await?;
        // 新たに材料化する列（新規・昇格）は全レコードを再計算待ちにする（終わるまでクエリ対象外）。
        if has_rows {
            let promoted: Vec<FieldId> = table
                .schema
                .fields
                .iter()
                .filter(|f| f.is_materialized())
                .filter(|f| {
                    !current
                        .and_then(|c| c.field_by_id(f.id))
                        .is_some_and(FieldDef::is_materialized)
                })
                .map(|f| f.id)
                .collect();
            super::materialize::mark_all_pending(conn, table.keys, tenant_id, table.id, &promoted)
                .await?;
        }
        // このテーブルを参照先にする材料化列は、行述語・マスクの追加で前提が崩れたら降格する（PIT-71）。
        super::materialize::demote_dependents(
            conn,
            tenant_id,
            table.keys,
            Some((table.id, &super::materialize::catalog_entry(table))),
            "参照先に row_policy / field_policy が追加された",
        )
        .await
    }
}

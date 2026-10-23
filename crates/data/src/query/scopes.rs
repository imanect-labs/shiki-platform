//! リンクの半結合・結合の参照先テーブル（Task 15.10・data-platform §16.1 の JOIN の束縛規則）。
//!
//! 参照先は**閲覧者本人の権限で**開く（PIT-20: lookup と同じ）。テーブル viewer（ReBAC）・フィールドマスク・
//! 行述語の材料をリクエストスコープの認可メモから引き、降下では参照先の行述語（`Guard(P)`）を必ず合成する。
//! 参照先を開けない（viewer でない・消えた）リンクを辿るクエリは 403（IR 直投入）にする。SQL 前段面は
//! テーブル名で解決するので、見えないテーブルは「存在しない」と同じ応答になる（[`crate::sql`]）。

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use authz::AuthContext;
use sqlx::PgConnection;
use uuid::Uuid;

use super::bound::{BoundQuery, Expr};
use super::ir::{Condition, DataQuery};
use super::planner::guard::Guard;
use super::planner::lower::ScopeLow;
use crate::index::state::load_states;
use crate::index::{IndexState, IndexTable};
use crate::model::{DataTable, FieldId, FieldType, TableSchema};
use crate::policy::compile_index::IndexGuardCtx;
use crate::policy::material::{self, PolicyMaterial};
use crate::policy::memo::AuthzMemo;
use crate::query::sql::SqlBuf;
use crate::store::DataStore;
use crate::DataError;

/// 参照先テーブル 1 つ（閲覧者本人の権限で開いたもの）。
pub(crate) struct ScopeTable {
    pub table: DataTable,
    pub masked: HashSet<String>,
    pub material: Arc<PolicyMaterial>,
}

/// 参照先テーブルの集合（テーブル ID で引く）。
#[derive(Default)]
pub(crate) struct ScopeTables {
    by_id: HashMap<Uuid, ScopeTable>,
}

impl ScopeTables {
    pub(crate) fn get(&self, id: Uuid) -> Option<&ScopeTable> {
        self.by_id.get(&id)
    }

    pub(crate) fn insert(&mut self, t: ScopeTable) {
        self.by_id.insert(t.table.id, t);
    }

    pub(crate) fn contains(&self, id: Uuid) -> bool {
        self.by_id.contains_key(&id)
    }
}

/// 条件木のリンクの半結合が参照する列（参照名・参照先の条件）。
fn link_conds<'q>(c: &'q Condition, out: &mut Vec<(&'q str, Option<&'q Condition>)>) {
    match c {
        Condition::All(v) | Condition::Any(v) => v.iter().for_each(|x| link_conds(x, out)),
        Condition::Not(x) => link_conds(x, out),
        Condition::Cmp(_) => {}
        Condition::Link(l) => out.push((l.field.as_str(), l.filter.as_deref())),
    }
}

/// リンク列の参照先テーブル（`record_ref` でなければ `None`・束縛が 422 にする）。
fn ref_of(schema: &TableSchema, key: &str) -> Option<Uuid> {
    schema
        .field(key)
        .filter(|f| f.field_type == FieldType::RecordRef)
        .and_then(|f| f.ref_table)
}

impl DataStore {
    /// クエリが辿る参照先テーブルを閲覧者本人の権限で開く（半結合の入れ子・結合の連鎖を幅優先で）。
    pub(crate) async fn load_scope_tables(
        &self,
        ctx: &AuthContext,
        az: &AuthzMemo<'_>,
        base: &DataTable,
        q: &DataQuery,
        trace_id: Option<&str>,
    ) -> Result<ScopeTables, DataError> {
        let mut out = ScopeTables::default();
        if q.joins.is_empty() && !q.filter.as_ref().is_some_and(has_link) {
            return Ok(out);
        }
        // (表のスキーマ, 条件) の待ち行列。結合は別名 → 表を引きながら順に辿る。
        let mut alias_table: HashMap<String, Uuid> = HashMap::new();
        let mut pending: Vec<(Uuid, Condition)> = Vec::new();
        if let Some(f) = &q.filter {
            pending.push((base.id, f.clone()));
        }
        for j in &q.joins {
            let from_schema = match &j.from {
                None => Some(base.schema.clone()),
                Some(a) => alias_table
                    .get(a)
                    .and_then(|id| out.get(*id))
                    .map(|t| t.table.schema.clone()),
            };
            let Some(rt) = from_schema.as_ref().and_then(|s| ref_of(s, &j.field)) else {
                continue;
            };
            self.open_scope(ctx, az, rt, &mut out, trace_id).await?;
            alias_table.insert(j.alias.clone(), rt);
            if let Some(f) = &j.filter {
                pending.push((rt, f.clone()));
            }
        }
        let mut guard = 0;
        while let Some((table_id, cond)) = pending.pop() {
            guard += 1;
            if guard > 64 {
                break;
            }
            let schema = if table_id == base.id {
                base.schema.clone()
            } else if let Some(t) = out.get(table_id) {
                t.table.schema.clone()
            } else {
                continue;
            };
            let mut links = Vec::new();
            link_conds(&cond, &mut links);
            for (key, sub) in links {
                let Some(rt) = ref_of(&schema, key) else {
                    continue;
                };
                self.open_scope(ctx, az, rt, &mut out, trace_id).await?;
                if let Some(sub) = sub {
                    pending.push((rt, sub.clone()));
                }
            }
        }
        Ok(out)
    }

    async fn open_scope(
        &self,
        ctx: &AuthContext,
        az: &AuthzMemo<'_>,
        id: Uuid,
        out: &mut ScopeTables,
        trace_id: Option<&str>,
    ) -> Result<(), DataError> {
        if out.contains(id) {
            return Ok(());
        }
        // 参照先のテーブル viewer（ReBAC）を閲覧者本人で確かめる。開けない参照先を辿るクエリは 403。
        let table = self
            .open_for_read(ctx, az, id, "data.query.link", trace_id)
            .await
            .map_err(|e| match e {
                DataError::NotFound | DataError::Forbidden => DataError::Forbidden,
                other => other,
            })?;
        let masked = self.masked_fields(ctx, az, &table).await?;
        let material = match &table.schema.row_policy {
            Some(p) => material::resolve(ctx, az, &[&p.read]).await?,
            None => Arc::new(PolicyMaterial::default()),
        };
        out.insert(ScopeTable {
            table,
            masked,
            material,
        });
        Ok(())
    }
}

pub(crate) fn has_link(c: &Condition) -> bool {
    let mut v = Vec::new();
    link_conds(c, &mut v);
    !v.is_empty()
}

/// 参照先テーブル 1 つの実行時の材料（索引の状態・個別共有の `record_int`）。
pub(crate) struct ScopeRt {
    pub table_id: Uuid,
    pub active: HashSet<(FieldId, IndexTable)>,
    pub shared_ints: Vec<i64>,
}

impl DataStore {
    /// 参照先テーブルの実行時の材料を読む（読取 Tx の中で・束縛済みのスコープが使うものだけ）。
    pub(crate) async fn scope_runtime(
        conn: &mut PgConnection,
        ctx: &AuthContext,
        tables: &ScopeTables,
        bq: &BoundQuery,
    ) -> Result<Vec<ScopeRt>, DataError> {
        let ids: BTreeSet<Uuid> = bq.scopes.iter().map(|s| s.table).collect();
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let Some(st) = tables.get(id) else {
                return Err(DataError::Forbidden);
            };
            let states = load_states(conn, st.table.keys).await?;
            let active: HashSet<(FieldId, IndexTable)> = states
                .iter()
                .filter(|(_, _, s)| *s == IndexState::Active)
                .map(|(f, t, _)| (*f, *t))
                .collect();
            let mut shared_ints: Vec<i64> = Self::record_ints_for(
                conn,
                &ctx.tenant_id,
                st.table.keys,
                &st.material.shared_record_ids,
            )
            .await?
            .into_values()
            .collect();
            shared_ints.sort_unstable();
            out.push(ScopeRt {
                table_id: id,
                active,
                shared_ints,
            });
        }
        Ok(out)
    }
}

/// スコープごとの降下の材料を組む（参照先の条件が要る索引が `active` でなければ「準備中」）。
pub(crate) fn scope_lows<'a>(
    ctx: &'a AuthContext,
    tables: &'a ScopeTables,
    rts: &'a [ScopeRt],
    bq: &'a BoundQuery,
) -> Result<Vec<ScopeLow<'a>>, DataError> {
    let mut out = Vec::with_capacity(bq.scopes.len());
    for sc in &bq.scopes {
        let (Some(st), Some(rt)) = (
            tables.get(sc.table),
            rts.iter().find(|r| r.table_id == sc.table),
        ) else {
            return Err(DataError::Forbidden);
        };
        // 参照先の条件が使う索引（バックフィル中なら準備中・半端な索引で絞らない）。
        for f in sc.q.fields.values() {
            if f.is_pseudo() {
                continue;
            }
            if !rt.active.contains(&(f.id, f.value_table())) {
                return Err(DataError::IndexPreparing {
                    field: f.key.clone(),
                    progress: None,
                });
            }
        }
        let guard = Guard::new(IndexGuardCtx {
            schema: &st.table.schema,
            material: &st.material,
            user: &ctx.principal.id,
            active: &rt.active,
            shared_ints: &rt.shared_ints,
        });
        // 行述語の降下の誤り（未知フィールド等）は実行前に検出する（降下では不成立へ倒すため）。
        let mut probe = SqlBuf::scoped(0, 0);
        guard.lower("x.record_int", &mut probe)?;
        out.push(ScopeLow {
            q: &sc.q,
            table_int: st.table.keys.table_int,
            guard,
        });
    }
    Ok(out)
}

/// record_ref の比較値（参照先テーブル → uuid の並び）。FROM の表・半結合の参照先・結合先の条件を
/// すべて辿る（`record_int` への引き直しは実行時・外へ出さない）。
pub(crate) fn ref_values(bq: &BoundQuery) -> HashMap<Uuid, Vec<Uuid>> {
    fn walk(e: &Expr, q: &BoundQuery, root: &BoundQuery, out: &mut HashMap<Uuid, Vec<Uuid>>) {
        let mut atoms = Vec::new();
        e.atoms(&mut atoms);
        for a in atoms {
            let f = q.field(a.field);
            let (super::bound::FieldKind::Link, Some(rt)) = (f.kind, f.ref_table) else {
                continue;
            };
            let vals: Vec<&super::bound::Val> = match &a.leaf {
                super::bound::Leaf::Eq(v) => vec![v],
                super::bound::Leaf::In(v) => v.iter().collect(),
                _ => Vec::new(),
            };
            for v in vals {
                if let super::bound::Val::Ref(u) = v {
                    out.entry(rt).or_default().push(*u);
                }
            }
        }
        let mut links = Vec::new();
        e.links(&mut links);
        for l in links {
            if let Some(sc) = root.scopes.get(l.scope) {
                walk(&l.filter, &sc.q, root, out);
            }
        }
    }
    let mut out = HashMap::new();
    walk(&bq.filter, bq, bq, &mut out);
    for j in &bq.joins {
        if let Some(sc) = bq.scopes.get(j.scope) {
            walk(&j.filter, &sc.q, bq, &mut out);
        }
    }
    out
}

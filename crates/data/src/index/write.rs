//! 書込経路の索引更新（write-through・data-platform §3.6）。
//!
//! レコード書込と**同一トランザクション**で、索引状態が `building` / `active` の
//! (フィールド, 索引テーブル) の行だけを書き換える。
//!
//! - **差分更新**: update / FSM 遷移は `FieldPatch`（v1 の差分）に載ったフィールドの索引行だけを触る。
//!   変わらないフィールドの索引行は 1 行も書き換えない（擬似列 `updated_at` だけは毎回動く）。
//! - create は全フィールド＋擬似列 3 行、delete はそのレコードの全索引行を消す。複数レコードの create
//!   （一括書込パス・data-platform §16.7）は**同じキー生成・同じ文**で、表ごとに 1 文にまとめて書く
//!   （各行が `record_int` を持つ）。
//! - バックフィル（[`super::backfill`]）は `building` の組だけを作り直す（冪等）。
//!
//! 値は全てバインド。SQL へ埋め込むのは固定の表名（[`IndexTable::relation`]）だけ（PIT-21）。

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use sqlx::PgConnection;

use super::link::{replace_forward, replace_link, LinkResolver};
use super::normalize::{field_keys, format_timestamp, text_key, FieldKeys};
use super::state::{field_id_i16, IndexPlan};
use super::IndexTable;
use crate::model::{
    FieldId, FieldPatch, TableSchema, FIRST_USER_FIELD_ID, SYSTEM_FIELD_CREATED_AT,
    SYSTEM_FIELD_OWNER, SYSTEM_FIELD_UPDATED_AT,
};
use crate::{map_db, DataError};

/// リンク列 1 つ分の参照先（ord, 参照先の外部 ID）。
type LinkTargets = Vec<(i16, uuid::Uuid)>;

/// 索引行を書くレコード（外部形 `key → 値` の保存値と擬似列）。
pub(crate) struct IndexedRecord<'a> {
    pub record_int: i64,
    pub owner: &'a str,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// 外部形（`key → 値`）の保存値（書込検証で正規化済み）。
    pub data: &'a Map<String, Value>,
}

/// 既存の索引行の扱い。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// 新規レコード（既存行が無いので消さない）。
    Fresh,
    /// 書込: 消してから入れる。unique は旧値の行を消してから素の INSERT（重複は 409）。
    Replace,
    /// バックフィル: 消してから入れる。unique は「同じレコードの行なら上書き」で冪等にする。
    Rebuild,
}

/// 1 フィールド × 1 索引テーブルの書換え。
struct Op<'v> {
    field: FieldId,
    table: IndexTable,
    old: Option<&'v Value>,
    new: Option<&'v Value>,
}

/// 書込 Tx 上の索引ライタ（[`super::state::lock_plan`] で得た計画に従う）。
pub(crate) struct IndexWriter<'a> {
    pub plan: &'a IndexPlan,
    pub schema: &'a TableSchema,
    /// 参照先テーブルの解決に使う（同一テナント内）。
    pub tenant_id: &'a str,
    pub max_multi_values: usize,
}

impl IndexWriter<'_> {
    /// create: 保守対象の全フィールド＋擬似列 3 行を書く（書込経路は
    /// [`crate::record_effects`] 経由で [`Self::on_create_many`] を呼ぶ）。
    #[cfg(test)]
    pub(crate) async fn on_create(
        &self,
        conn: &mut PgConnection,
        rec: &IndexedRecord<'_>,
    ) -> Result<(), DataError> {
        self.on_create_many(conn, std::slice::from_ref(rec), |_, _| false)
            .await
    }

    /// create（複数レコード）: 各レコードの保守対象の全フィールド＋擬似列を、**表ごとに 1 文**で書く。
    ///
    /// 1 件の create と同じ規則（同じキー生成・同じ INSERT 文）で、レコード数に依らず文の数が一定になる
    /// （一括書込パス・data-platform §16.7）。`defer(field, table)` が真の組は書かない（索引の遅延構築:
    /// 呼び出し側がその組を `building` にしてあり、適用後のバックフィルが本体から作る・§10）。
    pub(crate) async fn on_create_many(
        &self,
        conn: &mut PgConnection,
        recs: &[IndexedRecord<'_>],
        defer: impl Fn(FieldId, IndexTable) -> bool,
    ) -> Result<(), DataError> {
        let mut batch = Batch::default();
        let mut links: Vec<(i64, FieldId, LinkTargets)> = Vec::new();
        for rec in recs {
            let sys = SystemValues::of(rec);
            let mut ops = Vec::new();
            for (field, tables) in self.plan.fields() {
                let new = self.value_of(field, rec.data, &sys);
                for t in tables {
                    if defer(field, *t) {
                        continue;
                    }
                    ops.push(Op {
                        field,
                        table: *t,
                        old: None,
                        new,
                    });
                }
            }
            self.collect(&mut batch, &mut links, rec.record_int, &ops, Mode::Fresh)?;
        }
        self.flush(conn, &batch, Mode::Fresh).await?;
        self.write_links(conn, links).await?;
        for rec in recs {
            self.refresh_sorts(conn, rec.record_int, None).await?;
        }
        Ok(())
    }

    /// update / FSM 遷移: 差分（`patches`・外部形 key）に載ったフィールドと `updated_at` だけを書き換える。
    pub(crate) async fn on_update(
        &self,
        conn: &mut PgConnection,
        record_int: i64,
        patches: &[FieldPatch],
        updated_at: DateTime<Utc>,
    ) -> Result<(), DataError> {
        let updated = Value::String(format_timestamp(updated_at));
        let mut ops = Vec::new();
        for p in patches {
            let Some(def) = self.schema.field(&p.field) else {
                continue;
            };
            for t in self.plan.tables_for(def.id) {
                ops.push(Op {
                    field: def.id,
                    table: *t,
                    old: non_null(&p.old),
                    new: non_null(&p.new),
                });
            }
        }
        for t in self.plan.tables_for(SYSTEM_FIELD_UPDATED_AT) {
            ops.push(Op {
                field: SYSTEM_FIELD_UPDATED_AT,
                table: *t,
                old: None,
                new: Some(&updated),
            });
        }
        self.apply(conn, record_int, &ops, Mode::Replace).await?;
        let mut changed: Vec<FieldId> = patches
            .iter()
            .filter_map(|p| self.schema.field(&p.field).map(|d| d.id))
            .collect();
        changed.push(SYSTEM_FIELD_UPDATED_AT);
        self.refresh_sorts(conn, record_int, Some(&changed)).await
    }

    /// 複数ソートの複合索引を作り直す（変更のあった列を含む組み合わせだけ・`None` は全て）。
    async fn refresh_sorts(
        &self,
        conn: &mut PgConnection,
        record_int: i64,
        changed: Option<&[FieldId]>,
    ) -> Result<(), DataError> {
        for set in &self.plan.sorts {
            if changed.is_some_and(|c| !set.keys.iter().any(|(f, _)| c.contains(f))) {
                continue;
            }
            super::composite::refresh(conn, self.plan.keys, self.schema, set, record_int).await?;
        }
        Ok(())
    }

    /// delete: そのレコードの索引行を全て消す（`old` は外部形の保存値・unique 行の特定に使う）。
    pub(crate) async fn on_delete(
        &self,
        conn: &mut PgConnection,
        record_int: i64,
        old: &Map<String, Value>,
    ) -> Result<(), DataError> {
        let mut ops = Vec::new();
        for (field, tables) in self.plan.fields() {
            let old_value = self
                .schema
                .field_by_id(field)
                .and_then(|d| old.get(&d.key))
                .and_then(non_null);
            for t in tables {
                ops.push(Op {
                    field,
                    table: *t,
                    old: old_value,
                    new: None,
                });
            }
        }
        self.apply(conn, record_int, &ops, Mode::Replace).await?;
        if !self.plan.sorts.is_empty() {
            super::composite::delete_record(conn, self.plan.keys, record_int).await?;
        }
        Ok(())
    }

    /// バックフィル: 指定の (フィールド, 索引テーブル) だけを本体から作り直す（冪等）。
    pub(crate) async fn rebuild(
        &self,
        conn: &mut PgConnection,
        rec: &IndexedRecord<'_>,
        targets: &[(FieldId, IndexTable)],
    ) -> Result<(), DataError> {
        let sys = SystemValues::of(rec);
        let ops: Vec<Op<'_>> = targets
            .iter()
            .map(|(field, table)| Op {
                field: *field,
                table: *table,
                old: None,
                new: self.value_of(*field, rec.data, &sys),
            })
            .collect();
        self.apply(conn, rec.record_int, &ops, Mode::Rebuild).await
    }

    /// バックフィル（複数レコード）: [`Self::rebuild`] と同じ規則で、表ごとに 1 文にまとめて作り直す。
    ///
    /// unique を含む組は 1 件ずつ（同じ文の中で 2 行が同じ値に当たると `ON CONFLICT DO UPDATE` が
    /// 409 ではなく文のエラーになるため・重複の検出を 1 件の書込と同じ形に保つ）。
    pub(crate) async fn rebuild_many(
        &self,
        conn: &mut PgConnection,
        recs: &[IndexedRecord<'_>],
        targets: &[(FieldId, IndexTable)],
    ) -> Result<(), DataError> {
        if targets.iter().any(|(_, t)| *t == IndexTable::Unique) {
            for rec in recs {
                self.rebuild(conn, rec, targets).await?;
            }
            return Ok(());
        }
        let mut batch = Batch::default();
        let mut links = Vec::new();
        for rec in recs {
            let sys = SystemValues::of(rec);
            let ops: Vec<Op<'_>> = targets
                .iter()
                .map(|(field, table)| Op {
                    field: *field,
                    table: *table,
                    old: None,
                    new: self.value_of(*field, rec.data, &sys),
                })
                .collect();
            self.collect(&mut batch, &mut links, rec.record_int, &ops, Mode::Rebuild)?;
        }
        self.flush(conn, &batch, Mode::Rebuild).await?;
        self.write_links(conn, links).await
    }

    fn value_of<'v>(
        &self,
        field: FieldId,
        data: &'v Map<String, Value>,
        sys: &'v SystemValues,
    ) -> Option<&'v Value> {
        match field {
            SYSTEM_FIELD_OWNER => Some(&sys.owner),
            SYSTEM_FIELD_CREATED_AT => Some(&sys.created_at),
            SYSTEM_FIELD_UPDATED_AT => Some(&sys.updated_at),
            _ => self
                .schema
                .field_by_id(field)
                .and_then(|d| data.get(&d.key))
                .and_then(non_null),
        }
    }

    /// 1 フィールドの索引キー（擬似列は値そのものを text キーに）。
    fn keys_of(
        &self,
        field: FieldId,
        table: IndexTable,
        v: Option<&Value>,
    ) -> Result<Option<FieldKeys>, DataError> {
        let Some(v) = v else { return Ok(None) };
        if field < FIRST_USER_FIELD_ID {
            let s = v.as_str().ok_or_else(|| {
                DataError::Internal(format!("擬似列 {field} の値が文字列ではありません"))
            })?;
            return Ok(Some(FieldKeys::Text(vec![text_key(0, s)])));
        }
        let Some(def) = self.schema.field_by_id(field) else {
            return Ok(None);
        };
        field_keys(def, table, v, self.max_multi_values)
    }

    async fn apply(
        &self,
        conn: &mut PgConnection,
        record_int: i64,
        ops: &[Op<'_>],
        mode: Mode,
    ) -> Result<(), DataError> {
        let mut batch = Batch::default();
        let mut links = Vec::new();
        self.collect(&mut batch, &mut links, record_int, ops, mode)?;
        self.flush(conn, &batch, mode).await?;
        self.write_links(conn, links).await
    }

    /// 1 レコード分の書換えを `batch`（表ごとの行）と `links`（リンク列ごとの参照先）へ積む。
    fn collect(
        &self,
        batch: &mut Batch,
        links: &mut Vec<(i64, FieldId, LinkTargets)>,
        record_int: i64,
        ops: &[Op<'_>],
        mode: Mode,
    ) -> Result<(), DataError> {
        for op in ops {
            // 定義の無いユーザー列（台帳とスキーマの食い違い）は触らない（消しもしない）。
            if op.field >= FIRST_USER_FIELD_ID && self.schema.field_by_id(op.field).is_none() {
                continue;
            }
            let fid = field_id_i16(op.field)?;
            if op.table != IndexTable::Link && op.table != IndexTable::Unique {
                batch.delete(record_int, op.table, fid);
            }
            match (op.table, self.keys_of(op.field, op.table, op.new)?) {
                (IndexTable::Link, keys) => {
                    let targets = match keys {
                        Some(FieldKeys::Link(t)) => t,
                        _ => Vec::new(),
                    };
                    links.push((record_int, op.field, targets));
                }
                (IndexTable::Unique, keys) => {
                    if mode == Mode::Replace {
                        if let Some(FieldKeys::Unique(old)) =
                            self.keys_of(op.field, op.table, op.old)?
                        {
                            batch.unique_delete.push((record_int, fid, old));
                        }
                    }
                    if let Some(FieldKeys::Unique(norm)) = keys {
                        batch.unique_insert.push((record_int, fid, norm));
                    }
                }
                (_, Some(FieldKeys::Text(rows))) => {
                    for r in rows {
                        batch
                            .text
                            .push((record_int, fid, r.ord, r.val, r.truncated));
                    }
                }
                (_, Some(FieldKeys::Num(rows))) => {
                    for (ord, n) in rows {
                        batch.num.push((record_int, fid, ord, n));
                    }
                }
                (_, Some(FieldKeys::Search(rows))) => {
                    for (ord, s) in rows {
                        batch.search.push((record_int, fid, ord, s));
                    }
                }
                (_, _) => {}
            }
        }
        Ok(())
    }

    /// 積んだ行を表ごとに 1 文で書く（`Fresh` 以外は先にそのレコードの旧行を消す）。
    async fn flush(
        &self,
        conn: &mut PgConnection,
        batch: &Batch,
        mode: Mode,
    ) -> Result<(), DataError> {
        let keys = self.plan.keys;
        if mode != Mode::Fresh {
            // 同じ (表, フィールド集合) を消すレコードをまとめて 1 文にする（バックフィルのチャンク）。
            let mut groups: Vec<(IndexTable, Vec<i16>, Vec<i64>)> = Vec::new();
            for (record_int, table, fields) in &batch.deletes {
                let mut fields = fields.clone();
                fields.sort_unstable();
                match groups
                    .iter_mut()
                    .find(|(t, f, _)| t == table && *f == fields)
                {
                    Some((_, _, recs)) => recs.push(*record_int),
                    None => groups.push((*table, fields, vec![*record_int])),
                }
            }
            // 消す行のあるテーブルなら本体の行もあるので、パーティションが引ける（引けなければ親へ向ける）。
            let partition = if groups.is_empty() {
                None
            } else {
                super::partition::partition_of(conn, self.tenant_id, keys).await?
            };
            for (table, fields, records) in &groups {
                if let Err(e) = delete_rows(conn, keys, partition, *table, fields, records).await {
                    if partition.is_some() {
                        super::partition::forget_partition(self.tenant_id, keys.table_int);
                    }
                    return Err(e);
                }
            }
            delete_unique(conn, keys, &batch.unique_delete).await?;
        }
        insert_text(conn, keys, &batch.text).await?;
        insert_num(conn, keys, &batch.num).await?;
        insert_search(conn, keys, &batch.search).await?;
        match insert_unique(conn, keys, &batch.unique_insert, mode == Mode::Rebuild).await? {
            None => Ok(()),
            // 範囲外の id は無い列（u16::MAX）として列なしにする。
            Some(fid) => {
                Err(self.unique_violation(FieldId::try_from(fid).unwrap_or(FieldId::MAX), false))
            }
        }
    }

    /// 違反した列を名指しした 409（列の定義が見つからなければ列なし）。
    fn unique_violation(&self, field: FieldId, link: bool) -> DataError {
        DataError::UniqueViolation {
            field: self
                .schema
                .field_by_id(field)
                .map(|f| crate::ConflictField::of(f, link)),
        }
    }

    /// リンク列の行（順方向・対称列があれば逆方向も同じ Tx）を書く。
    async fn write_links(
        &self,
        conn: &mut PgConnection,
        links: Vec<(i64, FieldId, LinkTargets)>,
    ) -> Result<(), DataError> {
        let keys = self.plan.keys;
        let mut resolver = LinkResolver::default();
        for (record_int, field, targets) in links {
            let def = self
                .schema
                .field_by_id(field)
                .ok_or_else(|| DataError::Internal(format!("リンク列 {field} の定義が不明")))?;
            let ref_table = def
                .ref_table
                .ok_or_else(|| DataError::Internal(format!("リンク列 {field} の参照先が不明")))?;
            let mut edges = Vec::with_capacity(targets.len());
            for (ord, id) in targets {
                if let Some((dt, dr)) = resolver
                    .resolve(conn, self.tenant_id, keys.tenant_int, ref_table, id)
                    .await?
                {
                    edges.push((ord, dt, dr));
                }
            }
            // 対称列があれば逆方向行も同じ Tx で書く（両方向が食い違う状態を作らない・§7.1）。
            let symmetric = def
                .link
                .as_ref()
                .filter(|l| !l.reverse)
                .and_then(|l| l.symmetric_field);
            let ref_int = match symmetric {
                Some(_) => resolver.table_int(conn, self.tenant_id, ref_table).await?,
                None => None,
            };
            let written = match (symmetric, ref_int) {
                (Some(sym), Some(rt)) => {
                    replace_link(conn, keys, field, record_int, &edges, (sym, rt)).await
                }
                _ => replace_forward(conn, keys, field, record_int, &edges).await,
            };
            // カーディナリティ（部分一意）の違反は書いたリンク列を名指しする（逆方向行の違反でも、
            // 書き手が書いたのはこの列）。
            written.map_err(|e| match e {
                DataError::UniqueViolation { field: None } => self.unique_violation(field, true),
                other => other,
            })?;
        }
        Ok(())
    }
}

fn non_null(v: &Value) -> Option<&Value> {
    (!v.is_null()).then_some(v)
}

/// 擬似列の値（text キーの元）。
struct SystemValues {
    owner: Value,
    created_at: Value,
    updated_at: Value,
}

impl SystemValues {
    fn of(rec: &IndexedRecord<'_>) -> Self {
        SystemValues {
            owner: Value::String(rec.owner.to_string()),
            created_at: Value::String(format_timestamp(rec.created_at)),
            updated_at: Value::String(format_timestamp(rec.updated_at)),
        }
    }
}

/// 索引行（表ごとにまとめて 1 文で書く）。各行は `record_int` を持つので、複数レコードを 1 文で書ける。
#[derive(Default)]
struct Batch {
    /// (record_int, 表, 消すフィールド)。`Fresh` では使わない。
    deletes: Vec<(i64, IndexTable, Vec<i16>)>,
    text: Vec<(i64, i16, i16, String, bool)>,
    num: Vec<(i64, i16, i16, String)>,
    search: Vec<(i64, i16, i16, String)>,
    unique_delete: Vec<(i64, i16, String)>,
    unique_insert: Vec<(i64, i16, String)>,
}

impl Batch {
    fn delete(&mut self, record_int: i64, table: IndexTable, field: i16) {
        match self
            .deletes
            .iter_mut()
            .find(|(r, t, _)| *r == record_int && *t == table)
        {
            Some((_, _, fields)) if !fields.contains(&field) => fields.push(field),
            Some(_) => {}
            None => self.deletes.push((record_int, table, vec![field])),
        }
    }
}

/// (フィールド, レコード) の組の索引行を消す文（表名・列名は固定集合とパーティション番号からだけ埋め込む・PIT-21）。
///
/// **組ごとに主キー (tenant_int, table_int, field_id, record_int, ord) の前方一致で引く形にする**（検証 D-6）。
/// `field_id = ANY($3) AND record_int = ANY($4)` の形は、新しい列で統計が古いと Postgres が値索引
/// (tenant_int, table_int, field_id, val, record_int) を選び、`record_int` の配列の要素ごとにその列の索引行を
/// 全部読み直した（バックフィルの 1 チャンクの削除が 3.5 秒・行数の 2 乗で遅くなる）。
///
/// 配列を `unnest` との結合に開くだけでは足りない（#915）。統計が古いとハッシュ結合が選ばれ、内側は
/// (tenant_int, table_int, field_id) の 3 列で列の索引行を全部読む。そこで組ごとの探索を
/// `LATERAL … OFFSET 0`（引き上げ不可＝必ず組ごとのパラメータ付き探索）にして行の ctid を集め、
/// 本体は TID で消す。
///
/// `partition` が分かれば、そのパーティションへ直接向け、組ごとの探索では `tenant_int` / `table_int` を
/// `(SELECT $n)` で包んで計画時に見せない（#986）。値が見えると、統計にこのテーブルの行が無いとき選択率が
/// 0 になり、主キーと値索引の推定がどちらも 1 行の下限で並ぶ。差は索引の大きさだけになり、主キーが肥大して
/// いると値索引が選ばれた（1,000 件 × 2 列で 5 秒）。値を見せなければ統計の平均の選択率で見積もられ、
/// 4 列等値の主キーが選ばれる（[`super::partition`]）。親テーブルに向けたまま値を隠すと計画時に
/// パーティションを刈り込めない（全パーティションを計画しロックする）ので、パーティションが分からない
/// ときは値を見せる元の形にする。パーティションごとに文が変わる（最大 3 表 × 分割数）が、名前付きの文として
/// 計画を使い回す（名前なしにすると毎回計画し、1 件 0.34ms・並行時の tps が 1/3 になる）。sqlx の文キャッシュ
/// （接続ごとに既定 100）から溢れた分は計画し直しになるが、それでも名前なしと同じ費用で済む。
/// 値を隠した組ごとの探索は generic plan でも同じ見積もりになり、1 パーティションしか載らないのでロックも増えない。
///
/// 外側の `tenant_int` は `+ 0` で索引に使えない形にする（#961）。統計がその行をほとんど数えていないと
/// (tenant_int, table_int) の索引探索の推定が 1 行まで下がり、TID 探索（推定 10 行）に勝って、テーブルの
/// 索引行を全部たどって ctid で絞る計画になった。`table_int` はパーティションの枝刈りに要るので素のまま
/// 残す（どの索引も先頭は `tenant_int` なので、`table_int` だけでは索引の全走査になり TID 探索に負ける）。
///
/// ctid はパーティションをまたぐと一意でないが、同じテーブルの行は 1 パーティション
/// （`table_int` のハッシュ）に収まり、外側も `tenant_int` / `table_int` で絞るので取り違えない。
pub(crate) fn delete_sql(relation: &str, record_column: &str, partition: Option<u16>) -> String {
    let (rel, tenant, table) = match partition {
        Some(n) => (
            format!("{relation}_p{n}"),
            "(SELECT $1::int4)",
            "(SELECT $2::int8)",
        ),
        None => (relation.to_string(), "$1", "$2"),
    };
    format!(
        "DELETE FROM {rel} t WHERE t.tenant_int + 0 = $1 AND t.table_int = $2 AND t.ctid = ANY(ARRAY(\
         SELECT i.ctid FROM unnest($3::int2[]) AS f(id) CROSS JOIN unnest($4::int8[]) AS r(id) \
         CROSS JOIN LATERAL (SELECT ctid FROM {rel} WHERE tenant_int = {tenant} AND table_int = {table} \
         AND field_id = f.id AND {record_column} = r.id OFFSET 0) i))"
    )
}

/// 値索引（text）の [`delete_sql`]（計画の形の回帰テスト用・実行時には使わない）。
#[doc(hidden)]
pub fn text_index_delete_sql(partition: Option<u16>) -> String {
    delete_sql("data_index_text", "record_int", partition)
}

async fn delete_rows(
    conn: &mut PgConnection,
    keys: crate::model::TableKeys,
    partition: Option<u16>,
    table: IndexTable,
    fields: &[i16],
    records: &[i64],
) -> Result<(), DataError> {
    let sql = delete_sql(table.relation(), table.record_column(), partition);
    sqlx::query(&sql)
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .bind(fields)
        .bind(records)
        .execute(&mut *conn)
        .await
        .map_err(map_db)?;
    Ok(())
}

async fn insert_text(
    conn: &mut PgConnection,
    keys: crate::model::TableKeys,
    rows: &[(i64, i16, i16, String, bool)],
) -> Result<(), DataError> {
    if rows.is_empty() {
        return Ok(());
    }
    let recs: Vec<i64> = rows.iter().map(|r| r.0).collect();
    let fields: Vec<i16> = rows.iter().map(|r| r.1).collect();
    let ords: Vec<i16> = rows.iter().map(|r| r.2).collect();
    let vals: Vec<&str> = rows.iter().map(|r| r.3.as_str()).collect();
    let truncs: Vec<bool> = rows.iter().map(|r| r.4).collect();
    sqlx::query(
        "INSERT INTO data_index_text \
           (tenant_int, table_int, field_id, record_int, ord, val, truncated) \
         SELECT $1, $2, u.f, u.r, u.o, u.v, u.t \
         FROM unnest($3::int8[], $4::int2[], $5::int2[], $6::text[], $7::bool[]) \
           AS u(r, f, o, v, t)",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(&recs)
    .bind(&fields)
    .bind(&ords)
    .bind(&vals)
    .bind(&truncs)
    .execute(&mut *conn)
    .await
    .map_err(map_db)?;
    Ok(())
}

async fn insert_num(
    conn: &mut PgConnection,
    keys: crate::model::TableKeys,
    rows: &[(i64, i16, i16, String)],
) -> Result<(), DataError> {
    if rows.is_empty() {
        return Ok(());
    }
    let recs: Vec<i64> = rows.iter().map(|r| r.0).collect();
    let fields: Vec<i16> = rows.iter().map(|r| r.1).collect();
    let ords: Vec<i16> = rows.iter().map(|r| r.2).collect();
    let vals: Vec<&str> = rows.iter().map(|r| r.3.as_str()).collect();
    sqlx::query(
        "INSERT INTO data_index_num (tenant_int, table_int, field_id, record_int, ord, val) \
         SELECT $1, $2, u.f, u.r, u.o, u.v::numeric \
         FROM unnest($3::int8[], $4::int2[], $5::int2[], $6::text[]) AS u(r, f, o, v)",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(&recs)
    .bind(&fields)
    .bind(&ords)
    .bind(&vals)
    .execute(&mut *conn)
    .await
    .map_err(map_db)?;
    Ok(())
}

async fn insert_search(
    conn: &mut PgConnection,
    keys: crate::model::TableKeys,
    rows: &[(i64, i16, i16, String)],
) -> Result<(), DataError> {
    if rows.is_empty() {
        return Ok(());
    }
    let recs: Vec<i64> = rows.iter().map(|r| r.0).collect();
    let fields: Vec<i16> = rows.iter().map(|r| r.1).collect();
    let ords: Vec<i16> = rows.iter().map(|r| r.2).collect();
    let vals: Vec<&str> = rows.iter().map(|r| r.3.as_str()).collect();
    sqlx::query(
        "INSERT INTO data_index_search (tenant_int, table_int, field_id, record_int, ord, val) \
         SELECT $1, $2, u.f, u.r, u.o, u.v \
         FROM unnest($3::int8[], $4::int2[], $5::int2[], $6::text[]) AS u(r, f, o, v)",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(&recs)
    .bind(&fields)
    .bind(&ords)
    .bind(&vals)
    .execute(&mut *conn)
    .await
    .map_err(map_db)?;
    Ok(())
}

async fn delete_unique(
    conn: &mut PgConnection,
    keys: crate::model::TableKeys,
    rows: &[(i64, i16, String)],
) -> Result<(), DataError> {
    if rows.is_empty() {
        return Ok(());
    }
    let recs: Vec<i64> = rows.iter().map(|r| r.0).collect();
    let fields: Vec<i16> = rows.iter().map(|r| r.1).collect();
    let norms: Vec<&str> = rows.iter().map(|r| r.2.as_str()).collect();
    // 他レコードの行は消さない（record_int も一致させる）。
    sqlx::query(
        "DELETE FROM data_unique WHERE tenant_int = $1 AND table_int = $2 \
           AND (record_int, field_id, val_norm) IN \
               (SELECT * FROM unnest($3::int8[], $4::int2[], $5::text[]))",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(&recs)
    .bind(&fields)
    .bind(&norms)
    .execute(&mut *conn)
    .await
    .map_err(map_db)?;
    Ok(())
}

/// unique の行を入れる。重複した組があれば、その最初の列の `field_id` を `Some` で返す（値・相手の行は返さない）。
///
/// 重複を DB エラーにせず `ON CONFLICT` で飛ばし、入らなかった組から違反した列を特定する（エラーの文言や
/// 制約名を解析しない・Tx を中断させない）。
async fn insert_unique(
    conn: &mut PgConnection,
    keys: crate::model::TableKeys,
    rows: &[(i64, i16, String)],
    idempotent: bool,
) -> Result<Option<i16>, DataError> {
    if rows.is_empty() {
        return Ok(None);
    }
    let recs: Vec<i64> = rows.iter().map(|r| r.0).collect();
    let fields: Vec<i16> = rows.iter().map(|r| r.1).collect();
    let norms: Vec<&str> = rows.iter().map(|r| r.2.as_str()).collect();
    // バックフィルは同じレコードの行なら上書き（冪等）、他レコードの行と衝突したら入らない＝重複。
    // 書込は衝突したら入らない＝重複（409）。
    let conflict = if idempotent {
        " ON CONFLICT (tenant_int, table_int, field_id, val_norm) \
          DO UPDATE SET record_int = excluded.record_int \
          WHERE du.record_int = excluded.record_int"
    } else {
        " ON CONFLICT (tenant_int, table_int, field_id, val_norm) DO NOTHING"
    };
    let sql = format!(
        "INSERT INTO data_unique AS du (tenant_int, table_int, field_id, val_norm, record_int) \
         SELECT $1, $2, u.f, u.v, u.r FROM unnest($3::int8[], $4::int2[], $5::text[]) AS u(r, f, v)\
         {conflict} RETURNING du.record_int, du.field_id, du.val_norm"
    );
    let written: Vec<(i64, i16, String)> = sqlx::query_as(&sql)
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .bind(&recs)
        .bind(&fields)
        .bind(&norms)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                DataError::UniqueViolation { field: None }
            }
            _ => map_db(e),
        })?;
    if written.len() == rows.len() {
        return Ok(None);
    }
    let written: std::collections::HashSet<(i64, i16, &str)> = written
        .iter()
        .map(|(r, f, v)| (*r, *f, v.as_str()))
        .collect();
    Ok(rows
        .iter()
        .find(|(r, f, v)| !written.contains(&(*r, *f, v.as_str())))
        .map(|r| r.1)
        // 数が合わないのに欠けた組が無い＝入力に同じ組が 2 度あった。最初の列で代表させる。
        .or(Some(rows[0].1)))
}

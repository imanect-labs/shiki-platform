//! テーブル記述子とキー表現。SQL 識別子は `'static` 定数のみを受け付け、
//! 組み立て時に英小文字・数字・アンダースコアへ制限する（インジェクション面ゼロ）。

use uuid::Uuid;

/// run 行（claim・リース・fencing の対象）を持つテーブルの記述子。
///
/// chat の `generation_run`、workflow の `step_execution` 等がこの形に乗る。
/// ステータス語彙そのもの（queued/running 以外の端末状態など）はドメイン所有。
#[derive(Debug, Clone, Copy)]
pub struct RunTableSpec {
    /// テーブル名（例: `generation_run`）。
    pub table: &'static str,
    /// ステータスカラム名（例: `status`）。
    pub status_column: &'static str,
    /// fencing token カラム名（bigint・claim ごとに +1）。
    pub fencing_column: &'static str,
    /// リース期限カラム名（timestamptz）。
    pub lease_column: &'static str,
    /// claim したワーカー ID カラム名。
    pub worker_column: &'static str,
    /// claim 時に +1 するリトライ回数カラム。takeover で attempt を増やさないドメイン
    /// （workflow step は attempt 不変で re-run・engine.md §9.5）は `None`。
    pub attempt_column: Option<&'static str>,
    /// 更新時に `now()` を書くカラム（例: `updated_at`）。持たないテーブルは `None`。
    pub updated_at_column: Option<&'static str>,
    /// claim 可能な待機ステータス値（例: `queued`）。
    pub queued_status: &'static str,
    /// 実行中ステータス値（例: `running`）。
    pub running_status: &'static str,
}

/// append-only イベントログテーブルの記述子。
///
/// キーカラムは run テーブルと同一（[`Key`] で与える）である前提。
/// 主キー `(キー, seq)` が重複 seq を拒否し exactly-once を担保する。
#[derive(Debug, Clone, Copy)]
pub struct EventTableSpec {
    /// テーブル名（例: `generation_event`）。
    pub table: &'static str,
    /// 単調 seq カラム名（bigint）。
    pub seq_column: &'static str,
    /// イベント種別カラム名（例: `type` / `kind`）。
    pub kind_column: &'static str,
    /// ペイロードカラム名（jsonb）。
    pub payload_column: &'static str,
}

impl RunTableSpec {
    pub(crate) fn validate(&self) {
        assert_ident(self.table);
        assert_ident(self.status_column);
        assert_ident(self.fencing_column);
        assert_ident(self.lease_column);
        assert_ident(self.worker_column);
        if let Some(c) = self.attempt_column {
            assert_ident(c);
        }
        if let Some(c) = self.updated_at_column {
            assert_ident(c);
        }
        assert_ident(self.queued_status);
        assert_ident(self.running_status);
    }
}

impl EventTableSpec {
    pub(crate) fn validate(&self) {
        assert_ident(self.table);
        assert_ident(self.seq_column);
        assert_ident(self.kind_column);
        assert_ident(self.payload_column);
    }
}

/// キーカラムへバインドする値。複合キー（例: workflow の `(tenant_id, run_id, step_path)`）
/// を型を保ったまま渡すための最小限の直和。
#[derive(Debug, Clone, Copy)]
pub enum KeyValue<'a> {
    Uuid(Uuid),
    Text(&'a str),
    BigInt(i64),
    /// NULL を書き得る任意値（[`fenced_finalize`](crate::fenced_finalize) の追加 SET 用）。
    OptText(Option<&'a str>),
}

/// 行を一意に特定するキー（カラム名の `'static` 列＋対応する値列）。
///
/// キー値は常にプレースホルダ `$1..$n` として最初にバインドされる。
#[derive(Debug, Clone, Copy)]
pub struct Key<'a> {
    columns: &'static [&'static str],
    values: &'a [KeyValue<'a>],
}

impl<'a> Key<'a> {
    /// キーを作る。`columns` と `values` の長さは一致していなければならない。
    pub fn new(columns: &'static [&'static str], values: &'a [KeyValue<'a>]) -> Self {
        // バインド不整合（列数≠値数）はプレースホルダずれ＝不正クエリのため常時検証する。
        assert_eq!(columns.len(), values.len(), "key columns/values mismatch");
        for c in columns {
            assert_ident(c);
        }
        Key { columns, values }
    }

    pub(crate) fn len(&self) -> usize {
        self.columns.len()
    }

    pub(crate) fn values(&self) -> &[KeyValue<'a>] {
        self.values
    }

    /// `col1 = $1 AND col2 = $2 ...`（キーは常に $1 から）。
    pub(crate) fn predicate(&self) -> String {
        self.columns
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c} = ${}", i + 1))
            .collect::<Vec<_>>()
            .join(" AND ")
    }

    /// `col1, col2, ...`（INSERT のカラムリスト用）。
    pub(crate) fn column_list(&self) -> String {
        self.columns.join(", ")
    }

    /// `$1, $2, ...`（INSERT の SELECT リスト用・predicate と同じ番号を再利用）。
    pub(crate) fn placeholders(&self) -> String {
        (1..=self.columns.len())
            .map(|i| format!("${i}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// SQL 識別子（またはステータス定数）を英小文字・数字・アンダースコアに制限する。
/// 値はすべてコード中の `'static` 定数であり、違反はプログラミングエラー。
///
/// SQL インジェクション境界のため **リリースビルドでも常時検証する**（`assert!`）。識別子は
/// クエリ文字列へ直接連結されるため、万一不正な定数（タイポ・空白混入）が入っても本番で
/// 黙って不正 SQL を組ませない（fail-fast）。
pub(crate) fn assert_ident(s: &str) {
    assert!(
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
        "invalid sql identifier: {s}"
    );
}

/// キー値列をクエリへ順にバインドする（`sqlx` の各クエリ型で共用）。
macro_rules! bind_key {
    ($query:expr, $key:expr) => {{
        let mut q = $query;
        for v in $key.values() {
            q = match *v {
                $crate::spec::KeyValue::Uuid(u) => q.bind(u),
                $crate::spec::KeyValue::Text(t) => q.bind(t),
                $crate::spec::KeyValue::BigInt(i) => q.bind(i),
                $crate::spec::KeyValue::OptText(t) => q.bind(t),
            };
        }
        q
    }};
}
pub(crate) use bind_key;

#[cfg(test)]
mod tests {
    use super::*;

    const K2: &[&str] = &["tenant_id", "run_id"];

    #[test]
    fn key_predicate_and_placeholders() {
        let id = Uuid::nil();
        let values = [KeyValue::Text("t1"), KeyValue::Uuid(id)];
        let key = Key::new(K2, &values);
        assert_eq!(key.predicate(), "tenant_id = $1 AND run_id = $2");
        assert_eq!(key.column_list(), "tenant_id, run_id");
        assert_eq!(key.placeholders(), "$1, $2");
        assert_eq!(key.len(), 2);
    }

    #[test]
    #[should_panic(expected = "invalid sql identifier")]
    fn rejects_bad_identifier() {
        assert_ident("run_id; DROP TABLE x");
    }
}

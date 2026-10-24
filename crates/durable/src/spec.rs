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
    /// イベント seq の採番カウンタ列（bigint・追記のたびに +1）。イベントログを持たない
    /// ドメイン（workflow の step 等）は `None`。
    ///
    /// **`max(seq) + 1` を数えてはいけない**。READ COMMITTED では行ロックを取っても、
    /// 数える側のスナップショットは古いままなので、並行追記が同じ seq を得て主キー違反になる
    /// （実測: 子エージェントのイベント中継と親のトークン列が衝突して run ごと落ちた）。
    /// ロックする行そのものを `UPDATE ... RETURNING` すれば EvalPlanQual で確定値が返る。
    pub event_seq_column: Option<&'static str>,
    /// `running` 以外で「リース失効時に takeover 可能」な非端末ステータス（例: chat の
    /// `waiting_approval`）。プロセスが承認待ち中にクラッシュしても、リース失効後に別ワーカーが
    /// checkpoint から resume できるようにする（#351）。これを持たないと承認待ちで死んだ run が
    /// 恒久的に孤児化する。takeover 時は `running_status` へ戻す。持たないドメインは `&[]`。
    pub resumable_statuses: &'static [&'static str],
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
    /// 追記時に **run 行から写す**列（例: chat の `tenant_id`）。キーに含まれない親の属性を
    /// イベント行へ持たせるためのもの（#815）。値は採番と同じ `UPDATE ... RETURNING` で run 行から
    /// 取るので、呼び出し側の申告ではなく親の正本が入る（親子の食い違いを作らない）。
    /// キー列と重複してはならない。写す列が無いドメインは `&[]`。
    pub copied_run_columns: &'static [&'static str],
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
        if let Some(c) = self.event_seq_column {
            assert_ident(c);
        }
        assert_ident(self.queued_status);
        assert_ident(self.running_status);
        for s in self.resumable_statuses {
            assert_ident(s);
        }
    }
}

impl EventTableSpec {
    pub(crate) fn validate(&self) {
        assert_ident(self.table);
        assert_ident(self.seq_column);
        assert_ident(self.kind_column);
        assert_ident(self.payload_column);
        for c in self.copied_run_columns {
            assert_ident(c);
        }
    }

    /// 写し列を足したイベント INSERT の列リストと、`bumped` CTE から取る値リスト・RETURNING 句。
    ///
    /// 戻り値は `(INSERT 列の追加分, SELECT 値の追加分, RETURNING の追加分)`。いずれも先頭に
    /// `, ` が付く（写し列が無ければ空文字列）。キー列との重複は同じ列を 2 度 INSERT する
    /// 不正 SQL になるため、プログラミングエラーとして落とす。
    pub(crate) fn copied_column_sql(&self, key: &Key<'_>) -> (String, String, String) {
        for c in self.copied_run_columns {
            assert!(
                !key.columns.contains(c),
                "copied_run_columns はキー列と重複できない: {c}"
            );
        }
        let list = |prefix: &str| -> String {
            let mut out = String::new();
            for c in self.copied_run_columns {
                out.push_str(", ");
                out.push_str(prefix);
                out.push_str(c);
            }
            out
        };
        (list(""), list("bumped."), list(""))
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
    fn copied_columns_come_from_bumped_run_row() {
        let ev = EventTableSpec {
            table: "evt",
            seq_column: "seq",
            kind_column: "type",
            payload_column: "payload",
            copied_run_columns: &["tenant_id"],
        };
        ev.validate();
        let id = Uuid::nil();
        let values = [KeyValue::Uuid(id)];
        let key = Key::new(&["run_id"], &values);
        let (cols, vals, ret) = ev.copied_column_sql(&key);
        assert_eq!(cols, ", tenant_id");
        assert_eq!(vals, ", bumped.tenant_id");
        assert_eq!(ret, ", tenant_id");

        let none = EventTableSpec {
            copied_run_columns: &[],
            ..ev
        };
        assert_eq!(
            none.copied_column_sql(&key),
            (String::new(), String::new(), String::new())
        );
    }

    #[test]
    #[should_panic(expected = "キー列と重複できない")]
    fn copied_column_must_not_overlap_key() {
        let ev = EventTableSpec {
            table: "evt",
            seq_column: "seq",
            kind_column: "type",
            payload_column: "payload",
            copied_run_columns: &["tenant_id"],
        };
        let values = [KeyValue::Text("t1"), KeyValue::Uuid(Uuid::nil())];
        let key = Key::new(K2, &values);
        ev.copied_column_sql(&key);
    }

    #[test]
    #[should_panic(expected = "invalid sql identifier")]
    fn rejects_bad_identifier() {
        assert_ident("run_id; DROP TABLE x");
    }
}

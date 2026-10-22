//! SQL 断片の組み立て器とバインド値（読取 SQL の共通部品・PIT-21）。
//!
//! **値は必ずバインド**する。SQL テキストへ埋め込むのは固定の表名・固定エイリアス・
//! 内部 ID 由来の JSONB キー（`"f{id}"`・埋め込み直前に再検証）だけ。
//! `field_id` も整数としてバインドする（data-platform §6.4: 埋め込む識別子すら消える）。

use sqlx::postgres::PgArguments;
use sqlx::Postgres;
use uuid::Uuid;

/// SQL バインド値（実行チョークポイントが順番どおり bind する）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Bind {
    Text(String),
    TextArray(Vec<String>),
    /// v1 の数値（`::numeric` へキャストして比較する）。
    Number(f64),
    UuidArray(Vec<Uuid>),
    I16(i16),
    I32(i32),
    I64(i64),
    I64Array(Vec<i64>),
    /// 10 進表記（SQL 側で `::numeric` にキャストする・二進誤差を持ち込まない）。
    Numeric(String),
    NumericArray(Vec<String>),
}

/// 1 文のバインド列（SQL テキストは組み立て関数が返す）。
#[derive(Debug, Clone, Default)]
pub(crate) struct SqlBuf {
    pub binds: Vec<Bind>,
}

impl SqlBuf {
    /// `$1 = tenant_int`, `$2 = table_int` を先に積んだ索引読取用の文（パーティション枝刈りの鍵）。
    pub(crate) fn scoped(tenant_int: i32, table_int: i64) -> Self {
        SqlBuf {
            binds: vec![Bind::I32(tenant_int), Bind::I64(table_int)],
        }
    }

    /// 値を積み、対応するプレースホルダ（`$n`）を返す。
    pub(crate) fn push(&mut self, b: Bind) -> String {
        self.binds.push(b);
        format!("${}", self.binds.len())
    }

    /// 型注釈付きのプレースホルダ（`$n::type`）。
    pub(crate) fn typed(&mut self, b: Bind) -> String {
        let ty = match &b {
            Bind::Text(_) => "text",
            Bind::TextArray(_) => "text[]",
            Bind::Number(_) => "float8",
            Bind::UuidArray(_) => "uuid[]",
            Bind::I16(_) => "int2",
            Bind::I32(_) => "int4",
            Bind::I64(_) => "int8",
            Bind::I64Array(_) => "int8[]",
            Bind::Numeric(_) => "numeric",
            Bind::NumericArray(_) => "numeric[]",
        };
        let ph = self.push(b);
        format!("{ph}::{ty}")
    }
}

/// `sqlx` のクエリ各種へバインド列を流し込む（型ごとの分岐をここ 1 箇所に閉じる）。
macro_rules! bind_into {
    ($q:expr, $b:expr) => {
        match $b {
            Bind::Text(v) => $q.bind(v),
            Bind::TextArray(v) => $q.bind(v),
            Bind::Number(v) => $q.bind(v),
            Bind::UuidArray(v) => $q.bind(v),
            Bind::I16(v) => $q.bind(v),
            Bind::I32(v) => $q.bind(v),
            Bind::I64(v) => $q.bind(v),
            Bind::I64Array(v) => $q.bind(v),
            Bind::Numeric(v) => $q.bind(v),
            Bind::NumericArray(v) => $q.bind(v),
        }
    };
}

pub(crate) fn bind_query<'q>(
    mut q: sqlx::query::Query<'q, Postgres, PgArguments>,
    binds: &[Bind],
) -> sqlx::query::Query<'q, Postgres, PgArguments> {
    for b in binds {
        q = bind_into!(q, b.clone());
    }
    q
}

pub(crate) fn bind_query_as<'q, O>(
    mut q: sqlx::query::QueryAs<'q, Postgres, O, PgArguments>,
    binds: &[Bind],
) -> sqlx::query::QueryAs<'q, Postgres, O, PgArguments> {
    for b in binds {
        q = bind_into!(q, b.clone());
    }
    q
}

pub(crate) fn bind_scalar<'q, O>(
    mut q: sqlx::query::QueryScalar<'q, Postgres, O, PgArguments>,
    binds: &[Bind],
) -> sqlx::query::QueryScalar<'q, Postgres, O, PgArguments> {
    for b in binds {
        q = bind_into!(q, b.clone());
    }
    q
}

/// LIKE パターンのメタ文字（`\` `%` `_`）をエスケープする（部分一致の値はリテラルとして扱う）。
pub(crate) fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_reserves_two_placeholders() {
        let mut s = SqlBuf::scoped(1, 2);
        assert_eq!(s.push(Bind::Text("x".into())), "$3");
        assert_eq!(s.typed(Bind::I16(5)), "$4::int2");
        assert_eq!(s.binds.len(), 4);
    }

    #[test]
    fn like_escape_quotes_meta() {
        assert_eq!(like_escape(r"50%_a\b"), r"50\%\_a\\b");
        assert_eq!(like_escape("普通"), "普通");
    }
}

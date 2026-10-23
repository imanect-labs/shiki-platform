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
    Bytes(Vec<u8>),
    /// 形状を変えないための真偽スイッチ（`HasRole` の畳み込み結果・値の長さによる分岐など。
    /// `plan_cache_mode = force_custom_plan` の下では計画時に定数として畳み込まれる・data-platform §16.3）。
    Bool(bool),
    /// 値なしを `NULL` として渡す text（カーソル位置の値なしなど・テキストを分岐させない）。
    OptText(Option<String>),
    /// 値なしを `NULL` として渡す numeric（10 進表記）。
    OptNumeric(Option<String>),
}

/// 1 文のバインド列（SQL テキストは組み立て関数が返す）。
#[derive(Debug, Clone, Default)]
pub(crate) struct SqlBuf {
    pub binds: Vec<Bind>,
    /// いま降ろしている表の `table_int` のプレースホルダ（`None` は `$2`＝文の主表）。リンクを辿る
    /// 半結合（参照先テーブルの条件と行述語）を降ろす間だけ、参照先の `table_int` のバインドへ切り替える。
    pub table: Option<String>,
    /// 文の中で採番した別名の数（入れ子の半結合の別名 `lk{n}` を一意にする・決定的）。
    pub aliases: u32,
    /// バインドだけを積み、SQL テキストを組まない（プランキャッシュの命中時・data-platform §16.3）。
    /// 同一形状の降下は同じ順で同じ種類のバインドを積むので、テキストはキャッシュから引き、
    /// 値だけをこの走査で集める。組み立て関数は [`sqlf!`] でテキストを作る（この間は空文字列を返す）。
    pub skip_text: bool,
}

/// SQL 断片を組む（[`SqlBuf::skip_text`] の間は組まずに空文字列）。**引数に副作用（バインドを積む呼び出し）を
/// 書かない**こと（テキストを組まないときは評価されない）。バインドは先に変数へ積んでから渡す。
macro_rules! sqlf {
    ($sql:expr, $($arg:tt)*) => {
        if $sql.skip_text {
            String::new()
        } else {
            format!($($arg)*)
        }
    };
}
pub(crate) use sqlf;

impl SqlBuf {
    /// `$1 = tenant_int`, `$2 = table_int` を先に積んだ索引読取用の文（パーティション枝刈りの鍵）。
    pub(crate) fn scoped(tenant_int: i32, table_int: i64) -> Self {
        SqlBuf {
            binds: vec![Bind::I32(tenant_int), Bind::I64(table_int)],
            table: None,
            aliases: 0,
            skip_text: false,
        }
    }

    /// バインドだけを集める走査（テキストはプランキャッシュから引く）。
    pub(crate) fn scoped_binds_only(tenant_int: i32, table_int: i64) -> Self {
        SqlBuf {
            skip_text: true,
            ..Self::scoped(tenant_int, table_int)
        }
    }

    /// いま降ろしている表の `table_int` のプレースホルダ（既定は `$2`）。
    pub(crate) fn t(&self) -> String {
        self.table.clone().unwrap_or_else(|| "$2".to_string())
    }

    /// `table_int` を `ph` に切り替えて `f` を降ろす（終わったら元に戻す）。
    pub(crate) fn within<R>(&mut self, ph: &str, f: impl FnOnce(&mut SqlBuf) -> R) -> R {
        let prev = self.table.replace(ph.to_string());
        let out = f(self);
        self.table = prev;
        out
    }

    /// 別名の番号を採る（文の中で一意・降下の順に決まるので同一形状なら同じ番号）。
    pub(crate) fn alias(&mut self) -> u32 {
        self.aliases += 1;
        self.aliases
    }

    /// 真偽スイッチ（`$n::bool`）。
    pub(crate) fn flag(&mut self, b: bool) -> String {
        self.typed(Bind::Bool(b))
    }

    /// 値を積み、対応するプレースホルダ（`$n`）を返す。
    pub(crate) fn push(&mut self, b: Bind) -> String {
        self.binds.push(b);
        if self.skip_text {
            return String::new();
        }
        format!("${}", self.binds.len())
    }

    /// 型注釈付きのプレースホルダ（`$n::type`）。
    pub(crate) fn typed(&mut self, b: Bind) -> String {
        let ty = match &b {
            Bind::Text(_) | Bind::OptText(_) => "text",
            Bind::TextArray(_) => "text[]",
            Bind::Number(_) => "float8",
            Bind::UuidArray(_) => "uuid[]",
            Bind::I16(_) => "int2",
            Bind::I32(_) => "int4",
            Bind::I64(_) => "int8",
            Bind::I64Array(_) => "int8[]",
            Bind::Numeric(_) | Bind::OptNumeric(_) => "numeric",
            Bind::NumericArray(_) => "numeric[]",
            Bind::Bytes(_) => "bytea",
            Bind::Bool(_) => "bool",
        };
        let ph = self.push(b);
        sqlf!(self, "{ph}::{ty}")
    }
}

impl Bind {
    /// バインドの種類（値を含まない・プランキャッシュの命中時に「同じ形の降下」かを照合する）。
    pub(crate) fn kind_tag(&self) -> u8 {
        match self {
            Bind::Text(_) => 1,
            Bind::TextArray(_) => 2,
            Bind::Number(_) => 3,
            Bind::UuidArray(_) => 4,
            Bind::I16(_) => 5,
            Bind::I32(_) => 6,
            Bind::I64(_) => 7,
            Bind::I64Array(_) => 8,
            Bind::Numeric(_) => 9,
            Bind::NumericArray(_) => 10,
            Bind::Bytes(_) => 11,
            Bind::Bool(_) => 12,
            Bind::OptText(_) => 13,
            Bind::OptNumeric(_) => 14,
        }
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
            Bind::Bytes(v) => $q.bind(v),
            Bind::Bool(v) => $q.bind(v),
            Bind::OptText(v) => $q.bind(v),
            Bind::OptNumeric(v) => $q.bind(v),
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

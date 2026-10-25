//! クエリの入口での拒否理由（422・構造化・data-platform §16.1 のエラー契約・Task 15.10）。
//!
//! SQL 前段面（[`crate::sql`]）と IR 直投入の資源上限が返す。LLM が自己修正できるよう、拒否の種類・
//! 説明・位置・上限・候補を構造化して返す（`EmitUiTool` が検証エラー全件を返す流儀と同じ）。
//!
//! **存在オラクルを作らない**: 存在しない列・見えない列（フィールドマスク）・存在しないテーブル・見えない
//! テーブル（viewer でない）は**同一のコード・同一のメッセージ**で返す（[`QueryRejection::unknown_name`]）。
//! 候補（`expected`）は閲覧者に見える名前だけから作るので、伏せた名前の有無で応答が変わらない。
//! 統計値・推定値・戦略名・内部キーは載せない（PIT-75）。

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// 拒否の種類（LLM の自己修正と失敗分類に使う語彙）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RejectCode {
    /// SQL として読めない（構文エラー）。
    SyntaxError,
    /// 受け付ける部分集合の外（DML / DDL・CTE・ウィンドウ関数・副問い合わせ・`UNION`・未許可の関数・
    /// `pg_*`・型キャスト・`SELECT *`・`OFFSET` など）。
    Unsupported,
    /// 列またはテーブルが見つからない（存在しない・見えない を区別しない）。
    UnknownName,
    /// `JOIN` が宣言済みリンク列の等値 1 個になっていない・リンクの向きや参照先が合わない。
    InvalidJoin,
    /// 型・演算子・集計の組み合わせが合わない（数値列に文字列・GROUP BY に無い列の射影など）。
    InvalidQuery,
    /// 入口の資源上限を超えた（`limit` にどの上限か）。
    LimitExceeded,
}

/// 超えた上限（どの上限か・その値）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RejectLimit {
    /// 上限の名前（`max_sql_bytes` / `max_ast_depth` / `max_ast_nodes` / `max_literals` /
    /// `max_compile_work` / `max_compile_ms` / `max_join_depth`）。
    pub name: String,
    /// 上限値。
    pub max: u64,
}

/// SQL テキスト上の位置（1 始まり）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SqlLocation {
    pub line: u64,
    pub column: u64,
}

/// 拒否理由（422 の本文）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct QueryRejection {
    pub code: RejectCode,
    /// 人と LLM が読む説明（日本語）。
    pub message: String,
    /// 書き直しの手がかり（受け付ける書き方の例など）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// SQL テキスト上の位置（分かる場合）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<SqlLocation>,
    /// 超えた上限（`limit_exceeded` のとき）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<RejectLimit>)]
    pub limit: Option<Box<RejectLimit>>,
    /// 使える名前・書き方の候補（閲覧者に見えるものだけ）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expected: Vec<String>,
}

impl QueryRejection {
    pub(crate) fn new(code: RejectCode, message: impl Into<String>) -> Self {
        QueryRejection {
            code,
            message: message.into(),
            hint: None,
            location: None,
            limit: None,
            expected: Vec::new(),
        }
    }

    #[must_use]
    pub(crate) fn hint(mut self, h: impl Into<String>) -> Self {
        self.hint = Some(h.into());
        self
    }

    #[must_use]
    pub(crate) fn at(mut self, loc: Option<SqlLocation>) -> Self {
        self.location = loc;
        self
    }

    #[must_use]
    pub(crate) fn expected(mut self, e: Vec<String>) -> Self {
        self.expected = e;
        self
    }

    /// 上限超過（どの上限に当たったかを含める）。
    pub(crate) fn limit(name: &str, max: u64) -> Self {
        let mut r = QueryRejection::new(
            RejectCode::LimitExceeded,
            format!("クエリが入口の上限 {name}（{max}）を超えています"),
        )
        .hint("条件・IN の要素・入れ子を減らしてください");
        r.limit = Some(Box::new(RejectLimit {
            name: name.to_string(),
            max,
        }));
        r
    }

    /// 名前が見つからない（**存在しない・見えないを区別しない**同一の応答）。
    ///
    /// `kind` は利用者が書いた位置（列かテーブルか）で決まり、名前の実在には依らない。`expected` には
    /// 閲覧者に見える候補だけを渡すこと。
    pub(crate) fn unknown_name(kind: NameKind, name: &str, expected: Vec<String>) -> Self {
        let what = match kind {
            NameKind::Column => "列",
            NameKind::Table => "テーブル",
        };
        QueryRejection::new(
            RejectCode::UnknownName,
            format!("{what} '{name}' は見つからないか、参照できません"),
        )
        .expected(expected)
    }
}

/// 名前の種類（利用者が書いた位置で決まる）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NameKind {
    Column,
    Table,
}

impl std::fmt::Display for QueryRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

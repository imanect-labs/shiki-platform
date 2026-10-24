//! 構造化エラー本文の型（OpenAPI の単一ソース・検証 C-P2-8）。
//!
//! [`crate::error::ApiError`] の構造化本文はこの型を直列化して作る（手書きの JSON と OpenAPI の食い違いを
//! 作らない）。web は OpenAPI から生成した型でこれを読む。

use serde::Serialize;
use utoipa::ToSchema;

/// 構造化 400（入力の誤り）。`message` は利用者に見せてよい日本語で、値・閲覧者が読めない列名・内部 ID・
/// SQL 断片を含まない。`field` は対象の列の参照名（書き手が名指しした列か、読める列だけ）。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BadRequestBody {
    /// 常に 400。
    pub status: u16,
    pub title: String,
    pub code: data::InvalidCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

impl BadRequestBody {
    pub fn new(code: data::InvalidCode, message: &str, field: Option<&str>) -> Self {
        BadRequestBody {
            status: 400,
            title: "Bad Request".into(),
            code,
            message: message.to_string(),
            field: field.map(str::to_string),
        }
    }
}

/// 409 の構造化コード（data）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConflictCode {
    /// 楽観ロック（rev）の不一致。`current_rev` に今の rev。
    RevConflict,
    /// unique 宣言の違反（値・相手の行は返さない）。
    UniqueViolation,
    /// 材料化した派生列が参照先の変更を受けて再計算中（`field`）。
    DerivedRecomputing,
    /// 列の索引を準備中（`field`・`progress`）。
    IndexPreparing,
    /// 索引列数の上限を超える（`field`・`indexed_fields`・`max_indexed_fields`）。
    IndexApprovalRequired,
    /// 同じ名前のテーブルが既にある。
    NameTaken,
    /// その他の競合（取込 ID の再利用・並行のスキーマ切替など）。
    Conflict,
}

/// 構造化 409（data の競合）。コードごとに使う項目だけが入る。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ConflictBody {
    /// 常に 409。
    pub status: u16,
    pub title: String,
    pub code: ConflictCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_rev: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// 索引の準備の進捗（0〜1・不明なら null）。`index_preparing` のときだけ項目を持つ。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<Option<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexed_fields: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_indexed_fields: Option<usize>,
}

impl ConflictBody {
    pub fn new(code: ConflictCode) -> Self {
        ConflictBody {
            status: 409,
            title: "Conflict".into(),
            code,
            current_rev: None,
            field: None,
            progress: None,
            indexed_fields: None,
            max_indexed_fields: None,
        }
    }
}

/// 構造化 413（本文が上限を超えた・一括取込の `import_too_large`）。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PayloadTooLargeBody {
    /// 常に 413。
    pub status: u16,
    pub title: String,
    /// `import_too_large`。
    pub code: String,
    pub max_bytes: usize,
}

/// 再試行できる 503（statement timeout の `read_timeout`）。`Retry-After` ヘッダを伴う。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RetryLaterBody {
    /// 常に 503。
    pub status: u16,
    pub title: String,
    /// `read_timeout`。
    pub code: String,
    pub message: String,
}

/// 一括取込の入力の形の誤り（400・`import_invalid`）。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ImportInvalidBody {
    /// 常に 400。
    pub status: u16,
    pub title: String,
    /// `import_invalid`。
    pub code: String,
    pub message: String,
}

/// 型を JSON 本文へ（直列化に失敗しない型だけを渡す）。
pub(crate) fn to_body<T: Serialize>(v: &T) -> serde_json::Value {
    serde_json::to_value(v).unwrap_or_else(|_| serde_json::json!({}))
}

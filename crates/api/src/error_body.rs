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

/// 409 の構造化コード（data・CSV の新規保存）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConflictCode {
    /// 楽観ロック（rev）の不一致。`current_rev` に今の rev。
    RevConflict,
    /// unique 宣言・一対一 / 一対多のリンクの参照先の一意の違反（値・相手の行は返さない）。
    /// 呼び出し側が読める列なら `field`（参照名）と、列の表示名つきの `message` を持つ。
    UniqueViolation,
    /// 材料化した派生列が参照先の変更を受けて再計算中（`field`）。
    DerivedRecomputing,
    /// 列の索引を準備中（`field`・`progress`）。
    IndexPreparing,
    /// 索引列数の上限を超える（`field`・`indexed_fields`・`max_indexed_fields`）。
    IndexApprovalRequired,
    /// 同じ名前のテーブルが既にある。CSV の新規保存（`POST /tabular/save`）では、保存先に同じ名前のファイルが
    /// ある（番号を付けるときは ` (50)` まで埋まっている・BUG-150）。
    NameTaken,
    /// アプリのインストール・アンインストールで、同じテナントのほかの組織にこのアプリの有効なインストールがある
    /// （`message` に理由と直し方・BUG-212）。
    InstalledInOtherOrg,
    /// アプリ・スキルのインストールで、その版の公開が取り下げられている（`message` に理由・GAP-69b）。
    Yanked,
    /// その他の競合（取込 ID の再利用・並行のスキーマ切替など）。
    Conflict,
}

/// 構造化 409（data の競合・CSV の新規保存の名前の衝突）。コードごとに使う項目だけが入る。
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
    /// 利用者に見せてよい文言（`unique_violation`〔読める列なら表示名つき・値は含まない〕と `installed_in_other_org`・
    /// `yanked`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
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
            message: None,
            progress: None,
            indexed_fields: None,
            max_indexed_fields: None,
        }
    }
}

/// アーティファクト（ワークフロー・アプリ・スキルなど）の 409 の構造化コード（GAP-41）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactConflictCode {
    /// 比べた版（`expected_version`）の後に、ほかの人が保存した。`current_version` に今の最新の版。
    VersionConflict,
    /// 同じ種類・同じ名前のものがすでにある（名前はテナントで一意なので、呼んだ人に見えないものを含む）。
    NameTaken,
}

/// アーティファクトの構造化 409（版の競合・名前の重複・GAP-41）。`message` は利用者に見せてよい日本語で、対象の id や
/// 呼んだ人に見えないものの名前を含まない。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ArtifactConflictBody {
    /// 常に 409。
    pub status: u16,
    pub title: String,
    pub code: ArtifactConflictCode,
    pub message: String,
    /// 今の最新の版（`version_conflict` のときだけ。呼んだ人は editor を確かめてある）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_version: Option<i64>,
}

impl ArtifactConflictBody {
    pub fn new(reason: artifact::ConflictReason) -> Self {
        use artifact::ConflictReason as R;
        let (code, message, current_version) = match reason {
            R::VersionChanged { kind, current } => (
                ArtifactConflictCode::VersionConflict,
                format!(
                    "ほかの人が先にこの{}を保存しました。あなたの変更は保存されていません。",
                    kind.noun()
                ),
                Some(current),
            ),
            R::NameTaken { kind } => (
                ArtifactConflictCode::NameTaken,
                format!(
                    "同じ名前の{}がすでにあります（あなたが見られないものを含みます）。別の名前にしてください。",
                    kind.noun()
                ),
                None,
            ),
        };
        ArtifactConflictBody {
            status: 409,
            title: "Conflict".into(),
            code,
            message,
            current_version,
        }
    }
}

/// ゴミ箱の完全削除で何も消さなかった理由（`DELETE /trash/{id}` の 409 の `code`・GAP-108b）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PurgeRefusalCode {
    /// ゴミ箱に無い（ほかの人が元に戻した・削除していない）。
    NotInTrash,
    /// 削除したフォルダの中にある（ゴミ箱に出ているのは親のフォルダ）。
    InsideDeletedFolder,
    /// フォルダの中に、削除した後に作られた（削除されていない）ノードがある。
    LiveDescendants,
}

/// ゴミ箱の完全削除の構造化 409。`title` は利用者に見せてよい日本語。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PurgeRefusalBody {
    /// 常に 409。
    pub status: u16,
    pub title: String,
    pub code: PurgeRefusalCode,
}

impl PurgeRefusalBody {
    pub fn new(code: PurgeRefusalCode) -> Self {
        let title = match code {
            PurgeRefusalCode::NotInTrash => "ゴミ箱にありません",
            PurgeRefusalCode::InsideDeletedFolder => {
                "フォルダごとゴミ箱にあります。ゴミ箱に出ているフォルダを完全に削除してください"
            }
            PurgeRefusalCode::LiveDescendants => {
                "このフォルダの中に、削除した後に作られたファイルがあります。フォルダを元に戻して中を確かめてください"
            }
        };
        PurgeRefusalBody {
            status: 409,
            title: title.into(),
            code,
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

/// 403 の構造化コード。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ForbiddenCode {
    /// 指定した列が存在しないか、閲覧者に読めない（両者を区別しない・列名を含めない）。
    FieldUnavailable,
}

/// 構造化 403。`field_unavailable` は存在しない列と読めない列で**同一の本文**（列の存在オラクルを作らない・
/// data-platform §16.1）。テーブル・操作の権限が無い 403 は従来どおり `{status, title}` だけ。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ForbiddenBody {
    /// 常に 403。
    pub status: u16,
    pub title: String,
    pub code: ForbiddenCode,
    pub message: String,
}

impl ForbiddenBody {
    pub fn field_unavailable() -> Self {
        ForbiddenBody {
            status: 403,
            title: "Forbidden".into(),
            code: ForbiddenCode::FieldUnavailable,
            message: data::FIELD_UNAVAILABLE_MESSAGE.into(),
        }
    }
}

/// 要求の形の誤り（本文の JSON・パス・クエリ文字列の読取に失敗した・ルートやメソッドが無い）の構造化本文。
///
/// axum の抽出器の拒否（英語の平文）とルータの 404 / 405 を、サーバ全体で同じ形に寄せる
/// （`crate::server::rejection`・検証 N14）。400 の `code` は `invalid_input`、それ以外は
/// `unsupported_media_type` / `payload_too_large` / `not_found` / `method_not_allowed` / `bad_request`。
/// `message` は利用者向けの日本語で、内部の型名・ライブラリの英語の文言を含まない。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RequestErrorBody {
    pub status: u16,
    pub title: String,
    pub code: String,
    pub message: String,
}

/// 構造化 422（クエリの入口の拒否・`data::QueryRejection` に `status` / `title` を足した本文・検証 C-P2-8）。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct QueryRejectionBody {
    /// 常に 422。
    pub status: u16,
    pub title: String,
    #[serde(flatten)]
    pub rejection: data::QueryRejection,
}

/// 実行の入力の誤り 1 件（項目のキーと理由・BUG-109）。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct InputIssueBody {
    /// 入力の項目のキー（入れ子は `addr.zip`・リストの要素は `tags[2]`）。入力全体の誤りは空。
    pub key: String,
    /// 理由（利用者に見せてよい日本語・例: `1 以上にしてください`）。
    pub message: String,
}

/// 実行の入力が入力項目の形に合わない（構造化 400・`invalid_input`・BUG-109）。
///
/// 実行のダイアログは `errors` の `key` を項目の見出しに直して欄の下に出す（本文を捨てて「API 400」とだけ
/// 出さない）。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct InvalidRunInputBody {
    /// 常に 400。
    pub status: u16,
    pub title: String,
    /// `invalid_input`。
    pub code: data::InvalidCode,
    /// 全体の文言（例: `入力した値に誤りがあります。`）。
    pub message: String,
    pub errors: Vec<InputIssueBody>,
}

impl InvalidRunInputBody {
    pub fn new(issues: &[workflow_engine::ir::input_schema::InputIssue]) -> Self {
        InvalidRunInputBody {
            status: 400,
            title: "Bad Request".into(),
            code: data::InvalidCode::InvalidInput,
            message: "入力した値に誤りがあります。".into(),
            errors: issues
                .iter()
                .map(|i| InputIssueBody {
                    key: i.key.clone(),
                    message: i.message.clone(),
                })
                .collect(),
        }
    }
}

/// 型を JSON 本文へ（直列化に失敗しない型だけを渡す）。
pub(crate) fn to_body<T: Serialize>(v: &T) -> serde_json::Value {
    serde_json::to_value(v).unwrap_or_else(|_| serde_json::json!({}))
}

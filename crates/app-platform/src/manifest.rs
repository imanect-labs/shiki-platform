//! ミニアプリ・マニフェスト（Task 9.1）。
//!
//! コードベース・ミニアプリ（B）の宣言。要求スコープ・所有テーブル・ワークフロー参照・
//! 予算・フロントバンドル・（B2 なら）サーバ関数・信頼ティアを持つ。マニフェストは
//! `artifact(kind=mini_app_code)` の body として保存され、A（宣言的）と同じ version＋ReBAC＋
//! 監査枠に乗る。

use data::TableSchema;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// 信頼ティア（Task 9.13）。first-party は署名必須・自動信頼、in-house は管理者同意、
/// marketplace は将来（審査付き第三者公開）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrustTier {
    FirstParty,
    InHouse,
    /// 予約（Phase 9 安定後の将来トラック・現時点では拒否）。
    Marketplace,
}

impl TrustTier {
    /// registry_entry.trust_tier の文字列表現（単一定義）。
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            TrustTier::FirstParty => "first_party",
            TrustTier::InHouse => "in_house",
            TrustTier::Marketplace => "marketplace",
        }
    }
}

/// 所有テーブル定義（インストール時に自動プロビジョンされる）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ManifestTable {
    /// テーブル名（テナント内一意・プロビジョン時に data_table.name になる）。
    pub name: String,
    pub schema: TableSchema,
}

/// AI/能力の予算ガードレール（アプリ登録時宣言・管理者キャップとの min が効く）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
pub struct Budget {
    /// 使用可能モデルの allowlist。**空 = テナントカタログに委ねる**（カタログは管理者管理の
    /// 閉集合で最終判定）。特定モデルに絞りたいアプリは明示的に列挙する。非空のとき
    /// ゲートウェイはモデル未指定の呼び出しも拒否する（既定モデルへのすり抜け防止）。
    #[serde(default)]
    pub models: Vec<String>,
    /// 日次コスト上限（マイクロ USD・None＝管理者キャップのみ）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_usd_micros: Option<i64>,
    /// 1 回の呼び出しの最大トークン。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<i64>,
}

/// フロントバンドル参照（B1・ObjectStore に格納）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FrontendBundle {
    /// ObjectStore のキー（バンドル tar/zip の content address）。
    pub bundle_key: String,
    /// バンドルの sha256（配信時に検証）。
    pub sha256: String,
}

/// B2 サーバ関数の宣言（サンドボックスで実行）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
pub struct ServerSpec {
    /// サーバコードバンドル（単一 JS・content address sha256 hex・BundleStore 管理）。
    /// インストール時に installation へピンされる（Task 9.12）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_sha256: Option<String>,
    /// エントリポイント（バンドル内のモジュールパス・将来のマルチモジュール用・現状未使用）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    /// 公開する関数名（event/cron/ユーザー起動の対象）。
    #[serde(default)]
    pub functions: Vec<String>,
    /// egress allowlist（完全一致 or `*.suffix`・default-deny）。
    #[serde(default)]
    pub egress_allowlist: Vec<String>,
    /// 購読する events（`<能力>.<操作>` またはアプリ内イベント名）。
    #[serde(default)]
    pub events: Vec<String>,
    /// cron スケジュール（式・関数名）。
    #[serde(default)]
    pub cron: Vec<CronEntry>,
}

/// cron エントリ。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CronEntry {
    pub function: String,
    /// cron 式（5 フィールド）。
    pub expr: String,
}

/// ミニアプリ・マニフェスト本体。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MiniAppManifest {
    pub name: String,
    /// semver（`1.2.3`）。publish の一意キー。
    pub version: String,
    #[serde(default)]
    pub description: String,
    /// 要求する能力スコープ（`<能力>.<操作>`・CapabilityScope へ照合）。
    #[serde(default)]
    pub requested_scopes: Vec<String>,
    /// agent.invoke で宣言するツール（ToolName へ照合）。
    #[serde(default)]
    pub tools: Vec<String>,
    /// 所有テーブル（インストール時プロビジョン）。
    #[serde(default)]
    pub tables: Vec<ManifestTable>,
    /// 参照ワークフロー（artifact_id・保存時に存在検証しない＝インストール時に解決）。
    #[serde(default)]
    pub workflows: Vec<Uuid>,
    #[serde(default)]
    pub budget: Budget,
    /// フロントバンドル（B1）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frontend: Option<FrontendBundle>,
    /// サーバ関数（B2・省略時は B1 のみ）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<ServerSpec>,
    pub trust_tier: TrustTier,
}

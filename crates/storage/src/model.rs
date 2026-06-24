//! ストレージのドメインモデル（ノード・アップロード結果の DTO）。

use authz::{FgaObject, Relation, Subject};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// ノード種別（フォルダ or ファイル）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Folder,
    File,
}

impl NodeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NodeKind::Folder => "folder",
            NodeKind::File => "file",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "folder" => Some(NodeKind::Folder),
            "file" => Some(NodeKind::File),
            _ => None,
        }
    }
}

/// ストレージノード（ファイル/フォルダのメタデータ）。
#[derive(Debug, Clone)]
pub struct Node {
    pub id: Uuid,
    pub org: String,
    pub tenant_id: String,
    pub kind: NodeKind,
    pub name: String,
    pub parent_id: Option<Uuid>,
    pub blob_sha256: Option<String>,
    pub size_bytes: Option<i64>,
    pub content_type: Option<String>,
    pub version: i64,
    pub deleted_at: Option<DateTime<Utc>>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `begin_upload`（declare）の結果＝アップロード用 presigned チケット。
///
/// クライアントは `upload_url` へバイトを直接 PUT し、`upload_id` で finalize する。
/// 重複排除は finalize 時（＝実バイトのアップロード＝所持証明の後）に行うため、
/// declare 段階では宣言ハッシュだけで他人の内容を取得できない（所持証明前の dedup を避ける）。
#[derive(Debug)]
pub struct UploadTicket {
    pub upload_id: Uuid,
    pub upload_url: String,
}

/// ダウンロード presigned URL（発行結果）。
#[derive(Debug)]
pub struct DownloadTicket {
    pub url: String,
    /// URL の有効秒数（クライアントが失効を判断するため）。
    pub expires_in_secs: u64,
}

/// 子一覧の 1 ページ（**権限フィルタ済み**・PIT-13）。
///
/// `items` は呼び出しユーザーが viewer 以上を持つノードのみ。`next_cursor` が
/// `Some` なら続きがあり、次回 `list_children` に渡すと続きから取得できる
/// （オーバーフェッチ＋keyset カーソル方式。末尾で空ページが 1 回返り得る）。
#[derive(Debug)]
pub struct ChildPage {
    pub items: Vec<Node>,
    pub next_cursor: Option<String>,
}

/// パンくず 1 要素（祖先ノードの最小情報）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crumb {
    pub id: Uuid,
    pub name: String,
    pub kind: NodeKind,
}

/// 共有先（subject）。Task 1.6 では user / role を対象とする
/// （group は OpenFGA 型未定義のため後続フェーズ）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ShareTarget {
    /// 個人ユーザー（`user:<id>`）。
    User { id: String },
    /// ロールメンバー全体（`role:<id>#member`）。配下ロールのメンバーも含む（上方向ロールアップ）。
    /// `role#member` は org 継承を含まないため、共有が org 全体へ広がらない（#72）。
    Role { id: String },
}

impl ShareTarget {
    /// OpenFGA タプル右辺の subject に変換する。ロールは `role:<id>#member` userset。
    pub fn subject(&self) -> Subject {
        match self {
            ShareTarget::User { id } => Subject::user(id),
            ShareTarget::Role { id } => Subject::userset(&FgaObject::role(id), Relation::Member),
        }
    }

    /// OpenFGA Read で得た subject 文字列を共有先へ戻す
    /// （`user:<id>` / `role:<id>#member`）。共有相手として解釈できない
    /// subject（owner の user 以外・parent の folder 等）は `None`。
    pub fn parse_subject(s: &str) -> Option<Self> {
        if let Some(id) = s.strip_prefix("user:") {
            return Some(ShareTarget::User { id: id.to_string() });
        }
        if let Some(rest) = s.strip_prefix("role:") {
            if let Some(id) = rest.strip_suffix("#member") {
                return Some(ShareTarget::Role { id: id.to_string() });
            }
        }
        None
    }
}

/// 共有で付与できる役割。owner/parent/member ではなく viewer/editor のみを許す
/// （閉じた共有語彙。design.md のストレージ ReBAC は viewer/editor）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShareRole {
    Viewer,
    Editor,
}

impl ShareRole {
    /// OpenFGA relation へ写す。
    pub fn relation(self) -> Relation {
        match self {
            ShareRole::Viewer => Relation::Viewer,
            ShareRole::Editor => Relation::Editor,
        }
    }

    /// relation を共有役割へ戻す（viewer/editor 以外は `None`）。
    pub fn from_relation(relation: Relation) -> Option<Self> {
        match relation {
            Relation::Viewer => Some(ShareRole::Viewer),
            Relation::Editor => Some(ShareRole::Editor),
            _ => None,
        }
    }
}

/// 共有相手 1 件（誰に・どの役割で共有したか）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShareEntry {
    pub target: ShareTarget,
    pub role: ShareRole,
}

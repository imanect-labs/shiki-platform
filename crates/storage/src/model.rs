//! ストレージのドメインモデル（ノード・アップロード結果の DTO）。

use authz::{Relation, Subject};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
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

/// ファイルの内容版 1 件（Task 1.7・履歴一覧/特定版取得で使う）。
///
/// `version` は `Node::version` と一致し、内容を持つ版（create / 内容更新 / 版復元）だけが
/// 履歴に並ぶ（rename/move 等のメタ版は欠番になる）。同一内容の版は `blob_sha256` を共有する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileVersion {
    /// テナント境界を上流（集約/キャッシュ）まで運ぶため day-1 から保持する（API 層で落とす）。
    pub tenant_id: String,
    pub version: i64,
    pub blob_sha256: String,
    pub size_bytes: i64,
    pub content_type: String,
    /// この版を作成した subject。
    pub author: String,
    pub created_at: DateTime<Utc>,
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

/// 子一覧の並び替えキー。keyset カーソルをこのキーに織り込み、サーバ側でソートする
/// （クライアント側の全件ソートは採らない＝全件取得の禁止・無限スクロール）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildSortKey {
    /// 名前（既定）。
    Name,
    /// 更新日時。
    Updated,
    /// サイズ（フォルダは 0 とみなす）。
    Size,
}

/// 子一覧の並び順（キー＋方向）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChildSort {
    pub key: ChildSortKey,
    /// `true` で降順。
    pub desc: bool,
}

impl Default for ChildSort {
    fn default() -> Self {
        Self {
            key: ChildSortKey::Name,
            desc: false,
        }
    }
}

/// パンくず 1 要素（祖先ノードの最小情報）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crumb {
    pub id: Uuid,
    pub name: String,
    pub kind: NodeKind,
}

/// 共有先（subject）。Task 1.6 では **user 共有のみ** を対象とする。
///
/// role / department / group 共有は OpenFGA のテナントスコープ（SAAS.1）と role provisioning
/// （SAAS.2）が前提になるため defer する（#76）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ShareTarget {
    /// 個人ユーザー（`user:<id>`）。
    User { id: String },
}

impl ShareTarget {
    /// OpenFGA タプル右辺の subject に変換する。
    pub fn subject(&self) -> Subject {
        match self {
            ShareTarget::User { id } => Subject::user(id),
        }
    }

    /// OpenFGA Read で得た subject 文字列を共有先へ戻す（`user:<id>`）。
    /// 共有相手として解釈できない subject（owner の user 以外・parent の folder 等）は `None`。
    pub fn parse_subject(s: &str) -> Option<Self> {
        s.strip_prefix("user:")
            .map(|id| ShareTarget::User { id: id.to_string() })
    }
}

/// 共有で付与できる役割。owner/parent/member ではなく viewer/editor のみを許す
/// （閉じた共有語彙。design.md のストレージ ReBAC は viewer/editor）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ShareEntry {
    pub target: ShareTarget,
    pub role: ShareRole,
}

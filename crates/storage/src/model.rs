//! ストレージのドメインモデル（ノード・アップロード結果の DTO）。

use chrono::{DateTime, Utc};
use serde::Serialize;
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

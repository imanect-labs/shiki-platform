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

/// `begin_upload`（declare）の結果。
///
/// 同一内容の blob が既存なら即座にノードが作られ（アップロード不要）、
/// 未存在なら presigned PUT URL が返り、クライアントが直接アップロードする。
#[derive(Debug)]
pub enum UploadOutcome {
    /// 重複排除によりアップロード不要。ノードは作成済み。
    Deduplicated(Node),
    /// アップロードが必要。クライアントは `upload_url` へ PUT 後 finalize する。
    NeedsUpload { upload_id: Uuid, upload_url: String },
}

/// ダウンロード presigned URL（発行結果）。
#[derive(Debug)]
pub struct DownloadTicket {
    pub url: String,
    /// URL の有効秒数（クライアントが失効を判断するため）。
    pub expires_in_secs: u64,
}

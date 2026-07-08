//! ワークスペース抽象（Task 5.4/5.8・Durable Workspace モデル）。
//!
//! 自律エージェントの**ワークスペース**は thread ごとの StorageService フォルダ（durable・版管理・監査・
//! 書込→再索引に自動で乗る）。file CRUD ツールはこのトレイト裏で StorageService を**直読み/直書き**する。
//! メタは強整合なので **read-after-write が成立**し、RAG の非同期索引とは経路が分離される（PIT-5 準拠）。
//!
//! 実装は shiki-server 側で `StorageService`（`write_file_at`/`resolve_child_file`/`read_file_internal`/
//! `list_children`/`soft_delete_file`）へ配線する（発話ユーザーの `AuthContext` で操作＝confused-deputy 回避）。
//! agent-core はストレージ実装に依存せず、テストではフェイクを差す。名前空間は**フラット**（サブディレクトリ
//! 非対応・アルファ）で、名前はワークスペース直下のファイル名を指す。

use async_trait::async_trait;
use authz::AuthContext;

use crate::tool::ToolError;

/// ワークスペース内の 1 エントリ（フラット・ファイルのみ）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceEntry {
    pub name: String,
    pub size: u64,
}

/// 書込結果（作成 or 新版）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceWrite {
    /// 保存先の storage node id。
    pub node_id: String,
    pub name: String,
    pub version: i64,
    /// 新規作成なら true、既存への上書き（新版）なら false。
    pub created: bool,
}

/// ワークスペース（thread ごとの Drive フォルダ）への CRUD の差し替え点。
///
/// 全操作は発話ユーザーの `ctx` 権限で実行する（昇格しない）。書込/削除は StorageService の
/// 書込イベント→再索引に自動で乗る（Task 5.8）。
#[async_trait]
pub trait WorkspaceStore: Send + Sync {
    /// ワークスペース直下のファイルを列挙する。
    async fn list(
        &self,
        ctx: &AuthContext,
        trace_id: Option<&str>,
    ) -> Result<Vec<WorkspaceEntry>, ToolError>;

    /// ファイル内容を読む（存在しなければ `ToolError::Invalid`）。
    async fn read(
        &self,
        ctx: &AuthContext,
        name: &str,
        trace_id: Option<&str>,
    ) -> Result<Vec<u8>, ToolError>;

    /// ファイルを書く（作成 or 新版）。書込イベント→再索引に乗る。
    async fn write(
        &self,
        ctx: &AuthContext,
        name: &str,
        bytes: Vec<u8>,
        content_type: &str,
        trace_id: Option<&str>,
    ) -> Result<WorkspaceWrite, ToolError>;

    /// ファイルを削除する（soft delete・存在しなければ `ToolError::Invalid`）。
    async fn delete(
        &self,
        ctx: &AuthContext,
        name: &str,
        trace_id: Option<&str>,
    ) -> Result<(), ToolError>;
}

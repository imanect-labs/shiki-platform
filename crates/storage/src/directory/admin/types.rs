use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// 外部グループをロールへ対応付ける置換案。未指定グループは変更しない。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GroupRoleMapping {
    pub expected_version: i64,
    pub group_id: String,
    pub role_ids: Vec<String>,
}

/// 一つの安定IDを持つロールの作成・変更。削除は別の明示APIで扱う。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RoleEdit {
    pub id: String,
    pub label: String,
    pub parent_id: Option<String>,
    /// 作成は0、更新は読取時の版。
    pub expected_version: i64,
}

/// 管理操作。Idempotency-Keyの内容が変われば拒否する。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DirectoryChangeSet {
    pub idempotency_key: Uuid,
    pub roles: Vec<RoleEdit>,
    pub mappings: Vec<GroupRoleMapping>,
}

/// Keycloakから取得した自組織のグループ。機微な属性は返さない。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, sqlx::FromRow)]
pub struct AdGroup {
    pub id: String,
    pub name: String,
    pub claim_path: String,
    pub distinguished_name: Option<String>,
    #[sqlx(default)]
    pub mapping_version: i64,
    #[sqlx(default)]
    pub role_ids: Vec<String>,
    pub ou_path: Vec<String>,
    pub parent_ids: Vec<String>,
}

/// 永続化された管理操作の進行状態。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, sqlx::FromRow)]
pub struct DirectoryOperation {
    pub id: Uuid,
    pub state: String,
    pub error_code: Option<String>,
    pub attempts: i32,
    pub processed: i32,
    pub total: i32,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AdGroupPage {
    pub items: Vec<AdGroup>,
    pub next_cursor: Option<String>,
    pub total: i64,
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct AdFolder {
    pub name: String,
    pub group_count: i64,
}
#[derive(Debug, Serialize, ToSchema)]
pub struct AdFolderPage {
    pub items: Vec<AdFolder>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct DirectoryMember {
    pub id: String,
    pub email: String,
    pub display_name: String,
    /// 最終認証同期時の直接ロール。表示は先頭20件。
    pub role_names: Vec<String>,
    pub role_count: i64,
}
#[derive(Debug, Serialize, ToSchema)]
pub struct DirectoryMemberPage {
    pub items: Vec<DirectoryMember>,
    pub next_cursor: Option<String>,
}

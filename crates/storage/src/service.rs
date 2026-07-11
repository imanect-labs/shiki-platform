//! StorageService — 権限・監査・content-addressing の単一チョークポイント（Task 1.3/1.4/1.9）。
//!
//! 不変条件:
//! - 全 read/write メソッドは第 1 引数に `&AuthContext` を取り、OpenFGA `check` を必ず通す。
//! - ハンドラに `db`/`store` を直接触らせない（このサービス経由でのみアクセス）。
//! - 各操作は allow/deny を監査ログに残す（書込系は同一 txn で原子的に）。
//! - バイトは presigned URL でクライアント↔MinIO 直転送し、アプリはメタ操作のみ（PIT-6）。

use std::{sync::Arc, time::Duration};

use authz::{
    AuthContext, AuthzClient, Consistency, FgaObject, Namespace, ObjectType, Principal, Relation,
    Subject,
};
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    audit::{self, AuditEntry, AuditRecorder, Chain, Decision},
    content_address::{
        blob_object_key, incoming_object_key, is_valid_sha256_hex, staging_object_key,
    },
    error::StorageError,
    event::{self, WriteEvent, WriteOp},
    model::{
        ChildPage, ChildSort, ChildSortKey, Crumb, DownloadTicket, FileVersion, Node, NodeKind,
        ShareEntry, ShareRole, ShareTarget, UploadTicket,
    },
    object_store::ObjectStore,
};

/// `node` テーブルの選択カラム（NodeRow と一致させる）。
const NODE_COLS: &str = "id, org, tenant_id, kind, name, parent_id, blob_sha256, size_bytes, \
                         content_type, version, deleted_at, created_by, created_at, updated_at";

/// 単一チョークポイントの StorageService。
pub struct StorageService {
    db: PgPool,
    store: Arc<dyn ObjectStore>,
    authz: Arc<dyn AuthzClient>,
    audit: AuditRecorder,
    presign_get_ttl: Duration,
    presign_put_ttl: Duration,
    /// 1 ファイルの最大アップロードサイズ（バイト）。declare の宣言サイズがこれを超えたら拒否し、
    /// 認証ユーザーによる無制限アップロードでのストレージ枯渇を防ぐ（容量ガード）。
    max_upload_size: i64,
}

#[derive(sqlx::FromRow)]
struct NodeRow {
    id: Uuid,
    org: String,
    tenant_id: String,
    kind: String,
    name: String,
    parent_id: Option<Uuid>,
    blob_sha256: Option<String>,
    size_bytes: Option<i64>,
    content_type: Option<String>,
    version: i64,
    deleted_at: Option<DateTime<Utc>>,
    created_by: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct PendingRow {
    parent_id: Option<Uuid>,
    name: String,
    content_type: String,
    declared_sha256: String,
    declared_size: i64,
    staging_key: String,
    /// 内容更新（既存ファイルの新版）の対象。NULL は新規ファイル作成。
    target_node_id: Option<Uuid>,
}

#[derive(sqlx::FromRow)]
struct VersionRow {
    tenant_id: String,
    version: i64,
    blob_sha256: String,
    size_bytes: i64,
    content_type: String,
    author: String,
    created_at: DateTime<Utc>,
}

impl VersionRow {
    fn into_model(self) -> FileVersion {
        FileVersion {
            tenant_id: self.tenant_id,
            version: self.version,
            blob_sha256: self.blob_sha256,
            size_bytes: self.size_bytes,
            content_type: self.content_type,
            author: self.author,
            created_at: self.created_at,
        }
    }
}

impl StorageService {
    pub fn new(
        db: PgPool,
        store: Arc<dyn ObjectStore>,
        authz: Arc<dyn AuthzClient>,
        presign_get_ttl: Duration,
        presign_put_ttl: Duration,
        max_upload_size: i64,
    ) -> Self {
        let audit = AuditRecorder::new(db.clone());
        StorageService {
            db,
            store,
            authz,
            audit,
            presign_get_ttl,
            presign_put_ttl,
            max_upload_size,
        }
    }
}

mod admin;
mod content_update;
mod finalize;
mod folder;
mod internal_io;
mod move_rename;
mod read;
mod restore;
mod sharing;
mod trash;
mod upload;
mod versions;
mod workspace_io;

pub use workspace_io::WriteAtOutcome;

fn row_to_node(row: NodeRow) -> Result<Node, StorageError> {
    let kind = NodeKind::parse(&row.kind)
        .ok_or_else(|| StorageError::Integrity(format!("未知のノード種別: {}", row.kind)))?;
    Ok(Node {
        id: row.id,
        org: row.org,
        tenant_id: row.tenant_id,
        kind,
        name: row.name,
        parent_id: row.parent_id,
        blob_sha256: row.blob_sha256,
        size_bytes: row.size_bytes,
        content_type: row.content_type,
        version: row.version,
        deleted_at: row.deleted_at,
        created_by: row.created_by,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

/// 共有先 subject の検証。subject 識別子（`user:<tenant>|<id>` / `role:<tenant>|<id>#member`）を
/// 壊す/曖昧化する id を弾く。文字ポリシーの正本は [`authz::validate_local_id`]
/// （`:`/`#`＝型/userset 区切り・`|`＝tenant 名前空間区切り・制御文字・空を拒否。単一定義・#91 M-3）。
/// 前後空白の拒否（trim で往復が変わる混乱の防止）だけは共有 API 固有のルールとしてここで足す。
///
/// role の**存在**（当該テナントに実在するロールか）はここでは強制しない: 全メンバーのログイン前でも
/// 部署（AD group 由来 role）へ共有できるようにするため（dangling grant 回避は共有ダイアログの
/// `directory_role` オートコンプリートで担保し、厳格な存在ゲートは IdP フル同期後に足す）。
fn validate_share_target(target: &ShareTarget) -> Result<(), StorageError> {
    let id = match target {
        ShareTarget::User { id } | ShareTarget::Role { id } => id,
    };
    if id != id.trim() {
        return Err(StorageError::Invalid(
            "共有先 id の前後に空白は使えません".into(),
        ));
    }
    authz::validate_local_id(id)
        .map_err(|violation| StorageError::Invalid(format!("共有先 id が不正です: {violation}")))?;
    Ok(())
}

/// admin プレーン操作（テナント・プロビジョニング/撤去）用の合成コンテキスト。
///
/// 呼び出しユーザーの `AuthContext` が存在しない管理操作でも、識別子構築（`ns()`）と
/// 監査記録を実行時と同じ経路に通すための継ぎ目。`actor` は呼び出し主体
/// （provisioner の `azp` 等）を監査ログの actor 列に刻む（#91 M-7: 固定 `"system"` だと
/// provisioner 資格情報の不正使用をどのクライアントか追えない）。
fn system_ctx(tenant_id: &str, org: &str, actor: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: actor.to_string(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant_id.to_string()),
        },
        org.to_string(),
        tenant_id.to_string(),
    )
}

/// ノード種別に対応する OpenFGA オブジェクト識別子（tenant 名前空間化済み・SAAS.1）。
/// `file:<tenant>|<id>` / `folder:<tenant>|<id>`。
fn node_fga_object(ns: &Namespace<'_>, kind: NodeKind, id: Uuid) -> FgaObject {
    match kind {
        NodeKind::File => ns.file(&id.to_string()),
        NodeKind::Folder => ns.folder(&id.to_string()),
    }
}

/// リネーム/移動の監査アクション名（種別ごと）。
fn update_action(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::File => "file.update",
        NodeKind::Folder => "folder.update",
    }
}

/// ソートキー 1 文字タグ（カーソルの先頭に置き、別ソートでのカーソル誤用を弾く）。
fn child_sort_tag(key: ChildSortKey) -> char {
    match key {
        ChildSortKey::Name => 'n',
        ChildSortKey::Updated => 'u',
        ChildSortKey::Size => 's',
    }
}

/// 行から「現在のソートキーの値」を text として取り出す（カーソルへ織り込む）。
/// SQL 側の列式（name / updated_at / coalesce(size,0)）と一致させる。
fn child_sort_value(key: ChildSortKey, row: &NodeRow) -> String {
    match key {
        ChildSortKey::Name => row.name.clone(),
        // timestamptz は RFC3339 で表現し、SQL 側で `::timestamptz` にキャストして比較する。
        ChildSortKey::Updated => row.updated_at.to_rfc3339(),
        ChildSortKey::Size => row.size_bytes.unwrap_or(0).to_string(),
    }
}

/// 子一覧 keyset カーソルの不透明エンコード。`tag(1)`＋`id`(36桁)＋`value` を連結し hex 化する
/// （uuid は固定長・value は最後尾なので区切り不要）。クライアントには不透明。
fn encode_child_cursor(sort: ChildSort, value: &str, id: Uuid) -> String {
    let tag = child_sort_tag(sort.key);
    hex::encode(format!("{tag}{id}{value}").as_bytes())
}

/// [`encode_child_cursor`] の逆。壊れた/別ソートのカーソルは `Invalid`（panic しない）。
fn decode_child_cursor(sort: ChildSort, cursor: &str) -> Result<(String, Uuid), StorageError> {
    let invalid = || StorageError::Invalid("カーソルが不正です".into());
    let bytes = hex::decode(cursor).map_err(|_| invalid())?;
    // 先頭 1 バイトがソートタグ、続く 36 **バイト**が uuid（ASCII 固定長）、残りが value。
    // バイト境界で分割してから UTF-8 検証する（マルチバイト境界外の split は panic するため）。
    if bytes.len() < 1 + 36 {
        return Err(invalid());
    }
    let (tag_bytes, rest) = bytes.split_at(1);
    if tag_bytes[0] != child_sort_tag(sort.key) as u8 {
        // ソート条件を変えたのに古いカーソルを使い回した等。誤った keyset 比較を避けて拒否する。
        return Err(StorageError::Invalid(
            "カーソルが現在のソート条件と一致しません".into(),
        ));
    }
    let (id_bytes, value_bytes) = rest.split_at(36);
    let id_part = std::str::from_utf8(id_bytes).map_err(|_| invalid())?;
    let id = Uuid::parse_str(id_part).map_err(|_| invalid())?;
    let value = String::from_utf8(value_bytes.to_vec()).map_err(|_| invalid())?;
    Ok((value, id))
}

/// タイムスタンプ keyset カーソルの不透明エンコード。`(ts(rfc3339), id)` を連結し hex 化する
/// （uuid は先頭固定長・タイムスタンプは末尾なので曖昧さなし）。ゴミ箱・共有一覧で共用。
fn encode_ts_cursor(ts: &str, id: Uuid) -> String {
    hex::encode(format!("{id}{ts}").as_bytes())
}

/// [`encode_ts_cursor`] の逆。壊れたカーソルは `Invalid`（panic しない）。
fn decode_ts_cursor(cursor: &str) -> Result<(String, Uuid), StorageError> {
    let invalid = || StorageError::Invalid("カーソルが不正です".into());
    let bytes = hex::decode(cursor).map_err(|_| invalid())?;
    if bytes.len() < 36 {
        return Err(invalid());
    }
    let (id_bytes, ts_bytes) = bytes.split_at(36);
    let id_part = std::str::from_utf8(id_bytes).map_err(|_| invalid())?;
    let id = Uuid::parse_str(id_part).map_err(|_| invalid())?;
    let ts = String::from_utf8(ts_bytes.to_vec()).map_err(|_| invalid())?;
    Ok((ts, id))
}

/// ノード名の検証。空/長すぎ/前後空白/`.`・`..`/パス区切り/制御文字を拒否する。
///
/// 名前は download の `Content-Disposition` ヘッダにも流れるため、`\r`/`\n` 等の制御文字を
/// 弾いてヘッダインジェクションの素地を断つ。前後空白は黙って trim せず拒否する（往復で
/// 名前が変わる混乱を避ける）。`.`/`..` は UI/同期での予約名衝突を避けるため拒否する。
fn validate_name(name: &str) -> Result<(), StorageError> {
    if name.is_empty() {
        return Err(StorageError::Invalid("名前が空です".into()));
    }
    if name.chars().count() > 255 {
        return Err(StorageError::Invalid(
            "名前が長すぎます（255 文字以内）".into(),
        ));
    }
    if name != name.trim() {
        return Err(StorageError::Invalid("名前の前後に空白は使えません".into()));
    }
    if name == "." || name == ".." {
        return Err(StorageError::Invalid("名前に . / .. は使えません".into()));
    }
    if name.contains('/') {
        return Err(StorageError::Invalid("名前に / は使えません".into()));
    }
    if name.chars().any(char::is_control) {
        return Err(StorageError::Invalid("名前に制御文字は使えません".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// tenant 名前空間化された識別子を組むための最小 `AuthContext`（純関数テスト用）。
    fn test_ctx(tenant_id: &str) -> AuthContext {
        AuthContext::new(
            authz::Principal {
                kind: authz::PrincipalKind::User,
                id: "u1".into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some(tenant_id.into()),
            },
            "org1".into(),
            tenant_id.into(),
        )
    }

    #[test]
    fn validate_name_rejects_bad_inputs() {
        assert!(validate_name("").is_err());
        assert!(validate_name("   ").is_err()); // 前後空白（trim で空）
        assert!(validate_name(" leading").is_err());
        assert!(validate_name("trailing ").is_err());
        assert!(validate_name(".").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("bad\nname").is_err()); // 制御文字（改行）
        assert!(validate_name("bad\rname").is_err());
        assert!(validate_name("bad\u{0}name").is_err()); // NUL
        assert!(validate_name(&"x".repeat(256)).is_err());
        assert!(validate_name("report.pdf").is_ok());
        assert!(validate_name("日本語.txt").is_ok());
        assert!(validate_name("a.b.c").is_ok()); // ドットを含む通常名は可
    }

    #[test]
    fn child_cursor_roundtrips() {
        let id = Uuid::new_v4();
        let sort = ChildSort::default();
        for value in ["report.pdf", "日本語フォルダ", "a", &"x".repeat(255)] {
            let c = encode_child_cursor(sort, value, id);
            let (got_value, got_id) = decode_child_cursor(sort, &c).expect("decode");
            assert_eq!(got_value, value);
            assert_eq!(got_id, id);
        }
    }

    #[test]
    fn child_cursor_roundtrips_each_sort_key() {
        // 各ソートキーで往復し、値（rfc3339 / 数値文字列 / 名前）を保てること。
        let id = Uuid::new_v4();
        for (key, value) in [
            (ChildSortKey::Name, "report.pdf"),
            (ChildSortKey::Updated, "2026-06-25T10:00:00+00:00"),
            (ChildSortKey::Size, "4096"),
        ] {
            let sort = ChildSort { key, desc: true };
            let c = encode_child_cursor(sort, value, id);
            let (got, got_id) = decode_child_cursor(sort, &c).expect("decode");
            assert_eq!(got, value);
            assert_eq!(got_id, id);
        }
    }

    #[test]
    fn child_cursor_rejects_cursor_from_other_sort() {
        // ソートを変えたのに古いカーソルを使い回すと拒否（誤った keyset 比較を防ぐ）。
        let id = Uuid::new_v4();
        let c = encode_child_cursor(
            ChildSort {
                key: ChildSortKey::Name,
                desc: false,
            },
            "a",
            id,
        );
        assert!(decode_child_cursor(
            ChildSort {
                key: ChildSortKey::Size,
                desc: false,
            },
            &c
        )
        .is_err());
    }

    #[test]
    fn child_cursor_rejects_garbage() {
        let sort = ChildSort::default();
        assert!(decode_child_cursor(sort, "zzz").is_err()); // 非 hex
        assert!(decode_child_cursor(sort, &hex::encode("short")).is_err()); // 1+36 バイト未満
                                                                            // 境界がマルチバイト文字の途中でも **panic せず** Invalid を返す（split_at 回帰）。
        let mut raw = vec![b'n']; // 正しいタグ
        raw.extend(std::iter::repeat_n(b'a', 35));
        raw.extend_from_slice("あ".as_bytes()); // 3 バイト → uuid 境界が文字途中
        assert!(decode_child_cursor(sort, &hex::encode(raw)).is_err());
    }

    #[test]
    fn validate_share_target_rejects_bad_ids() {
        use crate::model::ShareTarget;
        // user / role とも同一ルール。正常系（role は AD group 由来の `/` を含んでも可）。
        assert!(validate_share_target(&ShareTarget::User { id: "alice".into() }).is_ok());
        assert!(validate_share_target(&ShareTarget::Role { id: "sales".into() }).is_ok());
        assert!(validate_share_target(&ShareTarget::Role {
            id: "sales/team-1".into()
        })
        .is_ok());
        // 異常系（`|`＝tenant 区切りも拒否）。
        for bad in ["", " alice", "alice ", "a:b", "a#member", "a|b", "bad\nid"] {
            assert!(
                validate_share_target(&ShareTarget::User { id: bad.into() }).is_err(),
                "user should reject {bad:?}"
            );
            assert!(
                validate_share_target(&ShareTarget::Role { id: bad.into() }).is_err(),
                "role should reject {bad:?}"
            );
        }
    }

    #[test]
    fn node_fga_object_maps_kind() {
        // tenant 名前空間化済み（`file:<tenant>|<id>` / `folder:<tenant>|<id>`）。
        let ctx = test_ctx("acme");
        let ns = ctx.ns();
        let id = Uuid::nil();
        assert_eq!(
            node_fga_object(&ns, NodeKind::File, id).as_str(),
            format!("file:acme|{id}")
        );
        assert_eq!(
            node_fga_object(&ns, NodeKind::Folder, id).as_str(),
            format!("folder:acme|{id}")
        );
    }
}

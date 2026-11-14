//! StorageService — 権限・監査・content-addressing の単一チョークポイント（Task 1.3/1.4/1.9）。
//!
//! 不変条件:
//! - 全 read/write メソッドは第 1 引数に `&AuthContext` を取り、OpenFGA `check` を必ず通す。
//! - ハンドラに `db`/`store` を直接触らせない（このサービス経由でのみアクセス）。
//! - 各操作は allow/deny を監査ログに残す（書込系は同一 txn で原子的に）。
//! - バイトは presigned URL でクライアント↔オブジェクトストア直転送し、アプリはメタ操作のみ（PIT-6）。

use std::{sync::Arc, time::Duration};

use authz::{
    AuthContext, AuthzClient, Consistency, FgaObject, Namespace, ObjectType, Principal, Relation,
    Subject, TenantId,
};
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use tokio::sync::Notify;
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
                         content_type, version, deleted_at, created_by, updated_by, created_at, \
                         updated_at";

/// 内容版の採番式（migration 0049・提案バージョン対応）。
///
/// 提案バージョン（`node_version.is_proposal`）も PK `(node_id, version)` の番号空間を
/// 共有するため、`node.version + 1` 固定では提案の版番号と衝突し得る。通常版の作成は
/// 常に「current と履歴最大値の大きい方 + 1」で採番する（提案が無ければ `+ 1` と等価）。
// tenant-sql: allow(採番式の断片・外側の UPDATE が tenant_id で絞る)
const NEXT_CONTENT_VERSION: &str = "GREATEST(version, COALESCE((SELECT MAX(v.version) \
     FROM node_version v WHERE v.node_id = node.id), 0)) + 1";

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
    /// 共有リンク有効期限の失効タイマを起こす通知（#342）。リンク発行/延長/redeem で今より早い
    /// 期限を設定したら `notify_one()` し、タイマが次回起床時刻を再計算する（定期ポーリング回避）。
    expiry_notify: Arc<Notify>,
    /// redeem の総当たり/Argon2 CPU DoS を抑える固定窓レート制限（#342 レビュー B-3・プロセス内）。
    /// principal / token を prefix 付き鍵で同じ器に載せる。
    redeem_limiter: share_link_ratelimit::RedeemRateLimiter,
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
    updated_by: String,
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
    /// 内容更新で申告した元の版（楽観ロック・#827）。NULL は無条件の新版。
    base_version: Option<i64>,
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
    is_proposal: bool,
    proposed_by: Option<String>,
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
            is_proposal: self.is_proposal,
            proposed_by: self.proposed_by,
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
            expiry_notify: Arc::new(Notify::new()),
            redeem_limiter: share_link_ratelimit::RedeemRateLimiter::default(),
        }
    }

    /// 共有リンク失効タイマ用の通知ハンドル（#342）。タイマ spawner がこれを購読し、
    /// `next_share_link_expiry()` まで sleep しつつ、新期限設定の `notify_one()` で起きる。
    pub fn expiry_notify(&self) -> Arc<Notify> {
        Arc::clone(&self.expiry_notify)
    }
}

mod admin;
mod content_update;
mod finalize;
mod folder;
mod internal_io;
mod labels;
mod move_rename;
mod node_scan;
mod proposal;
mod purge;
mod purge_empty;
mod purge_pending;
mod read;
mod reindex;
mod restore;
mod share_link;
mod share_link_expiry;
mod share_link_grant;
mod share_link_ratelimit;
mod share_link_reconcile;
mod share_link_redeem;
mod share_link_util;
mod shared_list;
mod sharing;
mod trash;
mod upload;
mod upload_gc;
mod versions;
mod workspace_io;

pub use labels::{NodeLabel, MAX_NODE_LABELS};
pub use node_scan::{named_scan_sql, NODE_SCAN_MAX_CHECKS};
pub use purge::{PurgeOutcome, PurgeReport};
pub use purge_empty::{
    EmptyTrashOutcome, TrashEntry, TrashPage, EMPTY_TRASH_MAX_NODES, EMPTY_TRASH_MAX_ROOTS,
};
pub use purge_pending::PurgeSweep;
pub use restore::RevertOutcome;
pub use shared_list::{shared_scan_sql, SHARED_SCAN_CHUNK, SHARED_SCAN_MAX_CHECKS};
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
        updated_by: row.updated_by,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

/// admin プレーン操作（テナント・プロビジョニング/撤去）用の合成コンテキスト。
///
/// 呼び出しユーザーの `AuthContext` が存在しない管理操作でも、識別子構築（`ns()`）と
/// 監査記録を実行時と同じ経路に通すための継ぎ目。`actor` は呼び出し主体
/// （provisioner の `azp` 等）を監査ログの actor 列に刻む（#91 M-7: 固定 `"system"` だと
/// provisioner 資格情報の不正使用をどのクライアントか追えない）。
///
/// `tenant_id` は管理 API の入力でも DB の値でも、文字ポリシー（[`TenantId::parse`]）を通す。
fn system_ctx(tenant_id: &str, org: &str, actor: &str) -> Result<AuthContext, StorageError> {
    let tenant = TenantId::parse(tenant_id)
        .map_err(|e| StorageError::Invalid(format!("tenant_id が不正です: {e}")))?;
    Ok(AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: actor.to_string(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant_id.to_string()),
        },
        org.to_string(),
        tenant,
    ))
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
fn update_action(kind: NodeKind) -> crate::audit::AuditAction {
    use crate::audit::AuditAction;
    match kind {
        NodeKind::File => AuditAction::FileUpdate,
        NodeKind::Folder => AuditAction::FolderUpdate,
    }
}

/// ソートキー 1 文字タグ（カーソルの先頭に置き、別ソートでのカーソル誤用を弾く）。
///
/// 大文字はフォルダ優先を織り込んだ現行形式（#1040）。旧形式（小文字・種別なし）のカーソルは
/// タグ不一致で拒否される（keyset の比較列が違うため使い回すと欠落・重複し得る）。
fn child_sort_tag(key: ChildSortKey) -> char {
    match key {
        ChildSortKey::Name => 'N',
        ChildSortKey::Updated => 'U',
        ChildSortKey::Size => 'S',
    }
}

/// ソート方向 1 文字タグ（カーソルに置き、昇順で得たカーソルの降順での誤用を弾く）。
fn child_sort_dir_tag(sort: ChildSort) -> char {
    if sort.desc {
        'D'
    } else {
        'A'
    }
}

/// 子一覧・名前検索の並び（**フォルダ優先** → ソートキー → 名前 → id）を SQL 断片にしたもの（#1040）。
///
/// プレースホルダは呼び出し側と共通: `$4` = カーソルのソート値（text）、`$5` = カーソルの id、
/// `$8` = カーソル行の種別（`folder`/`file`）、`$9` = カーソル行の名前。
struct ChildOrderSql {
    /// `ORDER BY` の中身。
    order_by: String,
    /// keyset 条件（カーソル無しなら素通し）。
    keyset: String,
}

/// 名前の自然順照合（migration 0110）。ORDER BY と keyset の両方に同じ照合を付けて順序を一致させる。
const NAME_COLLATED: &str = "name COLLATE node_name_natural";

/// [`ChildOrderSql`] を組む。
///
/// フォルダを昇降どちらでも先頭に置きつつ keyset を 1 本の行比較に保つため、種別の順位式を
/// 方向に合わせて反転する（昇順は `kind <> 'folder'`＝フォルダが false で先、降順は
/// `kind = 'folder'`＝フォルダが true で先）。更新日時・サイズが同値の行（サイズ順のフォルダは
/// 全て 0）は名前の自然順で並べ、最後に id で一意に決める（id だけだと見た目が無作為になる）。
fn child_order_sql(sort: ChildSort) -> ChildOrderSql {
    let (dir, cmp, rank_op) = if sort.desc {
        ("DESC", "<", "=")
    } else {
        ("ASC", ">", "<>")
    };
    let name_param = "$9::text COLLATE node_name_natural";
    let (cols, params) = match sort.key {
        ChildSortKey::Name => (vec![NAME_COLLATED], vec![name_param]),
        ChildSortKey::Updated => (
            vec!["updated_at", NAME_COLLATED],
            vec!["$4::timestamptz", name_param],
        ),
        // フォルダは size_bytes が NULL のため 0 とみなす（NULL を keyset から排除）。
        ChildSortKey::Size => (
            vec!["coalesce(size_bytes, 0)", NAME_COLLATED],
            vec!["$4::bigint", name_param],
        ),
    };
    let rank = format!("(kind {rank_op} 'folder')");
    let rank_param = format!("($8::text {rank_op} 'folder')");
    let order_by = std::iter::once(rank.as_str())
        .chain(cols.iter().copied())
        .chain(std::iter::once("id"))
        .map(|c| format!("{c} {dir}"))
        .collect::<Vec<_>>()
        .join(", ");
    ChildOrderSql {
        order_by,
        keyset: format!(
            "($4::text IS NULL OR ({rank}, {}, id) {cmp} ({rank_param}, {}, $5))",
            cols.join(", "),
            params.join(", ")
        ),
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

/// 子一覧 keyset カーソルの位置（カーソル行のソート値・名前・id・種別）。
#[derive(Debug, PartialEq, Eq)]
struct ChildCursor {
    value: String,
    name: String,
    id: Uuid,
    kind: NodeKind,
}

/// 子一覧 keyset カーソルの不透明エンコード。`tag(1)`＋`方向(1)`＋`種別(1)`＋`id`(36桁)＋`value` を連結し
/// hex 化する（uuid は固定長・value は最後尾なので区切り不要）。クライアントには不透明。
/// 名前順以外は value の後ろに `\n`＋名前を足す（同値の行を名前で並べるため）。value
/// （RFC3339・整数）は改行を含まないので最初の `\n` で一意に分けられる。
fn encode_child_cursor(sort: ChildSort, c: &ChildCursor) -> String {
    let tag = child_sort_tag(sort.key);
    let dir = child_sort_dir_tag(sort);
    let k = match c.kind {
        NodeKind::Folder => 'd',
        NodeKind::File => 'f',
    };
    let id = c.id;
    let body = match sort.key {
        ChildSortKey::Name => c.name.clone(),
        ChildSortKey::Updated | ChildSortKey::Size => format!("{}\n{}", c.value, c.name),
    };
    hex::encode(format!("{tag}{dir}{k}{id}{body}").as_bytes())
}

/// [`encode_child_cursor`] の逆。壊れた/別ソートのカーソルは `Invalid`（panic しない）。
fn decode_child_cursor(sort: ChildSort, cursor: &str) -> Result<ChildCursor, StorageError> {
    let invalid = || StorageError::Invalid("カーソルが不正です".into());
    let bytes = hex::decode(cursor).map_err(|_| invalid())?;
    // 先頭 1 バイトがソートタグ、次の 1 バイトが方向、次の 1 バイトが種別、続く 36 **バイト**が
    // uuid（ASCII 固定長）、残りが本体。バイト境界で分割してから UTF-8 検証する
    // （マルチバイト境界外の split は panic）。
    if bytes.len() < 3 + 36 {
        return Err(invalid());
    }
    let (head, rest) = bytes.split_at(3);
    // キーか方向が違うカーソルは keyset の比較の向き・列が合わず、欠落・重複し得るので拒否する。
    if head[0] != child_sort_tag(sort.key) as u8 || head[1] != child_sort_dir_tag(sort) as u8 {
        // ソート条件を変えたのに古いカーソルを使い回した等。誤った keyset 比較を避けて拒否する。
        return Err(StorageError::Invalid(
            "カーソルが現在のソート条件と一致しません".into(),
        ));
    }
    let kind = match head[2] {
        b'd' => NodeKind::Folder,
        b'f' => NodeKind::File,
        _ => return Err(invalid()),
    };
    let (id_bytes, body_bytes) = rest.split_at(36);
    let id_part = std::str::from_utf8(id_bytes).map_err(|_| invalid())?;
    let id = Uuid::parse_str(id_part).map_err(|_| invalid())?;
    let body = String::from_utf8(body_bytes.to_vec()).map_err(|_| invalid())?;
    let (value, name) = match sort.key {
        ChildSortKey::Name => (body.clone(), body),
        ChildSortKey::Updated | ChildSortKey::Size => {
            let (v, n) = body.split_once('\n').ok_or_else(invalid)?;
            (v.to_string(), n.to_string())
        }
    };
    Ok(ChildCursor {
        value,
        name,
        id,
        kind,
    })
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

/// ファイル名の拡張子（最後の `.` より後ろ）。末尾がドットのものは拡張子なしとみなす。
///
/// `.docx` のような先頭ドットだけの名前も拡張子ありとして扱う。開く画面の判定（web の
/// `endsWith`・Office API の `rsplit_once`）がそう読むので、ここで「拡張子なし」とみなすと
/// `README` → `.docx` のような改名が素通りし、中身と画面が食い違う。
fn file_extension(name: &str) -> Option<&str> {
    match name.rsplit_once('.') {
        Some((_, ext)) if !ext.is_empty() => Some(ext),
        _ => None,
    }
}

/// 改名で拡張子が変わらないことを確かめる（大文字小文字の違いは同じとみなす・#617）。
fn ensure_same_extension(current: &str, new_name: &str) -> Result<(), StorageError> {
    let (cur, new) = (file_extension(current), file_extension(new_name));
    let same = match (cur, new) {
        (None, None) => true,
        (Some(a), Some(b)) => a.to_lowercase() == b.to_lowercase(),
        _ => false,
    };
    if same {
        return Ok(());
    }
    Err(StorageError::Invalid(match cur {
        Some(ext) => format!("拡張子は変更できません（.{ext} のままにしてください）"),
        None => "拡張子のないファイルに拡張子は付けられません".into(),
    }))
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
            authz::TenantId::parse(tenant_id).unwrap(),
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
    fn rename_keeps_file_extension() {
        assert!(ensure_same_extension("提案書.docx", "提案書_改訂.docx").is_ok());
        assert!(ensure_same_extension("提案書.docx", "提案書.DOCX").is_ok()); // 大小のみ
        assert!(ensure_same_extension("a.tar.gz", "b.tar.gz").is_ok());
        assert!(ensure_same_extension("README", "README2").is_ok());
        assert!(ensure_same_extension(".env", "local.env").is_ok());
        // 先頭ドットだけの名前も拡張子として読む（開く画面の判定と揃える）。
        assert!(ensure_same_extension("README", ".docx").is_err());
        assert!(ensure_same_extension(".md", ".docx").is_err());
        assert!(ensure_same_extension(".env", ".env.local").is_err());
        assert!(ensure_same_extension("メモ.md", "メモ.docx").is_err());
        assert!(ensure_same_extension("表.xlsx", "表.md").is_err());
        assert!(ensure_same_extension("メモ.md", "メモ").is_err()); // 外す
        assert!(ensure_same_extension("README", "README.md").is_err()); // 付ける
        assert!(ensure_same_extension("a.tar.gz", "a.tar").is_err());
    }

    /// テスト用のカーソル位置（id は毎回ランダム）。
    fn cursor(value: &str, name: &str, kind: NodeKind) -> ChildCursor {
        ChildCursor {
            value: value.into(),
            name: name.into(),
            id: Uuid::new_v4(),
            kind,
        }
    }

    #[test]
    fn child_cursor_roundtrips() {
        let sort = ChildSort::default();
        for kind in [NodeKind::Folder, NodeKind::File] {
            for name in ["report.pdf", "日本語フォルダ", "a", &"x".repeat(255)] {
                let c = cursor(name, name, kind);
                let got =
                    decode_child_cursor(sort, &encode_child_cursor(sort, &c)).expect("decode");
                assert_eq!(got, c);
            }
        }
    }

    #[test]
    fn child_cursor_roundtrips_each_sort_key() {
        // 各ソートキーで往復し、値（rfc3339 / 数値文字列）と同値時の並びに使う名前を保てること。
        for (key, value) in [
            (ChildSortKey::Updated, "2026-06-25T10:00:00+00:00"),
            (ChildSortKey::Size, "4096"),
        ] {
            let sort = ChildSort { key, desc: true };
            let c = cursor(value, "資料 2.txt", NodeKind::File);
            let got = decode_child_cursor(sort, &encode_child_cursor(sort, &c)).expect("decode");
            assert_eq!(got, c);
        }
    }

    #[test]
    fn child_cursor_rejects_missing_name_part() {
        // 名前順以外で `\n`＋名前が欠けたカーソルは不正（keyset の比較値が揃わない）。
        let raw = format!("SAf{}4096", Uuid::new_v4());
        let sort = ChildSort {
            key: ChildSortKey::Size,
            desc: false,
        };
        assert!(decode_child_cursor(sort, &hex::encode(raw)).is_err());
    }

    #[test]
    fn child_cursor_rejects_cursor_from_other_sort() {
        // ソートを変えたのに古いカーソルを使い回すと拒否（誤った keyset 比較を防ぐ）。
        let c = encode_child_cursor(
            ChildSort {
                key: ChildSortKey::Name,
                desc: false,
            },
            &cursor("a", "a", NodeKind::Folder),
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
    fn child_cursor_rejects_cursor_from_other_direction() {
        // 昇順で得たカーソルを降順に渡すと、逆向きの keyset 比較で欠落・重複するので拒否する。
        let asc = ChildSort::default();
        let desc = ChildSort {
            key: ChildSortKey::Name,
            desc: true,
        };
        let c = encode_child_cursor(asc, &cursor("a", "a", NodeKind::File));
        assert!(decode_child_cursor(desc, &c).is_err());
        assert!(decode_child_cursor(asc, &c).is_ok());
    }

    #[test]
    fn child_cursor_rejects_legacy_format() {
        // フォルダ優先以前（小文字タグ・種別なし）のカーソルは keyset の列が違うので拒否する。
        let legacy = hex::encode(format!("n{}a", Uuid::new_v4()));
        assert!(decode_child_cursor(ChildSort::default(), &legacy).is_err());
    }

    #[test]
    fn child_cursor_rejects_garbage() {
        let sort = ChildSort::default();
        assert!(decode_child_cursor(sort, "zzz").is_err()); // 非 hex
        assert!(decode_child_cursor(sort, &hex::encode("short")).is_err()); // 3+36 バイト未満
        let mut bad_kind = vec![b'N', b'A', b'x'];
        bad_kind.extend(Uuid::new_v4().to_string().bytes());
        assert!(decode_child_cursor(sort, &hex::encode(bad_kind)).is_err()); // 未知の種別
                                                                             // 境界がマルチバイト文字の途中でも **panic せず** Invalid を返す（split_at 回帰）。
        let mut raw = vec![b'N', b'A', b'f']; // 正しいタグ・方向・種別
        raw.extend(std::iter::repeat_n(b'a', 35));
        raw.extend_from_slice("あ".as_bytes()); // 3 バイト → uuid 境界が文字途中
        assert!(decode_child_cursor(sort, &hex::encode(raw)).is_err());
    }

    #[test]
    fn child_order_keeps_folders_first_both_directions() {
        // 昇順は `kind <> 'folder'` ASC、降順は `kind = 'folder'` DESC → どちらもフォルダが先頭。
        let asc = child_order_sql(ChildSort::default());
        assert_eq!(
            asc.order_by,
            "(kind <> 'folder') ASC, name COLLATE node_name_natural ASC, id ASC"
        );
        assert!(asc
            .keyset
            .contains(") > (($8::text <> 'folder'), $9::text COLLATE node_name_natural, $5)"));
        // 同値（サイズ順のフォルダは全て 0）は名前の自然順 → id で並べる。
        let desc = child_order_sql(ChildSort {
            key: ChildSortKey::Size,
            desc: true,
        });
        assert_eq!(
            desc.order_by,
            "(kind = 'folder') DESC, coalesce(size_bytes, 0) DESC, \
             name COLLATE node_name_natural DESC, id DESC"
        );
        assert!(desc.keyset.contains(
            ") < (($8::text = 'folder'), $4::bigint, $9::text COLLATE node_name_natural, $5)"
        ));
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

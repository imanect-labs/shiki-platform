//! StorageService: 一般アクセス（共有リンクの公開範囲・#338）— owner 側のポリシー管理。
//!
//! Google Drive の「一般アクセス」に相当。owner がノードに `organization`（組織内）/
//! `anyone`（すべての認証済みユーザー）の公開範囲を設定でき、有効期限・パスワードを付けられる。
//! 認可の正本は OpenFGA タプル（`organization#member` / `user:*` を viewer/editor に付与）で、
//! このモジュールはその付与/剥奪とポリシー台帳（`node_general_access`）の更新を扱う。
//!
//! redeem（パスワード解錠）と失効処理（遅延失効・イベント駆動タイマ）は
//! [`super::general_access_redeem`] に分割している（1 ファイル 500 行ガード）。

#[allow(clippy::wildcard_imports)]
use super::*;

use crate::model::{GeneralAccess, GeneralAccessLevel};

/// 新しく設定する一般アクセス（restricted 以外）。`None` は restricted（＝台帳行の削除）を表す。
pub(super) struct NewPolicy {
    pub(super) level: GeneralAccessLevel,
    pub(super) role: ShareRole,
    pub(super) expires_at: Option<DateTime<Utc>>,
    /// Argon2id PHC 文字列（`None` = パスワード無し）。有りのときは broad タプルを書かない。
    pub(super) password_hash: Option<String>,
}

/// レベルに対応する broad な共有先 subject（restricted は `None`）。
/// `organization` → `organization:<tenant>|<org>#member`、`anyone` → `user:*`。
/// redeem/失効モジュールと共有する（`pub(super)`）。
pub(super) fn broad_subject(
    ns: &Namespace<'_>,
    level: GeneralAccessLevel,
    org: &str,
) -> Option<Subject> {
    match level {
        GeneralAccessLevel::Organization => Some(ns.organization_member(org)),
        GeneralAccessLevel::Anyone => Some(Subject::public()),
        GeneralAccessLevel::Restricted => None,
    }
}

/// FGA object の型プレフィクスからノード種別を判定する（`folder:` 以外は file 扱い）。
fn kind_of(obj: &FgaObject) -> NodeKind {
    if obj.as_str().starts_with("folder:") {
        NodeKind::Folder
    } else {
        NodeKind::File
    }
}

/// `node_general_access` の 1 行（読み出し用）。redeem モジュールと共有する。
#[derive(sqlx::FromRow)]
pub(super) struct PolicyRow {
    pub(super) org: String,
    pub(super) level: String,
    pub(super) role: String,
    pub(super) expires_at: Option<DateTime<Utc>>,
    pub(super) password_hash: Option<String>,
}

impl StorageService {
    /// 一般アクセスの現在設定を返す（owner 権限）。行が無ければ restricted。
    /// パスワードは `has_password` のみ露出し、ハッシュ/平文は返さない。
    pub async fn get_general_access(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<GeneralAccess, StorageError> {
        self.authorize_share_admin(ctx, node_id, "node.general_access.get", trace_id)
            .await?;
        let row: Option<PolicyRow> = sqlx::query_as(
            "SELECT org, level, role, expires_at, password_hash \
             FROM node_general_access WHERE node_id = $1 AND tenant_id = $2",
        )
        .bind(node_id)
        .bind(&ctx.tenant_id)
        .fetch_optional(&self.db)
        .await?;
        let Some(row) = row else {
            return Ok(GeneralAccess::restricted());
        };
        let (Some(level), Some(role)) = (
            GeneralAccessLevel::parse(&row.level),
            ShareRole::parse(&row.role),
        ) else {
            return Err(StorageError::Integrity(format!(
                "一般アクセスの level/role が不正: {}/{}",
                row.level, row.role
            )));
        };
        Ok(GeneralAccess {
            level,
            role,
            expires_at: row.expires_at,
            has_password: row.password_hash.is_some(),
        })
    }

    /// 一般アクセスを設定する（owner 権限）。`level == Restricted` は clear と同義。
    ///
    /// パスワード有りのときは broad タプルを書かず（redeem 発行に委ねる）、行のみ更新する。
    /// パスワードの扱いは 3 値: 新パスワード指定→更新、`keep_password`→既存ハッシュ引き継ぎ
    /// （level/期限だけ変更する編集で再入力を強いない）、どちらでもない→パスワード無し。
    #[allow(clippy::too_many_arguments)]
    pub async fn set_general_access(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        level: GeneralAccessLevel,
        role: ShareRole,
        expires_at: Option<DateTime<Utc>>,
        password: Option<&str>,
        keep_password: bool,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        if level == GeneralAccessLevel::Restricted {
            return self.clear_general_access(ctx, node_id, trace_id).await;
        }
        let password_hash = match password {
            Some(pw) if !pw.is_empty() => Some(hash_password(pw)?),
            _ if keep_password => self.existing_password_hash(ctx, node_id).await?,
            _ => None,
        };
        let new = NewPolicy {
            level,
            role,
            expires_at,
            password_hash,
        };
        self.apply_general_access(ctx, node_id, Some(new), "node.general_access.set", trace_id)
            .await
    }

    /// 一般アクセスを解除して restricted へ戻す（owner 権限）。broad タプルと redeem 済み
    /// per-user タプルを剥奪し、台帳行を削除する。
    pub async fn clear_general_access(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        self.apply_general_access(ctx, node_id, None, "node.general_access.clear", trace_id)
            .await
    }

    /// 既存の一般アクセス行のパスワードハッシュを引く（`keep_password` 編集用・無ければ `None`）。
    async fn existing_password_hash(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
    ) -> Result<Option<String>, StorageError> {
        let hash: Option<Option<String>> = sqlx::query_scalar(
            "SELECT password_hash FROM node_general_access WHERE node_id = $1 AND tenant_id = $2",
        )
        .bind(node_id)
        .bind(&ctx.tenant_id)
        .fetch_optional(&self.db)
        .await?;
        Ok(hash.flatten())
    }

    /// set/clear の共通処理。`new = None` は restricted（clear）。
    ///
    /// 順序（fail-closed 指向）: ①owner 認可 → ②旧 broad タプル＋既存 redeem 全タプルを剥奪
    /// （アクセスを残さない・剥奪の巻き戻しはしない＝失敗しても過小権限で安全）→ ③新 broad
    /// タプルを付与（パスワード無しのときのみ）→ ④DB（行 upsert/削除＋台帳全削除＋監査）を 1 tx
    /// でコミット → 失敗時は ③で付与した新タプルのみ補償剥奪する。
    async fn apply_general_access(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        new: Option<NewPolicy>,
        action: &'static str,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        let obj = self
            .authorize_share_admin(ctx, node_id, action, trace_id)
            .await?;
        let kind = kind_of(&obj);
        let ns = ctx.ns();

        // ② 既存を読んで旧タプルを剥奪する。
        let existing: Option<PolicyRow> = sqlx::query_as(
            "SELECT org, level, role, expires_at, password_hash \
             FROM node_general_access WHERE node_id = $1 AND tenant_id = $2",
        )
        .bind(node_id)
        .bind(&ctx.tenant_id)
        .fetch_optional(&self.db)
        .await?;
        if let Some(ex) = &existing {
            // 旧 broad タプル（パスワード無しのときのみ書かれていた）。
            if ex.password_hash.is_none() {
                if let (Some(level), Some(role)) = (
                    GeneralAccessLevel::parse(&ex.level),
                    ShareRole::parse(&ex.role),
                ) {
                    if let Some(subject) = broad_subject(&ns, level, &ex.org) {
                        let _ = self
                            .authz
                            .delete_tuple(&subject, role.relation(), &obj)
                            .await?;
                    }
                }
            }
        }
        // 既存 redeem 済み per-user タプルを剥奪する（ポリシー変更で redeem をやり直させる）。
        let grants: Vec<(String, String)> = sqlx::query_as(
            "SELECT user_id, role FROM node_general_access_grant \
             WHERE node_id = $1 AND tenant_id = $2",
        )
        .bind(node_id)
        .bind(&ctx.tenant_id)
        .fetch_all(&self.db)
        .await?;
        for (user_id, grole) in &grants {
            if let Some(role) = ShareRole::parse(grole) {
                let _ = self
                    .authz
                    .delete_tuple(&ns.user(user_id), role.relation(), &obj)
                    .await?;
            }
        }

        // ③ 新 broad タプルを付与（パスワード無しのときのみ）。補償用に (subject, relation) を控える。
        let mut compensation: Option<(Subject, Relation)> = None;
        if let Some(np) = &new {
            if np.password_hash.is_none() {
                if let Some(subject) = broad_subject(&ns, np.level, &ctx.org) {
                    let rel = np.role.relation();
                    let granted = self.authz.write_tuple(&subject, rel, &obj).await?;
                    if granted {
                        compensation = Some((subject, rel));
                    }
                }
            }
        }

        // ④ DB 反映（行 upsert/削除＋台帳全削除＋ハッシュチェーン監査）を 1 tx で。
        if let Err(e) = self
            .persist_general_access(ctx, node_id, kind, new.as_ref(), action, trace_id)
            .await
        {
            if let Some((subject, rel)) = compensation {
                let _ = self.authz.delete_tuple(&subject, rel, &obj).await;
            }
            return Err(e);
        }

        // 新しい期限を設定したら失効タイマを起こして次回起床を再計算させる。
        if new.as_ref().and_then(|n| n.expires_at).is_some() {
            self.expiry_notify.notify_one();
        }
        Ok(())
    }

    /// 一般アクセスの DB 反映（行 upsert/削除＋台帳全削除＋監査）を 1 tx で行う。
    async fn persist_general_access(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        kind: NodeKind,
        new: Option<&NewPolicy>,
        action: &'static str,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        let mut tx = self.db.begin().await?;
        // ポリシー変更時は既存の redeem 台帳を全消去する（タプルは呼び出し元で剥奪済み）。
        sqlx::query("DELETE FROM node_general_access_grant WHERE node_id = $1 AND tenant_id = $2")
            .bind(node_id)
            .bind(&ctx.tenant_id)
            .execute(&mut *tx)
            .await?;
        let metadata = match new {
            None => json!({}),
            Some(np) => {
                sqlx::query(
                    "INSERT INTO node_general_access \
                       (node_id, tenant_id, org, kind, level, role, expires_at, password_hash, created_by, updated_by) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9) \
                     ON CONFLICT (node_id) DO UPDATE SET \
                       level = EXCLUDED.level, role = EXCLUDED.role, expires_at = EXCLUDED.expires_at, \
                       password_hash = EXCLUDED.password_hash, updated_by = EXCLUDED.updated_by, updated_at = now()",
                )
                .bind(node_id)
                .bind(&ctx.tenant_id)
                .bind(&ctx.org)
                .bind(kind.as_str())
                .bind(np.level.as_str())
                .bind(np.role.as_str())
                .bind(np.expires_at)
                .bind(np.password_hash.as_deref())
                .bind(&ctx.principal.id)
                .execute(&mut *tx)
                .await?;
                json!({
                    "level": np.level,
                    "role": np.role,
                    "expires_at": np.expires_at,
                    "has_password": np.password_hash.is_some(),
                })
            }
        };
        if new.is_none() {
            sqlx::query("DELETE FROM node_general_access WHERE node_id = $1 AND tenant_id = $2")
                .bind(node_id)
                .bind(&ctx.tenant_id)
                .execute(&mut *tx)
                .await?;
        }
        audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action,
                object_type: kind.as_str(),
                object_id: &node_id.to_string(),
                decision: Decision::Allow,
                trace_id,
                metadata,
            },
            Chain::Yes,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

/// パスワードを Argon2id で PHC 文字列にハッシュ化する（ソルトは CSPRNG）。
/// redeem モジュールの検証と対で使う（`pub(super)`）。
pub(super) fn hash_password(password: &str) -> Result<String, StorageError> {
    use argon2::password_hash::rand_core::OsRng;
    use argon2::password_hash::{PasswordHasher, SaltString};
    use argon2::Argon2;

    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|_| StorageError::Integrity("パスワードハッシュ生成に失敗しました".into()))
}

/// パスワードを PHC 文字列に対して検証する（定数時間・失敗はすべて false へ潰す＝オラクル防止）。
pub(super) fn verify_password(password: &str, phc: &str) -> bool {
    use argon2::password_hash::{PasswordHash, PasswordVerifier};
    use argon2::Argon2;

    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{hash_password, verify_password};

    #[test]
    fn password_hash_roundtrips_and_rejects_wrong() {
        let phc = hash_password("s3cret-passphrase").unwrap();
        // PHC 文字列は Argon2id のものであること。
        assert!(phc.starts_with("$argon2"));
        assert!(verify_password("s3cret-passphrase", &phc));
        assert!(!verify_password("wrong", &phc));
        // 壊れた PHC は検証失敗（パニックしない）。
        assert!(!verify_password("x", "not-a-phc-string"));
    }

    #[test]
    fn password_hashes_are_salted_unique() {
        // 同じパスワードでもソルトが異なり別ハッシュになること。
        let a = hash_password("same").unwrap();
        let b = hash_password("same").unwrap();
        assert_ne!(a, b);
        assert!(verify_password("same", &a));
        assert!(verify_password("same", &b));
    }
}

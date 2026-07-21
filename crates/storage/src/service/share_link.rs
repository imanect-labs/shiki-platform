//! StorageService: 共有リンク（#342）— owner 側の発行・一覧・失効・延長と broad タプル reconcile。
//!
//! #338/#339 の一般アクセス（1 node 1 ポリシー）を作り替え、1 リソースに複数のリンクを
//! ぶら下げる。各リンクは範囲(audience)・権限・有効期限・パスワードを持ち、個別に失効/延長できる。
//! 認可の正本は OpenFGA タプルで、broad タプル集合は「**active な全リンクの (subject,relation)
//! 和集合**」の射影として [`StorageService::reconcile_broad`] で合わせる（複数リンクが同一タプルを
//! 共有する参照カウントを正しく扱う）。
//!
//! redeem（パスワード解錠・per-user 参照カウント）は [`super::share_link_redeem`]、失効処理
//! （遅延失効・イベント駆動タイマ）は [`super::share_link_expiry`] に分割している（500 行ガード）。

#[allow(clippy::wildcard_imports)]
use super::*;

use super::share_link_util::{hash_password, kind_of, new_share_token};
use crate::model::{GeneralAccessLevel, ShareLink};

/// 一覧/発行結果のリンク 1 行。
#[derive(sqlx::FromRow)]
struct ShareLinkRow {
    link_id: Uuid,
    token: String,
    audience: String,
    role: String,
    expires_at: Option<DateTime<Utc>>,
    has_password: bool,
    label: Option<String>,
    created_at: DateTime<Utc>,
}

impl ShareLinkRow {
    fn into_model(self) -> Result<ShareLink, StorageError> {
        let (Some(audience), Some(role)) = (
            GeneralAccessLevel::parse(&self.audience),
            ShareRole::parse(&self.role),
        ) else {
            return Err(StorageError::Integrity(format!(
                "共有リンクの audience/role が不正: {}/{}",
                self.audience, self.role
            )));
        };
        Ok(ShareLink {
            link_id: self.link_id,
            token: self.token,
            audience,
            role,
            expires_at: self.expires_at,
            has_password: self.has_password,
            label: self.label,
            created_at: self.created_at,
        })
    }
}

/// active（未失効・未期限切れ）リンクを絞る SQL 述語（プレースホルダは呼び出し側 bind に依存しない）。
const ACTIVE_PREDICATE: &str = "revoked_at IS NULL AND (expires_at IS NULL OR expires_at > now())";

impl StorageService {
    /// 共有リンクを発行する（owner 権限）。発行結果（token 含む）を返す。
    ///
    /// `audience == Restricted` は付与ゼロの純ポインタ（既存アクセス者向け）。`password` 指定時は
    /// broad タプルを書かず redeem 経由で per-user タプルを発行する。認可通過**後**にパスワードを
    /// ハッシュ化する（未認可で argon2 を走らせない・DoS 対策）。
    #[allow(clippy::too_many_arguments)]
    pub async fn create_share_link(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        audience: GeneralAccessLevel,
        role: ShareRole,
        expires_at: Option<DateTime<Utc>>,
        password: Option<&str>,
        label: Option<&str>,
        trace_id: Option<&str>,
    ) -> Result<ShareLink, StorageError> {
        let obj = self
            .authorize_share_admin(ctx, node_id, "node.share_link.create", trace_id)
            .await?;
        let kind = kind_of(&obj);
        let ns = ctx.ns();
        let password_hash = match password {
            Some(pw) if !pw.is_empty() => Some(hash_password(pw)?),
            _ => None,
        };
        let link_id = Uuid::new_v4();
        let token = new_share_token();
        let created_at = Utc::now();

        let mut tx = self.db.begin().await?;
        self.lock_node(&mut tx, node_id).await?;
        sqlx::query(
            "INSERT INTO node_share_link \
               (link_id, node_id, tenant_id, org, kind, audience, role, token, expires_at, password_hash, label, created_by, updated_by) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $12)",
        )
        .bind(link_id)
        .bind(node_id)
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(kind.as_str())
        .bind(audience.as_str())
        .bind(role.as_str())
        .bind(&token)
        .bind(expires_at)
        .bind(password_hash.as_deref())
        .bind(label)
        .bind(&ctx.principal.id)
        .execute(&mut *tx)
        .await?;

        let added = self
            .reconcile_broad(
                &mut tx,
                &ns,
                &obj,
                node_id,
                &ctx.tenant_id,
                &ctx.org,
                Utc::now(),
            )
            .await?;

        if let Err(e) = self
            .finalize_share_link_tx(
                tx,
                ctx,
                node_id,
                kind,
                "node.share_link.create",
                json!({
                    "link_id": link_id,
                    "audience": audience,
                    "role": role,
                    "expires_at": expires_at,
                    "has_password": password_hash.is_some(),
                }),
                trace_id,
            )
            .await
        {
            self.compensate_broad(&obj, &added).await;
            return Err(e);
        }
        if expires_at.is_some() {
            self.expiry_notify.notify_one();
        }
        Ok(ShareLink {
            link_id,
            token,
            audience,
            role,
            expires_at,
            has_password: password_hash.is_some(),
            label: label.map(str::to_owned),
            created_at,
        })
    }

    /// ノードの active な共有リンク一覧を返す（owner 権限）。失効/期限切れは含めない。
    pub async fn list_share_links(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Vec<ShareLink>, StorageError> {
        self.authorize_share_admin(ctx, node_id, "node.share_link.list", trace_id)
            .await?;
        let rows: Vec<ShareLinkRow> = sqlx::query_as(&format!(
            "SELECT link_id, token, audience, role, expires_at, \
                    (password_hash IS NOT NULL) AS has_password, label, created_at \
             FROM node_share_link \
             WHERE node_id = $1 AND tenant_id = $2 AND {ACTIVE_PREDICATE} \
             ORDER BY created_at DESC",
        ))
        .bind(node_id)
        .bind(&ctx.tenant_id)
        .fetch_all(&self.db)
        .await?;
        rows.into_iter().map(ShareLinkRow::into_model).collect()
    }

    /// 共有リンクを失効する（owner 権限）。broad タプルを reconcile（他 active が要求しない分のみ
    /// 剥奪）し、このリンクの redeem 済み per-user タプルを参照カウントして剥奪する。
    pub async fn revoke_share_link(
        &self,
        ctx: &AuthContext,
        link_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        let Some((node_id, obj)) = self
            .authorize_link_owner(ctx, link_id, "node.share_link.revoke", trace_id)
            .await?
        else {
            return Err(StorageError::Forbidden);
        };
        let ns = ctx.ns();

        let mut tx = self.db.begin().await?;
        self.lock_node(&mut tx, node_id).await?;
        // ソフト失効（active だったものだけ）。
        let updated = sqlx::query(&format!(
            "UPDATE node_share_link SET revoked_at = now(), updated_at = now(), updated_by = $3 \
             WHERE link_id = $1 AND tenant_id = $2 AND {ACTIVE_PREDICATE}",
        ))
        .bind(link_id)
        .bind(&ctx.tenant_id)
        .bind(&ctx.principal.id)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() == 0 {
            // 既に失効/期限切れ＝冪等成功（タプルは reconcile が整合させる）。
            return Ok(());
        }
        // broad タプルを reconcile（このリンクは active から外れた）。
        let added = self
            .reconcile_broad(
                &mut tx,
                &ns,
                &obj,
                node_id,
                &ctx.tenant_id,
                &ctx.org,
                Utc::now(),
            )
            .await?;
        // このリンクの per-user redeem タプルを参照カウントして剥奪。
        if let Err(e) = self
            .reconcile_user_grants_for_link(
                &mut tx,
                &ns,
                &obj,
                link_id,
                node_id,
                &ctx.tenant_id,
                Utc::now(),
            )
            .await
        {
            self.compensate_broad(&obj, &added).await;
            return Err(e);
        }
        if let Err(e) = self
            .finalize_share_link_tx(
                tx,
                ctx,
                node_id,
                kind_of(&obj),
                "node.share_link.revoke",
                json!({ "link_id": link_id }),
                trace_id,
            )
            .await
        {
            self.compensate_broad(&obj, &added).await;
            return Err(e);
        }
        Ok(())
    }

    /// 共有リンクの有効期限を延長/変更する（owner 権限。`expires_at = None` で無期限化）。
    /// 期限切れだったリンクを未来へ延ばした場合は reconcile で broad タプルを復活させる。
    pub async fn extend_share_link(
        &self,
        ctx: &AuthContext,
        link_id: Uuid,
        expires_at: Option<DateTime<Utc>>,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        let Some((node_id, obj)) = self
            .authorize_link_owner(ctx, link_id, "node.share_link.extend", trace_id)
            .await?
        else {
            return Err(StorageError::Forbidden);
        };
        let ns = ctx.ns();

        let mut tx = self.db.begin().await?;
        self.lock_node(&mut tx, node_id).await?;
        // 未失効リンクのみ延長できる（失効済みは対象外）。
        let updated = sqlx::query(
            "UPDATE node_share_link SET expires_at = $3, updated_at = now(), updated_by = $4 \
             WHERE link_id = $1 AND tenant_id = $2 AND revoked_at IS NULL",
        )
        .bind(link_id)
        .bind(&ctx.tenant_id)
        .bind(expires_at)
        .bind(&ctx.principal.id)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() == 0 {
            return Err(StorageError::Forbidden);
        }
        // 台帳の per-user 期限スナップショットも追随させる（タイマ sweep の基準を一致させる）。
        sqlx::query("UPDATE node_share_link_grant SET expires_at = $2 WHERE link_id = $1")
            .bind(link_id)
            .bind(expires_at)
            .execute(&mut *tx)
            .await?;
        // 期限切れ→未来へ延ばした場合に broad タプルを復活（active に戻る）。
        let added = self
            .reconcile_broad(
                &mut tx,
                &ns,
                &obj,
                node_id,
                &ctx.tenant_id,
                &ctx.org,
                Utc::now(),
            )
            .await?;
        if let Err(e) = self
            .finalize_share_link_tx(
                tx,
                ctx,
                node_id,
                kind_of(&obj),
                "node.share_link.extend",
                json!({ "link_id": link_id, "expires_at": expires_at }),
                trace_id,
            )
            .await
        {
            self.compensate_broad(&obj, &added).await;
            return Err(e);
        }
        self.expiry_notify.notify_one();
        Ok(())
    }

    /// link_id からリンク所有 node を解決し、その node の owner 認可を通す（二段認可）。
    /// 見つからない/別テナントのリンクは `Ok(None)`（存在秘匿・呼び出し側で Forbidden）。
    async fn authorize_link_owner(
        &self,
        ctx: &AuthContext,
        link_id: Uuid,
        action: &'static str,
        trace_id: Option<&str>,
    ) -> Result<Option<(Uuid, FgaObject)>, StorageError> {
        let node_id: Option<Uuid> = sqlx::query_scalar(
            "SELECT node_id FROM node_share_link WHERE link_id = $1 AND tenant_id = $2",
        )
        .bind(link_id)
        .bind(&ctx.tenant_id)
        .fetch_optional(&self.db)
        .await?;
        let Some(node_id) = node_id else {
            return Ok(None);
        };
        let obj = self
            .authorize_share_admin(ctx, node_id, action, trace_id)
            .await?;
        Ok(Some((node_id, obj)))
    }

    /// 同一 node の reconcile を直列化する advisory lock（並行 create/revoke の lost-update 防止）。
    pub(super) async fn lock_node(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        node_id: Uuid,
    ) -> Result<(), StorageError> {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('share_link:' || $1, 0))")
            .bind(node_id.to_string())
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    /// 監査記録＋コミットをまとめる（失敗時は呼び出し側が broad 補償剥奪する）。
    #[allow(clippy::too_many_arguments)]
    async fn finalize_share_link_tx(
        &self,
        mut tx: sqlx::Transaction<'_, sqlx::Postgres>,
        ctx: &AuthContext,
        node_id: Uuid,
        kind: NodeKind,
        action: &'static str,
        metadata: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
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

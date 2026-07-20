//! StorageService: 一般アクセス（#338）— パスワード解錠（redeem）と有効期限の失効処理。
//!
//! - [`StorageService::redeem_general_access`]: パスワード検証後に呼び出しユーザーへ per-user
//!   タプルを発行する（authenticated なら誰でも・失敗は一律 403）。
//! - 失効: [`StorageService::enforce_general_access_expiry`]（セッション開始点の遅延失効・
//!   defense-in-depth）と [`StorageService::revoke_expired_general_access`]（イベント駆動タイマ）＋
//!   [`StorageService::next_general_access_expiry`]（タイマの次回起床時刻）。
//!
//! ポリシー管理と共有するヘルパ（`broad_subject`/`PolicyRow`/`verify_password`）は
//! [`super::general_access`] に定義している。

#[allow(clippy::wildcard_imports)]
use super::*;

use super::general_access::{broad_subject, verify_password, PolicyRow};
use crate::model::GeneralAccessLevel;

/// 失効タイマが剥奪する期限切れ redeem 済み per-user 付与。
#[derive(sqlx::FromRow)]
struct ExpiredGrant {
    node_id: Uuid,
    user_id: String,
    tenant_id: String,
    kind: String,
    role: String,
}

/// 失効タイマが剥奪する期限切れの一般アクセスポリシー。
#[derive(sqlx::FromRow)]
struct ExpiredPolicy {
    node_id: Uuid,
    tenant_id: String,
    org: String,
    kind: String,
    level: String,
    role: String,
}

impl StorageService {
    /// パスワード付き一般アクセスを解錠し、呼び出しユーザーへ per-user タプルを発行する（#338）。
    ///
    /// **authenticated であれば誰でも**呼べる（owner ゲート無し）。失敗理由は区別せず一律 `Forbidden`
    /// に潰す（オラクル防止・存在秘匿・WOPI token の fail-closed collapse に倣う）。
    pub async fn redeem_general_access(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        password: &str,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        // 存在秘匿: 見えない/別テナントのノードは一律 Forbidden。
        let Ok(node) = self.load_node(ctx, node_id, false).await else {
            return Err(StorageError::Forbidden);
        };
        let row: Option<PolicyRow> = sqlx::query_as(
            "SELECT org, level, role, expires_at, password_hash \
             FROM node_general_access WHERE node_id = $1 AND tenant_id = $2",
        )
        .bind(node_id)
        .bind(&ctx.tenant_id)
        .fetch_optional(&self.db)
        .await?;
        // redeem はパスワード付き一般アクセス専用。
        let Some(row) = row else {
            return Err(StorageError::Forbidden);
        };
        let Some(hash) = row.password_hash.as_deref() else {
            return Err(StorageError::Forbidden);
        };
        let (Some(level), Some(role)) = (
            GeneralAccessLevel::parse(&row.level),
            ShareRole::parse(&row.role),
        ) else {
            return Err(StorageError::Forbidden);
        };
        // 期限切れは Forbidden（ちょうど exp == now も失効扱い・fail-closed）。
        if let Some(exp) = row.expires_at {
            if exp <= Utc::now() {
                return Err(StorageError::Forbidden);
            }
        }
        // レベルの該当性: organization は当該組織のメンバーのみ、anyone は認証済みなら誰でも。
        match level {
            GeneralAccessLevel::Anyone => {}
            GeneralAccessLevel::Organization => {
                let member = self
                    .authz
                    .check(
                        &ctx.subject(),
                        Relation::Member,
                        &ctx.ns().organization(&row.org),
                        Consistency::MinimizeLatency,
                    )
                    .await?;
                if !member {
                    return Err(StorageError::Forbidden);
                }
            }
            GeneralAccessLevel::Restricted => return Err(StorageError::Forbidden),
        }
        // パスワード検証（Argon2id・定数時間）。不一致は generic Forbidden。
        if !verify_password(password, hash) {
            return Err(StorageError::Forbidden);
        }

        // per-user タプルを発行し、redeem 台帳へ記録する（失効処理が明示共有と区別するため）。
        let obj = node_fga_object(&ctx.ns(), node.kind, node_id);
        let subject = ctx.ns().user(&ctx.principal.id);
        let granted = self
            .authz
            .write_tuple(&subject, role.relation(), &obj)
            .await?;
        let persisted = self
            .persist_redeem(
                ctx,
                node_id,
                node.kind,
                role,
                row.expires_at,
                level,
                trace_id,
            )
            .await;
        if let Err(e) = persisted {
            if granted {
                let _ = self
                    .authz
                    .delete_tuple(&subject, role.relation(), &obj)
                    .await;
            }
            return Err(e);
        }
        if row.expires_at.is_some() {
            self.expiry_notify.notify_one();
        }
        Ok(())
    }

    /// redeem の台帳 upsert＋監査を 1 tx で。
    #[allow(clippy::too_many_arguments)]
    async fn persist_redeem(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        kind: NodeKind,
        role: ShareRole,
        expires_at: Option<DateTime<Utc>>,
        level: GeneralAccessLevel,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        let mut tx = self.db.begin().await?;
        sqlx::query(
            "INSERT INTO node_general_access_grant \
               (node_id, user_id, tenant_id, kind, role, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (node_id, user_id) DO UPDATE SET \
               role = EXCLUDED.role, expires_at = EXCLUDED.expires_at, granted_at = now()",
        )
        .bind(node_id)
        .bind(&ctx.principal.id)
        .bind(&ctx.tenant_id)
        .bind(kind.as_str())
        .bind(role.as_str())
        .bind(expires_at)
        .execute(&mut *tx)
        .await?;
        audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: "node.general_access.redeem",
                object_type: kind.as_str(),
                object_id: &node_id.to_string(),
                decision: Decision::Allow,
                trace_id,
                metadata: json!({ "level": level, "role": role }),
            },
            Chain::Yes,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// セッション開始点の遅延失効（#338・defense-in-depth）。当該ノードに期限切れの一般アクセスが
    /// あれば broad タプルと redeem 済み per-user タプルを先行剥奪して台帳行を掃除する。
    ///
    /// PK プローブ 1 回（期限付き & 期限切れの行のみヒット）で、一般アクセスを持たない大多数の
    /// ノードでは 0 行＝ほぼ無コスト。長寿命セッションの厳密失効はイベント駆動タイマが担う。
    pub(crate) async fn enforce_general_access_expiry(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        kind: NodeKind,
    ) -> Result<(), StorageError> {
        let row: Option<(String, String, String)> = sqlx::query_as(
            "SELECT org, level, role FROM node_general_access \
             WHERE node_id = $1 AND tenant_id = $2 \
               AND expires_at IS NOT NULL AND expires_at <= now()",
        )
        .bind(node_id)
        .bind(&ctx.tenant_id)
        .fetch_optional(&self.db)
        .await?;
        let Some((org, level, role)) = row else {
            return Ok(());
        };
        let ns = ctx.ns();
        let obj = node_fga_object(&ns, kind, node_id);
        if let (Some(level), Some(role)) =
            (GeneralAccessLevel::parse(&level), ShareRole::parse(&role))
        {
            if let Some(subject) = broad_subject(&ns, level, &org) {
                let _ = self
                    .authz
                    .delete_tuple(&subject, role.relation(), &obj)
                    .await;
            }
        }
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
                    .await;
            }
        }
        sqlx::query("DELETE FROM node_general_access WHERE node_id = $1 AND tenant_id = $2")
            .bind(node_id)
            .bind(&ctx.tenant_id)
            .execute(&self.db)
            .await?;
        sqlx::query("DELETE FROM node_general_access_grant WHERE node_id = $1 AND tenant_id = $2")
            .bind(node_id)
            .bind(&ctx.tenant_id)
            .execute(&self.db)
            .await?;
        Ok(())
    }

    /// 期限切れの一般アクセス（broad＋redeem）を剥奪し台帳を掃除する（#338・イベント駆動タイマから）。
    ///
    /// admin プレーン（`AuthContext` 無し）。`expires_at <= now` の行だけを処理する。返り値は
    /// 剥奪した件数（ログ用の概数）。全テナント横断で走るが識別子は保存済み tenant_id から
    /// `Namespace::for_tenant` で再構成するため越境しない。
    pub async fn revoke_expired_general_access(
        &self,
        now: DateTime<Utc>,
    ) -> Result<u64, StorageError> {
        let mut count: u64 = 0;

        // ① 期限切れの redeem 済み per-user タプル。
        let grants: Vec<ExpiredGrant> = sqlx::query_as(
            "SELECT node_id, user_id, tenant_id, kind, role FROM node_general_access_grant \
             WHERE expires_at IS NOT NULL AND expires_at <= $1 LIMIT 1000",
        )
        .bind(now)
        .fetch_all(&self.db)
        .await?;
        for g in grants {
            let ns = Namespace::for_tenant(&g.tenant_id);
            if let (Some(kind), Some(role)) = (NodeKind::parse(&g.kind), ShareRole::parse(&g.role))
            {
                let obj = node_fga_object(&ns, kind, g.node_id);
                let _ = self
                    .authz
                    .delete_tuple(&ns.user(&g.user_id), role.relation(), &obj)
                    .await;
            }
            sqlx::query(
                "DELETE FROM node_general_access_grant WHERE node_id = $1 AND user_id = $2",
            )
            .bind(g.node_id)
            .bind(&g.user_id)
            .execute(&self.db)
            .await?;
            count += 1;
        }

        // ② 期限切れのポリシー（broad タプル剥奪＋行削除＋監査）。
        let policies: Vec<ExpiredPolicy> = sqlx::query_as(
            "SELECT node_id, tenant_id, org, kind, level, role FROM node_general_access \
             WHERE expires_at IS NOT NULL AND expires_at <= $1 LIMIT 1000",
        )
        .bind(now)
        .fetch_all(&self.db)
        .await?;
        for p in policies {
            let ns = Namespace::for_tenant(&p.tenant_id);
            if let (Some(kind), Some(level), Some(role)) = (
                NodeKind::parse(&p.kind),
                GeneralAccessLevel::parse(&p.level),
                ShareRole::parse(&p.role),
            ) {
                let obj = node_fga_object(&ns, kind, p.node_id);
                // broad タプルはパスワード付きだと書かれていないが、delete は冪等 no-op で安全。
                if let Some(subject) = broad_subject(&ns, level, &p.org) {
                    let _ = self
                        .authz
                        .delete_tuple(&subject, role.relation(), &obj)
                        .await;
                }
            }
            sqlx::query("DELETE FROM node_general_access WHERE node_id = $1")
                .bind(p.node_id)
                .execute(&self.db)
                .await?;
            // 失効の監査（system ctx・非チェーン。set/redeem はチェーン監査済み）。
            let ctx = system_ctx(&p.tenant_id, &p.org, "system");
            let _ = self
                .audit
                .record(
                    &ctx,
                    AuditEntry {
                        action: "node.general_access.expire",
                        object_type: &p.kind,
                        object_id: &p.node_id.to_string(),
                        decision: Decision::Allow,
                        trace_id: None,
                        metadata: json!({ "level": p.level }),
                    },
                )
                .await;
            count += 1;
        }
        Ok(count)
    }

    /// 次に失効する一般アクセスの時刻（policy/grant の両テーブル横断の最小 `expires_at`）。
    /// 失効タイマが次回起床時刻に使う。期限付き行が 1 つも無ければ `None`。
    pub async fn next_general_access_expiry(&self) -> Result<Option<DateTime<Utc>>, StorageError> {
        // LEAST は NULL 引数を無視する（両方 NULL のときのみ NULL）。
        let row: (Option<DateTime<Utc>>,) = sqlx::query_as(
            "SELECT LEAST(\
               (SELECT MIN(expires_at) FROM node_general_access WHERE expires_at IS NOT NULL),\
               (SELECT MIN(expires_at) FROM node_general_access_grant WHERE expires_at IS NOT NULL))",
        )
        .fetch_one(&self.db)
        .await?;
        Ok(row.0)
    }
}

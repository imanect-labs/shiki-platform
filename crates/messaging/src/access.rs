//! 組織の設定と、監査担当者による閲覧・エクスポート（eDiscovery・design §4.14・Task 14.9）。
//!
//! # 不変条件（PIT-65）
//!
//! - **通常の read 経路に相乗りしない。** この閲覧は [`crate::store_message::fetch_messages`] を
//!   使わず、専用の [`admin_messages`] で読む。逆に通常の一覧・検索・AI の文脈からは、承認の有無に
//!   かかわらずここへ到達しない（FGA に「管理者は読める」relation を作らない）。
//! - 閲覧するのは**監査担当者**（`organization#audit_viewer`）。組織の管理者（`admin`）には与えない。
//! - 公開チャンネルは承認レコード不要。**DM と非公開チャンネルは承認レコードの期間内だけ**。
//!   承認者はテナントが指名し、申請者本人は承認できず、未指名なら閲覧できない（fail-closed）。
//! - 対象はチャンネル単位＋発言の期間（利用者単位は作らない）。期間は既定 7 日・最長 30 日。
//! - 全操作を `audit_log` へ落とす。読まれた本人には通知しない。

use authz::{AuthContext, Consistency, Relation};
use chat::model::ContentBlock;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use storage::audit::{record_on, AuditAction, AuditEntry, Chain, Decision};
use storage::db_tenant::begin_tenant_tx;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::store::{self, ChannelRow, SettingsRow};
use crate::{ChannelKind, Error, MessagingService};

/// 閲覧できる日数の既定と上限。
pub const DEFAULT_ACCESS_DAYS: i32 = 7;
pub const MAX_ACCESS_DAYS: i32 = 30;
/// 1 回の閲覧・エクスポートで返す発言の上限。
const ADMIN_READ_LIMIT: i64 = 20_000;

/// 組織のメッセージ設定。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MessagingSettings {
    /// `everyone`（組織メンバー全員）／`admins`（組織の管理者だけ）が公開チャンネルを作れる。
    pub public_channel_creators: String,
    /// 発言の保存期間（日）。`None` は無期限（既定）。監査ログの保存期間より短くできない。
    pub retention_days: Option<i32>,
    /// DM・非公開チャンネルの閲覧を承認する人（利用者 id）。空なら閲覧できない。
    pub access_approvers: Vec<String>,
    /// 楽観ロック（読んだ値をそのまま返す）。
    pub rev: i64,
    /// 監査ログの保存期間（日・参考表示）。
    #[serde(default)]
    pub audit_retention_days: i32,
}

/// 閲覧の申請（承認レコード）。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, sqlx::FromRow)]
pub struct AccessGrant {
    pub id: Uuid,
    pub channel_id: Uuid,
    /// 対象チャンネルの名前（DM は 2 人の表示名・承認者が判断するため。一覧でだけ埋める）。
    #[sqlx(default)]
    #[serde(default)]
    pub channel_name: String,
    pub requester: String,
    pub reason: String,
    pub messages_from: DateTime<Utc>,
    pub messages_until: DateTime<Utc>,
    pub valid_days: i32,
    pub status: String,
    pub approver: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub valid_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// 申請の入力。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AccessRequest {
    pub channel_id: Uuid,
    pub reason: String,
    pub messages_from: DateTime<Utc>,
    pub messages_until: DateTime<Utc>,
    /// 閲覧できる日数（既定 7・最長 30）。
    #[serde(default)]
    pub valid_days: Option<i32>,
}

/// 監査担当者から見たチャンネル（本文は含まない）。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdminChannel {
    pub id: Uuid,
    pub kind: ChannelKind,
    /// 公開・非公開はチャンネル名、DM は 2 人の表示名。
    pub name: String,
    pub archived: bool,
    pub created_at: DateTime<Utc>,
}

/// 監査担当者の閲覧の 1 発言（編集前・削除前の本文も含む）。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdminMessage {
    pub id: Uuid,
    pub parent_id: Option<Uuid>,
    pub author_kind: String,
    pub author_id: String,
    pub author_name: String,
    pub blocks: Vec<ContentBlock>,
    pub created_at: DateTime<Utc>,
    pub edited_at: Option<DateTime<Utc>>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub deleted_by: Option<String>,
    pub expired: bool,
    /// 編集前・削除前の本文（古い順）。
    pub revisions: Vec<AdminRevision>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdminRevision {
    pub rev: i32,
    pub reason: String,
    pub blocks: Vec<ContentBlock>,
    pub recorded_at: DateTime<Utc>,
}

const GRANT_COLS: &str =
    "id, channel_id, requester, reason, messages_from, messages_until, valid_days, \
    status, approver, decided_at, valid_until, created_at";

impl MessagingService {
    async fn require_org_admin(&self, ctx: &AuthContext) -> Result<(), Error> {
        self.require_member(ctx).await?;
        if authz::is_org_admin(self.authz.as_ref(), ctx, Consistency::HigherConsistency).await? {
            Ok(())
        } else {
            Err(Error::Forbidden("組織の管理者だけが操作できます"))
        }
    }

    async fn require_auditor(&self, ctx: &AuthContext) -> Result<(), Error> {
        self.require_member(ctx).await?;
        let ok = self
            .authz
            .check(
                &ctx.subject(),
                Relation::AuditViewer,
                &ctx.ns().organization(ctx.org()),
                Consistency::HigherConsistency,
            )
            .await?;
        if ok {
            Ok(())
        } else {
            Err(Error::Forbidden("監査担当者だけが操作できます"))
        }
    }

    /// 組織の設定（組織の管理者）。
    pub async fn settings(&self, ctx: &AuthContext) -> Result<MessagingSettings, Error> {
        self.require_org_admin(ctx).await?;
        let audit_days = self.audit_retention_days(ctx).await?;
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let s = store::settings(&mut tx, ctx).await?;
        tx.commit().await?;
        Ok(to_view(s, audit_days))
    }

    async fn audit_retention_days(&self, ctx: &AuthContext) -> Result<i32, Error> {
        Ok(
            storage::audit_retention::AuditRetention::new(self.pool.clone())
                .retention_days(ctx.tenant_id())
                .await?,
        )
    }

    /// 設定を変える（組織の管理者）。保存期間は監査ログの保存期間より短くできない。
    pub async fn update_settings(
        &self,
        ctx: &AuthContext,
        input: MessagingSettings,
        trace: Option<&str>,
    ) -> Result<MessagingSettings, Error> {
        self.require_org_admin(ctx).await?;
        if !matches!(
            input.public_channel_creators.as_str(),
            "everyone" | "admins"
        ) {
            return Err(Error::Invalid(
                "公開チャンネルを作れる人の指定が不正です".into(),
            ));
        }
        let audit_days = self.audit_retention_days(ctx).await?;
        if let Some(days) = input.retention_days {
            if days < audit_days {
                return Err(Error::Invalid(format!(
                    "発言の保存期間は監査ログの保存期間（{audit_days} 日）より短くできません"
                )));
            }
            if days > 36_500 {
                return Err(Error::Invalid("保存期間が長すぎます".into()));
            }
        }
        let mut approvers: Vec<String> = Vec::new();
        for a in input.access_approvers {
            if !approvers.contains(&a) {
                self.require_org_user(ctx, &a).await?;
                approvers.push(a);
            }
        }
        if approvers.len() > 20 {
            return Err(Error::Invalid("承認者は 20 人までです".into()));
        }
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let rev = store::put_settings(
            &mut tx,
            ctx,
            &SettingsRow {
                public_channel_creators: input.public_channel_creators.clone(),
                retention_days: input.retention_days,
                access_approvers: approvers.clone(),
                rev: input.rev,
            },
        )
        .await?;
        record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: AuditAction::MessagingSettingsUpdate,
                object_type: "organization",
                object_id: ctx.org(),
                decision: Decision::Allow,
                trace_id: trace,
                metadata: serde_json::json!({
                    "public_channel_creators": input.public_channel_creators,
                    "retention_days": input.retention_days,
                    "access_approvers": approvers,
                    "rev": rev,
                }),
            },
            Chain::Yes,
        )
        .await?;
        tx.commit().await?;
        self.settings(ctx).await
    }

    /// 監査担当者から見たチャンネルの一覧（名前と種別だけ・本文は読まない）。
    pub async fn admin_channels(
        &self,
        ctx: &AuthContext,
        query: &str,
        trace: Option<&str>,
    ) -> Result<Vec<AdminChannel>, Error> {
        self.require_auditor(ctx).await?;
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let pattern = format!("%{}%", storage::directory::escape_like(query.trim()));
        let rows: Vec<ChannelRow> = sqlx::query_as(
            "SELECT c.id, c.kind, c.name, c.topic, c.announcement, c.dm_key, c.created_by, c.created_at, \
               c.archived_at FROM channel c WHERE c.tenant_id = $1 AND c.org = $2 \
               AND (c.name ILIKE $3 ESCAPE '\\' OR (c.kind = 'dm' AND EXISTS (SELECT 1 FROM channel_member m \
                 JOIN directory_user u ON u.tenant_id = m.tenant_id AND u.org = m.org AND u.user_id = m.user_id \
                 WHERE m.tenant_id = c.tenant_id AND m.org = c.org AND m.channel_id = c.id \
                 AND u.display_name ILIKE $3 ESCAPE '\\'))) \
             ORDER BY c.kind, lower(COALESCE(c.name, '')), c.created_at LIMIT 200",
        )
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(&pattern)
        .fetch_all(&mut *tx)
        .await?;
        let mut out = Vec::new();
        for c in rows {
            out.push(AdminChannel {
                id: c.id,
                kind: c.kind(),
                name: admin_channel_name(&mut tx, ctx, &c).await?,
                archived: c.archived_at.is_some(),
                created_at: c.created_at,
            });
        }
        audit_access(
            &mut tx,
            ctx,
            AuditAction::MessagingAdminRead,
            "organization",
            ctx.org(),
            trace,
            serde_json::json!({ "op": "list_channels", "hits": out.len() }),
        )
        .await?;
        tx.commit().await?;
        Ok(out)
    }

    /// DM・非公開チャンネルの閲覧を申請する（監査担当者）。
    pub async fn request_access(
        &self,
        ctx: &AuthContext,
        input: AccessRequest,
        trace: Option<&str>,
    ) -> Result<AccessGrant, Error> {
        self.require_auditor(ctx).await?;
        let reason = input.reason.trim().to_string();
        if reason.is_empty() || reason.chars().count() > 2000 {
            return Err(Error::Invalid("理由は 1〜2000 文字で書いてください".into()));
        }
        if input.messages_from >= input.messages_until {
            return Err(Error::Invalid("発言の期間が不正です".into()));
        }
        let days = input.valid_days.unwrap_or(DEFAULT_ACCESS_DAYS);
        if !(1..=MAX_ACCESS_DAYS).contains(&days) {
            return Err(Error::Invalid(format!(
                "閲覧できる期間は 1〜{MAX_ACCESS_DAYS} 日です"
            )));
        }
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let c = store::channel(&mut tx, ctx, input.channel_id)
            .await?
            .ok_or(Error::NotFound)?;
        if c.kind() == ChannelKind::Public {
            return Err(Error::Invalid(
                "公開チャンネルは申請なしで閲覧できます".into(),
            ));
        }
        let id = Uuid::new_v4();
        let grant: AccessGrant = sqlx::query_as(&format!(
            "INSERT INTO messaging_access_grant (tenant_id, org, id, channel_id, requester, reason, \
               messages_from, messages_until, valid_days) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
             RETURNING {GRANT_COLS}"
        ))
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(id)
        .bind(c.id)
        .bind(&ctx.principal().id)
        .bind(&reason)
        .bind(input.messages_from)
        .bind(input.messages_until)
        .bind(days)
        .fetch_one(&mut *tx)
        .await?;
        audit_access(
            &mut tx,
            ctx,
            AuditAction::MessagingAccessRequest,
            "messaging_access_grant",
            &id.to_string(),
            trace,
            serde_json::json!({
                "channel_id": c.id, "kind": c.kind().as_str(), "reason": reason,
                "messages_from": input.messages_from, "messages_until": input.messages_until,
                "valid_days": days,
            }),
        )
        .await?;
        tx.commit().await?;
        Ok(grant)
    }

    /// 申請の一覧（監査担当者は自分の申請、承認者は全件）。
    pub async fn list_grants(&self, ctx: &AuthContext) -> Result<Vec<AccessGrant>, Error> {
        self.require_member(ctx).await?;
        let me = ctx.principal().id.clone();
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let settings = store::settings(&mut tx, ctx).await?;
        let approver = settings.access_approvers.contains(&me);
        if !approver {
            self.require_auditor(ctx).await?;
        }
        let mut rows: Vec<AccessGrant> = sqlx::query_as(&format!(
            "SELECT {GRANT_COLS} FROM messaging_access_grant WHERE tenant_id = $1 AND org = $2 \
               AND ($3 OR requester = $4) ORDER BY created_at DESC LIMIT 200"
        ))
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(approver)
        .bind(&me)
        .fetch_all(&mut *tx)
        .await?;
        for g in &mut rows {
            if let Some(c) = store::channel(&mut tx, ctx, g.channel_id).await? {
                g.channel_name = admin_channel_name(&mut tx, ctx, &c).await?;
            }
        }
        tx.commit().await?;
        Ok(rows)
    }

    /// 承認・却下する（テナントが指名した承認者だけ・申請者本人は不可）。
    pub async fn decide_access(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        approve: bool,
        trace: Option<&str>,
    ) -> Result<AccessGrant, Error> {
        self.require_member(ctx).await?;
        let me = ctx.principal().id.clone();
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let settings = store::settings(&mut tx, ctx).await?;
        if !settings.access_approvers.contains(&me) {
            return Err(Error::Forbidden("閲覧の承認者に指名されていません"));
        }
        let grant = grant_for_update(&mut tx, ctx, id).await?;
        if grant.requester == me {
            return Err(Error::Forbidden("自分の申請は承認できません"));
        }
        if grant.status != "pending" {
            return Err(Error::Conflict("この申請はすでに判断済みです"));
        }
        let grant: AccessGrant = sqlx::query_as(&format!(
            "UPDATE messaging_access_grant SET status = $4, approver = $5, decided_at = now(), \
               valid_until = CASE WHEN $4 = 'approved' THEN now() + make_interval(days => valid_days) END \
             WHERE tenant_id = $1 AND org = $2 AND id = $3 RETURNING {GRANT_COLS}"
        ))
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(id)
        .bind(if approve { "approved" } else { "rejected" })
        .bind(&me)
        .fetch_one(&mut *tx)
        .await?;
        audit_access(
            &mut tx,
            ctx,
            if approve {
                AuditAction::MessagingAccessApprove
            } else {
                AuditAction::MessagingAccessReject
            },
            "messaging_access_grant",
            &id.to_string(),
            trace,
            serde_json::json!({ "channel_id": grant.channel_id, "requester": grant.requester,
                "valid_until": grant.valid_until }),
        )
        .await?;
        tx.commit().await?;
        Ok(grant)
    }

    /// 承認を取り消す（承認者か申請者）。
    pub async fn revoke_access(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        trace: Option<&str>,
    ) -> Result<AccessGrant, Error> {
        self.require_member(ctx).await?;
        let me = ctx.principal().id.clone();
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let settings = store::settings(&mut tx, ctx).await?;
        let grant = grant_for_update(&mut tx, ctx, id).await?;
        if grant.requester != me && !settings.access_approvers.contains(&me) {
            return Err(Error::Forbidden("取り消せるのは申請者か承認者だけです"));
        }
        let grant: AccessGrant = sqlx::query_as(&format!(
            "UPDATE messaging_access_grant SET status = 'revoked', \
               valid_until = LEAST(COALESCE(valid_until, now()), now()) \
             WHERE tenant_id = $1 AND org = $2 AND id = $3 RETURNING {GRANT_COLS}"
        ))
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        audit_access(
            &mut tx,
            ctx,
            AuditAction::MessagingAccessRevoke,
            "messaging_access_grant",
            &id.to_string(),
            trace,
            serde_json::json!({ "channel_id": grant.channel_id }),
        )
        .await?;
        tx.commit().await?;
        Ok(grant)
    }

    /// 監査担当者の閲覧（`export = true` はエクスポートとして監査に残す）。
    ///
    /// 公開チャンネルは承認不要。DM・非公開は自分が申請して承認された、期間内の承認レコードが要り、
    /// 読める発言はその承認レコードの発言の期間に限る。
    pub async fn admin_read(
        &self,
        ctx: &AuthContext,
        channel: Uuid,
        from: DateTime<Utc>,
        until: DateTime<Utc>,
        export: bool,
        trace: Option<&str>,
    ) -> Result<Vec<AdminMessage>, Error> {
        self.require_auditor(ctx).await?;
        if from >= until {
            return Err(Error::Invalid("発言の期間が不正です".into()));
        }
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let c = store::channel(&mut tx, ctx, channel)
            .await?
            .ok_or(Error::NotFound)?;
        let (from, until, grant) = if c.kind() == ChannelKind::Public {
            (from, until, None)
        } else {
            // 承認者が今は未指名なら、過去の承認があっても閲覧させない（fail-closed）。
            if store::settings(&mut tx, ctx)
                .await?
                .access_approvers
                .is_empty()
            {
                drop(tx);
                self.deny(ctx, "channel", channel, "admin_read", trace)
                    .await;
                return Err(Error::Forbidden(
                    "閲覧の承認者が指名されていないため、DM・非公開チャンネルは閲覧できません",
                ));
            }
            let grant: Option<AccessGrant> = sqlx::query_as(&format!(
                "SELECT {GRANT_COLS} FROM messaging_access_grant WHERE tenant_id = $1 AND org = $2 \
                   AND channel_id = $3 AND requester = $4 AND status = 'approved' \
                   AND approver IS NOT NULL AND approver <> requester AND valid_until > now() \
                   AND messages_from <= $5 AND messages_until >= $6 \
                 ORDER BY valid_until DESC LIMIT 1"
            ))
            .bind(ctx.tenant_id())
            .bind(ctx.org())
            .bind(channel)
            .bind(&ctx.principal().id)
            .bind(from)
            .bind(until)
            .fetch_optional(&mut *tx)
            .await?;
            let Some(grant) = grant else {
                drop(tx);
                self.deny(ctx, "channel", channel, "admin_read", trace)
                    .await;
                return Err(Error::Forbidden(
                    "指定の期間全体を閲覧できる承認がありません（承認の期間・範囲の外を含む）",
                ));
            };
            // 範囲は承認が丸ごと覆うものだけ通す（黙って縮めて一部を欠落させない）。
            (from, until, Some(grant.id))
        };
        let messages = admin_messages(&mut tx, ctx, channel, from, until).await?;
        audit_access(
            &mut tx,
            ctx,
            if export {
                AuditAction::MessagingAdminExport
            } else {
                AuditAction::MessagingAdminRead
            },
            "channel",
            &channel.to_string(),
            trace,
            serde_json::json!({
                "kind": c.kind().as_str(), "grant": grant, "from": from, "until": until,
                "messages": messages.len(),
            }),
        )
        .await?;
        tx.commit().await?;
        Ok(messages)
    }
}

async fn grant_for_update(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    id: Uuid,
) -> Result<AccessGrant, Error> {
    sqlx::query_as(&format!(
        "SELECT {GRANT_COLS} FROM messaging_access_grant WHERE tenant_id = $1 AND org = $2 AND id = $3 \
         FOR UPDATE"
    ))
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(id)
    .fetch_optional(conn)
    .await?
    .ok_or(Error::NotFound)
}

async fn admin_channel_name(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    c: &ChannelRow,
) -> Result<String, Error> {
    if let Some(n) = &c.name {
        return Ok(n.clone());
    }
    if let Some(owner) = c.agent_dm_owner() {
        let names = store::display_names(conn, ctx, &[owner.to_string()]).await?;
        let who = names
            .get(owner)
            .cloned()
            .unwrap_or_else(|| owner.to_string());
        return Ok(format!("{who} とエージェントの DM"));
    }
    let users: Vec<String> = c
        .dm_key
        .as_deref()
        .and_then(|k| k.split_once(store::DM_SEP))
        .map(|(a, b)| vec![a.to_string(), b.to_string()])
        .unwrap_or_default();
    let names = store::display_names(conn, ctx, &users).await?;
    Ok(users
        .iter()
        .map(|u| names.get(u).cloned().unwrap_or_else(|| u.clone()))
        .collect::<Vec<_>>()
        .join(" ↔ "))
}

/// 監査担当者の閲覧専用の読取（境界も削除の秘匿も掛けない・通常の経路からは呼ばない）。
async fn admin_messages(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    channel: Uuid,
    from: DateTime<Utc>,
    until: DateTime<Utc>,
) -> Result<Vec<AdminMessage>, Error> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: Uuid,
        parent_id: Option<Uuid>,
        author_kind: String,
        author_id: String,
        author_name: Option<String>,
        blocks: sqlx::types::Json<Vec<ContentBlock>>,
        created_at: DateTime<Utc>,
        edited_at: Option<DateTime<Utc>>,
        deleted_at: Option<DateTime<Utc>>,
        deleted_by: Option<String>,
        expired_at: Option<DateTime<Utc>>,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT m.id, m.parent_id, m.author_kind, m.author_id, u.display_name AS author_name, m.blocks, \
           m.created_at, m.edited_at, m.deleted_at, m.deleted_by, m.expired_at \
         FROM channel_message m LEFT JOIN directory_user u ON u.tenant_id = m.tenant_id AND u.org = m.org \
           AND u.user_id = m.author_id \
         WHERE m.tenant_id = $1 AND m.org = $2 AND m.channel_id = $3 AND m.created_at >= $4 \
           AND m.created_at < $5 ORDER BY m.created_at, m.id LIMIT $6",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(channel)
    .bind(from)
    .bind(until)
    .bind(ADMIN_READ_LIMIT + 1)
    .fetch_all(&mut *conn)
    .await?;
    // 上限を超えたら黙って切らずに失敗させる（期間全体を取得したと誤認させない）。
    if rows.len() as i64 > ADMIN_READ_LIMIT {
        return Err(Error::Invalid(format!(
            "期間内の発言が {ADMIN_READ_LIMIT} 件を超えています。期間を絞ってください"
        )));
    }
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let revs: Vec<RevisionRow> = sqlx::query_as(
        "SELECT message_id, rev, reason, blocks, recorded_at FROM channel_message_revision \
             WHERE tenant_id = $1 AND org = $2 AND message_id = ANY($3) ORDER BY message_id, rev",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(&ids)
    .fetch_all(conn)
    .await?;
    let mut by_msg: std::collections::HashMap<Uuid, Vec<AdminRevision>> =
        std::collections::HashMap::new();
    for (mid, rev, reason, blocks, at) in revs {
        by_msg.entry(mid).or_default().push(AdminRevision {
            rev,
            reason,
            blocks: blocks.0,
            recorded_at: at,
        });
    }
    Ok(rows
        .into_iter()
        .map(|r| AdminMessage {
            revisions: by_msg.remove(&r.id).unwrap_or_default(),
            id: r.id,
            parent_id: r.parent_id,
            author_kind: r.author_kind,
            author_name: r.author_name.unwrap_or_else(|| r.author_id.clone()),
            author_id: r.author_id,
            blocks: if r.deleted_at.is_some() {
                Vec::new()
            } else {
                r.blocks.0
            },
            created_at: r.created_at,
            edited_at: r.edited_at,
            deleted_at: r.deleted_at,
            deleted_by: r.deleted_by,
            expired: r.expired_at.is_some(),
        })
        .collect())
}

/// 改訂の 1 行（発言 id・版・理由・本文・時刻）。
type RevisionRow = (
    Uuid,
    i32,
    String,
    sqlx::types::Json<Vec<ContentBlock>>,
    DateTime<Utc>,
);

async fn audit_access(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    action: AuditAction,
    object_type: &str,
    object_id: &str,
    trace: Option<&str>,
    metadata: serde_json::Value,
) -> Result<(), Error> {
    record_on(
        conn,
        ctx,
        AuditEntry {
            action,
            object_type,
            object_id,
            decision: Decision::Allow,
            trace_id: trace,
            metadata,
        },
        Chain::Yes,
    )
    .await?;
    Ok(())
}

fn to_view(s: SettingsRow, audit_days: i32) -> MessagingSettings {
    MessagingSettings {
        public_channel_creators: s.public_channel_creators,
        retention_days: s.retention_days,
        access_approvers: s.access_approvers,
        rev: s.rev,
        audit_retention_days: audit_days,
    }
}

/// 申請の期間の妥当性（テストから使う）。
#[must_use]
pub fn valid_access_days(days: i32) -> bool {
    (1..=MAX_ACCESS_DAYS).contains(&days)
}

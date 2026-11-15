//! 参加者の操作（参加・招待・退出・役割・一覧・通知設定・DM の非表示・既読）。
//!
//! 剥奪は FGA から先に行い、購読中の接続へ leave を push する（PIT-64）。

use authz::{AuthContext, CheckTuple, Consistency, Relation};
use sqlx::PgConnection;
use storage::audit::AuditAction;
use storage::db_tenant::begin_tenant_tx;
use uuid::Uuid;

use crate::pubsub::Signal;
use crate::service::audit;
use crate::store::{self, ChannelRow};
use crate::{ChannelKind, ChannelView, Error, MemberView, MessagingService, NotifyLevel};

impl MessagingService {
    /// 公開チャンネルに参加する（`can_join`）。
    pub async fn join(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        trace: Option<&str>,
    ) -> Result<ChannelView, Error> {
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let c = self.readable_channel(&mut tx, ctx, id).await?;
        if c.archived_at.is_some() {
            return Err(Error::Forbidden(
                "アーカイブされたチャンネルには参加できません",
            ));
        }
        if !self.check(ctx, Relation::CanJoin, id).await? {
            self.deny(ctx, "channel", id, "join", trace).await;
            return Err(Error::Forbidden("このチャンネルには招待が必要です"));
        }
        let me = ctx.principal().id.clone();
        self.add_members_on(&mut tx, ctx, &c, &[me], trace).await?;
        tx.commit().await?;
        self.channel_view(ctx, id).await
    }

    /// 招待する。公開チャンネルは参加者なら誰でも、非公開チャンネルは admin だけ。
    pub async fn invite(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        users: Vec<String>,
        trace: Option<&str>,
    ) -> Result<Vec<MemberView>, Error> {
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let c = self.readable_channel(&mut tx, ctx, id).await?;
        if c.kind() == ChannelKind::Dm {
            return Err(Error::Forbidden(
                "DM には招待できません（3 人以上は非公開チャンネルを作ってください）",
            ));
        }
        if c.archived_at.is_some() {
            return Err(Error::Forbidden(
                "アーカイブされたチャンネルには招待できません",
            ));
        }
        let me = ctx.principal().id.clone();
        let allowed = match c.kind() {
            ChannelKind::Public => store::member(&mut tx, ctx, id, &me).await?.is_some(),
            _ => self.check(ctx, Relation::CanManage, id).await?,
        };
        if !allowed {
            self.deny(ctx, "channel", id, "invite", trace).await;
            return Err(Error::Forbidden("このチャンネルに招待する権限がありません"));
        }
        if users.len() > 500 {
            return Err(Error::Invalid("一度に招待できるのは 500 人までです".into()));
        }
        for u in &users {
            self.require_org_user(ctx, u).await?;
        }
        self.add_members_on(&mut tx, ctx, &c, &users, trace).await?;
        tx.commit().await?;
        self.members(ctx, id).await
    }

    async fn add_members_on(
        &self,
        conn: &mut PgConnection,
        ctx: &AuthContext,
        c: &ChannelRow,
        users: &[String],
        trace: Option<&str>,
    ) -> Result<(), Error> {
        let obj = ctx.ns().channel(&c.id.to_string());
        let mut tuples = Vec::new();
        for u in users {
            if store::member(conn, ctx, c.id, u).await?.is_some() {
                continue;
            }
            store::upsert_member(conn, ctx, c.id, u).await?;
            tuples.push((ctx.ns().user(u), Relation::Member, obj.clone()));
        }
        if tuples.is_empty() {
            return Ok(());
        }
        audit(
            conn,
            ctx,
            AuditAction::MessagingMemberAdd,
            c.id,
            trace,
            serde_json::json!({ "users": users }),
        )
        .await?;
        self.write_tuples(&tuples).await?;
        Ok(())
    }

    /// 退出する（自分）または外す（admin）。1 対 1 の DM は退出できず、最後の admin も退出できない。
    pub async fn remove_member(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        user: &str,
        trace: Option<&str>,
    ) -> Result<(), Error> {
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let c = self.readable_channel(&mut tx, ctx, id).await?;
        if c.kind() == ChannelKind::Dm {
            return Err(Error::Forbidden("DM は退出できません（非表示にできます）"));
        }
        let me = ctx.principal().id.clone();
        if user != me && !self.check(ctx, Relation::CanManage, id).await? {
            self.deny(ctx, "channel", id, "remove_member", trace).await;
            return Err(Error::Forbidden("ほかの参加者を外せるのは管理者だけです"));
        }
        let obj = ctx.ns().channel(&id.to_string());
        let admins = self.admin_ids(&obj).await?;
        let target = ctx.ns().user(user).to_string();
        if admins.contains(&target) && admins.len() <= 1 {
            return Err(Error::Forbidden(
                "最後の管理者は退出できません（先にほかの人を管理者にしてください）",
            ));
        }
        store::delete_member(&mut tx, ctx, id, user).await?;
        audit(
            &mut tx,
            ctx,
            AuditAction::MessagingMemberRemove,
            id,
            trace,
            serde_json::json!({ "user": user, "self": user == me }),
        )
        .await?;
        let subject = ctx.ns().user(user);
        // 剥奪は先に FGA から（DB を巻き戻しても権限は残さない側に倒す）。
        for rel in [Relation::Member, Relation::Admin, Relation::Poster] {
            self.authz.delete_tuple(&subject, rel, &obj).await?;
        }
        tx.commit().await?;
        // 購読中の接続へ leave を push する（PIT-64）。取りこぼしても購読の再評価 TTL で閉じる。
        self.hub
            .publish(
                ctx.tenant_id(),
                id,
                Signal::Leave {
                    user_id: user.to_string(),
                },
            )
            .await;
        Ok(())
    }

    async fn admin_ids(&self, obj: &authz::FgaObject) -> Result<Vec<String>, Error> {
        let tuples = self.authz.read_tuples(obj, Some(Relation::Admin)).await?;
        Ok(tuples.into_iter().map(|t| t.user).collect())
    }

    /// admin・投稿者（アナウンス専用）を付け外しする（admin）。
    pub async fn set_member_role(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        user: &str,
        admin: Option<bool>,
        poster: Option<bool>,
        trace: Option<&str>,
    ) -> Result<Vec<MemberView>, Error> {
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let c = self.managed_channel(&mut tx, ctx, id, trace).await?;
        if store::member(&mut tx, ctx, id, user).await?.is_none() {
            return Err(Error::Invalid("参加していない利用者です".into()));
        }
        let obj = ctx.ns().channel(&id.to_string());
        let subject = ctx.ns().user(user);
        if let Some(admin) = admin {
            if admin {
                self.authz
                    .write_tuple(&subject, Relation::Admin, &obj)
                    .await?;
            } else {
                let admins = self.admin_ids(&obj).await?;
                if admins.len() <= 1 {
                    return Err(Error::Forbidden("最後の管理者は外せません"));
                }
                self.authz
                    .delete_tuple(&subject, Relation::Admin, &obj)
                    .await?;
            }
        }
        if let Some(poster) = poster {
            if !c.announcement {
                return Err(Error::Invalid(
                    "投稿者を個別に決めるのはアナウンス専用チャンネルだけです".into(),
                ));
            }
            if poster {
                self.authz
                    .write_tuple(&subject, Relation::Poster, &obj)
                    .await?;
            } else {
                self.authz
                    .delete_tuple(&subject, Relation::Poster, &obj)
                    .await?;
            }
        }
        audit(
            &mut tx,
            ctx,
            AuditAction::MessagingMemberRole,
            id,
            trace,
            serde_json::json!({ "user": user, "admin": admin, "poster": poster }),
        )
        .await?;
        tx.commit().await?;
        self.members(ctx, id).await
    }

    /// 参加者の一覧。
    pub async fn members(&self, ctx: &AuthContext, id: Uuid) -> Result<Vec<MemberView>, Error> {
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let c = self.readable_channel(&mut tx, ctx, id).await?;
        let rows = store::members(&mut tx, ctx, id).await?;
        let ids: Vec<String> = rows.iter().map(|r| r.user_id.clone()).collect();
        let names = store::display_names(&mut tx, ctx, &ids).await?;
        tx.commit().await?;
        let obj = ctx.ns().channel(&id.to_string());
        let admins = if c.kind() == ChannelKind::Dm {
            Vec::new()
        } else {
            self.admin_ids(&obj).await?
        };
        let posters: Vec<String> = if c.announcement {
            self.authz
                .read_tuples(&obj, Some(Relation::Poster))
                .await?
                .into_iter()
                .map(|t| t.user)
                .collect()
        } else {
            Vec::new()
        };
        let active = self.active_users(ctx, &ids).await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let subject = ctx.ns().user(&r.user_id).to_string();
                MemberView {
                    display_name: names.get(&r.user_id).cloned().unwrap_or_default(),
                    is_admin: admins.contains(&subject),
                    can_post: !c.announcement || posters.contains(&subject),
                    active: active.get(&r.user_id).copied().unwrap_or(false),
                    joined_at: r.visible_from,
                    user_id: r.user_id,
                }
            })
            .collect())
    }

    /// 利用者が組織のメンバーのままか（無効化された人の発言・参加者に「（無効）」を付ける）。
    pub(crate) async fn active_users(
        &self,
        ctx: &AuthContext,
        ids: &[String],
    ) -> Result<std::collections::HashMap<String, bool>, Error> {
        let org = ctx.ns().organization(ctx.org());
        let items: Vec<CheckTuple> = ids
            .iter()
            .map(|u| CheckTuple {
                subject: ctx.ns().user(u),
                relation: Relation::Member,
                object: org.clone(),
            })
            .collect();
        let ok = self
            .authz
            .batch_check(&items, Consistency::MinimizeLatency)
            .await?;
        Ok(ids.iter().cloned().zip(ok).collect())
    }

    /// 通知の受け方を変える。
    pub async fn set_notify(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        level: NotifyLevel,
    ) -> Result<(), Error> {
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        self.readable_channel(&mut tx, ctx, id).await?;
        if !store::set_notify(&mut tx, ctx, id, &ctx.principal().id, level).await? {
            return Err(Error::Forbidden("参加していないチャンネルです"));
        }
        tx.commit().await?;
        Ok(())
    }

    /// DM を一覧から隠す（新しい発言が来たら戻る）。
    pub async fn hide_dm(&self, ctx: &AuthContext, id: Uuid) -> Result<(), Error> {
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let c = self.readable_channel(&mut tx, ctx, id).await?;
        if c.kind() != ChannelKind::Dm {
            return Err(Error::Invalid("非表示にできるのは DM だけです".into()));
        }
        store::set_hidden(&mut tx, ctx, id, &ctx.principal().id, true).await?;
        tx.commit().await?;
        Ok(())
    }

    /// 既読位置を進める。
    pub async fn mark_read(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), Error> {
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        self.readable_channel(&mut tx, ctx, id).await?;
        let me = ctx.principal().id.clone();
        if store::member(&mut tx, ctx, id, &me).await?.is_some() {
            let at = at.min(chrono::Utc::now());
            store::mark_read(&mut tx, ctx, id, &me, at).await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

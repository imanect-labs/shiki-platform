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
        self.require_org_users(ctx, &users).await?;
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
        // 公開返信の書込（参加の境界の確かめ）と直列化する。
        store::lock_channel(conn, ctx, c.id).await?;
        // 新しく加わった人だけ（既に参加している人の境界 visible_from は動かさない）。
        let added = store::add_members(conn, ctx, c.id, users).await?;
        // member タプルは指定された全員に書く（冪等）。DB の行が残っているのにタプルが欠けた状態
        // （退出のタプル剥奪の後に DB のコミットが失敗した）も、参加し直せば直る。
        let tuples: Vec<_> = users
            .iter()
            .map(|u| (ctx.ns().user(u), Relation::Member, obj.clone()))
            .collect();
        if tuples.is_empty() {
            return Ok(());
        }
        if !added.is_empty() {
            audit(
                conn,
                ctx,
                AuditAction::MessagingMemberAdd,
                c.id,
                trace,
                serde_json::json!({ "users": added }),
            )
            .await?;
        }
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
        store::lock_channel(&mut tx, ctx, id).await?;
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
        // 管理者の数の判定・タプルの書込・失敗時の巻き戻しまでを、チャンネル単位で 1 つの区間にする。
        // トランザクションのロックは rollback で外れるので、DB の失敗の後の巻き戻しが後続の役割変更と
        // 前後して、後続の剥奪を覆しうる。セッションのロックは巻き戻しが終わるまで持つ。
        // 接続を持ったまま待たないよう、トランザクションを開く前に取る。
        let lock = store::lock_channel_session(&self.pool, ctx, id).await?;
        let result = self
            .apply_member_role(ctx, id, user, admin, poster, trace)
            .await;
        lock.release().await;
        result?;
        self.members(ctx, id).await
    }

    async fn apply_member_role(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        user: &str,
        admin: Option<bool>,
        poster: Option<bool>,
        trace: Option<&str>,
    ) -> Result<(), Error> {
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let c = self.managed_channel(&mut tx, ctx, id, trace).await?;
        // 入力の検証を書込より先に済ませる（途中で失敗して権限だけ変わる状態を作らない）。
        if poster.is_some() && !c.announcement {
            return Err(Error::Invalid(
                "投稿者を個別に決めるのはアナウンス専用チャンネルだけです".into(),
            ));
        }
        let obj = ctx.ns().channel(&id.to_string());
        let subject = ctx.ns().user(user);
        // ここから先は set_member_role が持つチャンネルのロックの内側（同時に外して管理者 0 人にしない）。
        // 参加の確認は除名と同じロックの内側で行う（除名と並行して権限が復活しないように）。
        if store::member(&mut tx, ctx, id, user).await?.is_none() {
            return Err(Error::Invalid("参加していない利用者です".into()));
        }
        // 操作者自身の管理権もロックの内側で引き直す（待っている間に外されていたら変えない）。
        if !self.check(ctx, Relation::CanManage, id).await? {
            return Err(Error::Forbidden("チャンネルの管理者だけが操作できます"));
        }
        // 今の状態を先に読み、変わらない指定は書かない。付与の通信エラーで巻き戻すのは
        // 「今回付けたかもしれない」タプルだけにするため（既に admin の人を admin=true にして
        // 通信エラーになっても、元からある admin を剥がさない＝最後の管理者を失わない）。
        let target = subject.to_string();
        let admin = match admin {
            Some(on) => {
                let admins = self.admin_ids(&obj).await?;
                let has = admins.contains(&target);
                if !on && has && admins.len() <= 1 {
                    return Err(Error::Forbidden("最後の管理者は外せません"));
                }
                (on != has).then_some(on)
            }
            None => None,
        };
        let poster = match poster {
            Some(on) => {
                let has = self
                    .authz
                    .read_tuples(&obj, Some(Relation::Poster))
                    .await?
                    .iter()
                    .any(|t| t.user == target);
                (on != has).then_some(on)
            }
            None => None,
        };
        audit(
            &mut tx,
            ctx,
            AuditAction::MessagingMemberRole,
            id,
            trace,
            serde_json::json!({ "user": user, "admin": admin, "poster": poster }),
        )
        .await?;
        // 付け外しが途中で失敗したら、変えた分を元に戻して失敗を返す（監査の無い変更を残さない）。
        let mut applied: Vec<(Relation, bool)> = Vec::new();
        let mut failed = None;
        for (flag, rel) in [(admin, Relation::Admin), (poster, Relation::Poster)] {
            let Some(on) = flag else { continue };
            let r = if on {
                self.authz.write_tuple(&subject, rel, &obj).await
            } else {
                self.authz.delete_tuple(&subject, rel, &obj).await
            };
            match r {
                Ok(true) => applied.push((rel, on)),
                Ok(false) => {}
                Err(e) => {
                    // 付与の通信エラーは適用されたか分からないので、剥がす側に倒す（fail-closed・
                    // share_link_redeem と同じ扱い）。剥奪のエラーは権限が残らない方向なので戻さない。
                    if on {
                        applied.push((rel, on));
                    }
                    failed = Some(Error::from(e));
                    break;
                }
            }
        }
        let result = match failed {
            Some(e) => Err(e),
            None => tx.commit().await.map_err(Error::from),
        };
        if let Err(e) = result {
            for (rel, on) in applied {
                let undo = if on {
                    self.authz.delete_tuple(&subject, rel, &obj).await
                } else {
                    self.authz.write_tuple(&subject, rel, &obj).await
                };
                if let Err(err) = undo {
                    tracing::error!(error = %err, "messaging: 役割変更の巻き戻しに失敗（タプルが残る）");
                }
            }
            return Err(e);
        }
        Ok(())
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

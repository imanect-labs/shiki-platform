//! チャンネルの操作（作成・参加・招待・退出・アーカイブ・DM・通知設定）。
//!
//! 認可は OpenFGA の `channel` 型（design §4.14 認可）を **[`AuthzClient`] だけで**判定する
//! （単一チョークポイント）。DB の `channel_member` は表示・未読・参加前発言の境界のための写しで、
//! タプルと同じトランザクションの中で書き、タプルの書込に失敗したら DB を巻き戻す。

use authz::{AuthContext, AuthzClient, CheckTuple, Consistency, PrincipalKind, Relation, Subject};
use sqlx::{PgConnection, PgPool};
use std::sync::Arc;
use storage::audit::{record_on, AuditAction, AuditEntry, Chain, Decision};
use storage::db_tenant::begin_tenant_tx;
use uuid::Uuid;

use crate::files::FileAccess;
use crate::pubsub::{Hub, Signal};
use crate::store::{self, ChannelRow, AGENT_DM_PREFIX};
use crate::{ChannelKind, ChannelView, CreateChannel, Error, NotifyLevel, UpdateChannel};

/// 職員間メッセージのサービス（認可・トランザクション・監査・配信の合図を持つ）。
pub struct MessagingService {
    pub(crate) pool: PgPool,
    pub(crate) authz: Arc<dyn AuthzClient>,
    pub(crate) hub: Hub,
    pub(crate) files: Arc<dyn FileAccess>,
    pub(crate) previews: Option<Arc<crate::preview::LinkPreviewer>>,
}

/// FGA の 1 タプル（書込・補償用）。
pub(crate) type Tuple = (Subject, Relation, authz::FgaObject);

impl MessagingService {
    pub fn new(
        pool: PgPool,
        authz: Arc<dyn AuthzClient>,
        hub: Hub,
        files: Arc<dyn FileAccess>,
    ) -> Self {
        Self {
            pool,
            authz,
            hub,
            files,
            previews: None,
        }
    }

    /// リンクのプレビューを有効にする（外部接続のある構成だけ・Task 14.19）。
    #[must_use]
    pub fn with_link_previews(mut self, previewer: crate::preview::LinkPreviewer) -> Self {
        self.previews = Some(Arc::new(previewer));
        self
    }

    /// 配信ハブ（SSE の購読に使う）。
    pub fn hub(&self) -> &Hub {
        &self.hub
    }

    // ---- 認可 ----

    /// 組織の利用者か（外部・匿名・ミニアプリ等の principal は messaging に入れない・design §4.14 外部ユーザー）。
    pub(crate) async fn require_member(&self, ctx: &AuthContext) -> Result<(), Error> {
        if ctx.principal().kind != PrincipalKind::User {
            return Err(Error::Forbidden("利用者だけが使えます"));
        }
        let ok = self
            .authz
            .check(
                &ctx.subject(),
                Relation::Member,
                &ctx.ns().organization(ctx.org()),
                Consistency::MinimizeLatency,
            )
            .await?;
        if ok {
            Ok(())
        } else {
            Err(Error::Forbidden("組織のメンバーではありません"))
        }
    }

    pub(crate) async fn check(
        &self,
        ctx: &AuthContext,
        relation: Relation,
        channel: Uuid,
    ) -> Result<bool, Error> {
        Ok(self
            .authz
            .check(
                &ctx.subject(),
                relation,
                &ctx.ns().channel(&channel.to_string()),
                Consistency::HigherConsistency,
            )
            .await?)
    }

    /// 読めるチャンネルを引く（読めなければ存在ごと隠す = NotFound）。
    pub(crate) async fn readable_channel(
        &self,
        conn: &mut PgConnection,
        ctx: &AuthContext,
        channel: Uuid,
    ) -> Result<ChannelRow, Error> {
        self.require_member(ctx).await?;
        let row = store::channel(conn, ctx, channel)
            .await?
            .ok_or(Error::NotFound)?;
        if !self.check(ctx, Relation::CanRead, channel).await? {
            return Err(Error::NotFound);
        }
        Ok(row)
    }

    /// 管理できるチャンネル（読めなければ NotFound・読めても admin でなければ Forbidden）。
    pub(crate) async fn managed_channel(
        &self,
        conn: &mut PgConnection,
        ctx: &AuthContext,
        channel: Uuid,
        trace: Option<&str>,
    ) -> Result<ChannelRow, Error> {
        let row = self.readable_channel(conn, ctx, channel).await?;
        if row.kind() == ChannelKind::Dm || !self.check(ctx, Relation::CanManage, channel).await? {
            self.deny(ctx, "channel", channel, "manage", trace).await;
            return Err(Error::Forbidden("チャンネルの管理者だけが操作できます"));
        }
        Ok(row)
    }

    /// 拒否を監査に残す（チェーンに入れない・失敗しても本処理は止めない）。
    pub(crate) async fn deny(
        &self,
        ctx: &AuthContext,
        object_type: &str,
        id: Uuid,
        op: &str,
        trace: Option<&str>,
    ) {
        let Ok(mut tx) = begin_tenant_tx(&self.pool, ctx.tenant()).await else {
            return;
        };
        let entry = AuditEntry {
            action: AuditAction::MessagingDeny,
            object_type,
            object_id: &id.to_string(),
            decision: Decision::Deny,
            trace_id: trace,
            metadata: serde_json::json!({ "op": op }),
        };
        if record_on(&mut tx, ctx, entry, Chain::No).await.is_ok() {
            let _ = tx.commit().await;
        }
    }

    /// 書込のタプルをまとめて付与する。途中で失敗したら付与済みを剥がして失敗を返す。
    pub(crate) async fn write_tuples(&self, tuples: &[Tuple]) -> Result<(), Error> {
        let mut done: Vec<&Tuple> = Vec::new();
        for t in tuples {
            match self.authz.write_tuple(&t.0, t.1, &t.2).await {
                Ok(true) => done.push(t),
                Ok(false) => {}
                Err(e) => {
                    for (s, r, o) in done {
                        let _ = self.authz.delete_tuple(s, *r, o).await;
                    }
                    return Err(e.into());
                }
            }
        }
        Ok(())
    }

    pub(crate) async fn delete_tuples(&self, tuples: &[Tuple]) -> Result<(), Error> {
        for (s, r, o) in tuples {
            self.authz.delete_tuple(s, *r, o).await?;
        }
        Ok(())
    }

    // ---- チャンネル ----

    /// チャンネルを作る（公開・非公開）。作成者が admin になる。
    pub async fn create_channel(
        &self,
        ctx: &AuthContext,
        input: CreateChannel,
        trace: Option<&str>,
    ) -> Result<ChannelView, Error> {
        self.require_member(ctx).await?;
        let name = validate_name(&input.name)?;
        let topic = input.topic.trim().to_string();
        if topic.chars().count() > 250 {
            return Err(Error::Invalid("トピックは 250 文字までです".into()));
        }
        if input.kind == ChannelKind::Dm {
            return Err(Error::Invalid("DM はチャンネルとして作れません".into()));
        }
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        if input.kind == ChannelKind::Public {
            let settings = store::settings(&mut tx, ctx).await?;
            if settings.public_channel_creators == "admins"
                && !authz::is_org_admin(self.authz.as_ref(), ctx, Consistency::HigherConsistency)
                    .await?
            {
                return Err(Error::Forbidden(
                    "公開チャンネルを作れるのは組織の管理者だけです（組織の設定）",
                ));
            }
        }
        let me = ctx.principal().id.clone();
        let mut invitees: Vec<String> = Vec::new();
        for u in input.members {
            if u != me && !invitees.contains(&u) {
                invitees.push(u);
            }
        }
        if invitees.len() > 500 {
            return Err(Error::Invalid("一度に招待できるのは 500 人までです".into()));
        }
        for u in &invitees {
            self.require_org_user(ctx, u).await?;
        }
        let id = Uuid::new_v4();
        let row = ChannelRow {
            id,
            kind: input.kind.as_str().into(),
            name: Some(name),
            topic,
            announcement: input.announcement,
            dm_key: None,
            created_by: me.clone(),
            created_at: chrono::Utc::now(),
            archived_at: None,
        };
        store::insert_channel(&mut tx, ctx, &row).await?;
        store::upsert_member(&mut tx, ctx, id, &me).await?;
        for u in &invitees {
            store::upsert_member(&mut tx, ctx, id, u).await?;
        }
        let ns = ctx.ns();
        let obj = ns.channel(&id.to_string());
        let mut tuples: Vec<Tuple> = vec![
            (
                Subject::object(&ns.organization(ctx.org())),
                Relation::Parent,
                obj.clone(),
            ),
            (ns.user(&me), Relation::Admin, obj.clone()),
            (ns.user(&me), Relation::Member, obj.clone()),
        ];
        if input.announcement {
            tuples.push((ns.user(&me), Relation::Poster, obj.clone()));
        } else {
            tuples.push((
                ns.channel_member(&id.to_string()),
                Relation::Poster,
                obj.clone(),
            ));
        }
        if input.kind == ChannelKind::Public {
            tuples.push((
                ns.organization_member(ctx.org()),
                Relation::OrgReader,
                obj.clone(),
            ));
        }
        for u in &invitees {
            tuples.push((ns.user(u), Relation::Member, obj.clone()));
        }
        audit(
            &mut tx,
            ctx,
            AuditAction::MessagingChannelCreate,
            id,
            trace,
            serde_json::json!({
                "kind": input.kind.as_str(),
                "announcement": input.announcement,
                "invited": invitees.len(),
            }),
        )
        .await?;
        self.write_tuples(&tuples).await?;
        if let Err(e) = tx.commit().await {
            let _ = self.delete_tuples(&tuples).await;
            return Err(e.into());
        }
        self.channel_view(ctx, id).await
    }

    /// 組織の利用者か（招待・DM の相手の検証）。
    pub(crate) async fn require_org_user(
        &self,
        ctx: &AuthContext,
        user: &str,
    ) -> Result<(), Error> {
        if authz::validate_local_id(user).is_err() {
            return Err(Error::Invalid("利用者の指定が不正です".into()));
        }
        let ok = self
            .authz
            .check(
                &ctx.ns().user(user),
                Relation::Member,
                &ctx.ns().organization(ctx.org()),
                Consistency::HigherConsistency,
            )
            .await?;
        if ok {
            Ok(())
        } else {
            Err(Error::Invalid(
                "組織のメンバーではない利用者が含まれています".into(),
            ))
        }
    }

    /// 1 対 1 の DM を開く（同じ 2 人の DM は 1 つだけ・冪等）。
    pub async fn open_dm(
        &self,
        ctx: &AuthContext,
        peer: &str,
        trace: Option<&str>,
    ) -> Result<ChannelView, Error> {
        self.require_member(ctx).await?;
        let me = ctx.principal().id.clone();
        if peer != me {
            self.require_org_user(ctx, peer).await?;
        }
        let key = store::dm_key(&me, peer);
        let id = self
            .ensure_dm(ctx, &key, &[me.as_str(), peer], trace)
            .await?;
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        store::set_hidden(&mut tx, ctx, id, &me, false).await?;
        tx.commit().await?;
        self.channel_view(ctx, id).await
    }

    /// 利用者と自分のパーソナルエージェントの DM（承認の通知先・Task 14.17）。参加者は本人だけ。
    pub async fn agent_dm(&self, ctx: &AuthContext) -> Result<Uuid, Error> {
        self.require_member(ctx).await?;
        let me = ctx.principal().id.clone();
        let key = format!("{AGENT_DM_PREFIX}{me}");
        self.ensure_dm(ctx, &key, &[me.as_str()], None).await
    }

    async fn ensure_dm(
        &self,
        ctx: &AuthContext,
        key: &str,
        users: &[&str],
        trace: Option<&str>,
    ) -> Result<Uuid, Error> {
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        if let Some(row) = store::channel_by_dm_key(&mut tx, ctx, key).await? {
            return Ok(row.id);
        }
        let id = Uuid::new_v4();
        let row = ChannelRow {
            id,
            kind: "dm".into(),
            name: None,
            topic: String::new(),
            announcement: false,
            dm_key: Some(key.to_string()),
            created_by: ctx.principal().id.clone(),
            created_at: chrono::Utc::now(),
            archived_at: None,
        };
        // 同時に開いた 2 回目は一意制約で落ちる → 先に作られた方を返す（冪等）。
        if let Err(Error::Conflict(_)) = store::insert_channel(&mut tx, ctx, &row).await {
            drop(tx);
            let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
            let row = store::channel_by_dm_key(&mut tx, ctx, key)
                .await?
                .ok_or(Error::NotFound)?;
            return Ok(row.id);
        }
        let ns = ctx.ns();
        let obj = ns.channel(&id.to_string());
        let mut tuples: Vec<Tuple> = vec![
            (
                Subject::object(&ns.organization(ctx.org())),
                Relation::Parent,
                obj.clone(),
            ),
            (
                ns.channel_member(&id.to_string()),
                Relation::Poster,
                obj.clone(),
            ),
        ];
        let mut seen: Vec<&str> = Vec::new();
        for u in users {
            if seen.contains(u) {
                continue;
            }
            seen.push(u);
            store::upsert_member(&mut tx, ctx, id, u).await?;
            tuples.push((ns.user(u), Relation::Member, obj.clone()));
        }
        audit(
            &mut tx,
            ctx,
            AuditAction::MessagingDmOpen,
            id,
            trace,
            serde_json::json!({ "agent": key.starts_with(AGENT_DM_PREFIX) }),
        )
        .await?;
        self.write_tuples(&tuples).await?;
        if let Err(e) = tx.commit().await {
            let _ = self.delete_tuples(&tuples).await;
            return Err(e.into());
        }
        Ok(id)
    }

    /// 自分が参加しているチャンネルと DM（非表示の DM は除く）。
    pub async fn list_channels(&self, ctx: &AuthContext) -> Result<Vec<ChannelView>, Error> {
        self.require_member(ctx).await?;
        let me = ctx.principal().id.clone();
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let rows = store::my_channels(&mut tx, ctx, &me).await?;
        // 写しの membership を正本（FGA）で確かめ直す（剥奪の取りこぼしを一覧に出さない）。
        let items: Vec<CheckTuple> = rows
            .iter()
            .flat_map(|r| {
                let obj = ctx.ns().channel(&r.channel.id.to_string());
                [
                    CheckTuple {
                        subject: ctx.subject(),
                        relation: Relation::CanRead,
                        object: obj.clone(),
                    },
                    CheckTuple {
                        subject: ctx.subject(),
                        relation: Relation::CanManage,
                        object: obj.clone(),
                    },
                    CheckTuple {
                        subject: ctx.subject(),
                        relation: Relation::CanPost,
                        object: obj,
                    },
                ]
            })
            .collect();
        let ok = self
            .authz
            .batch_check(&items, Consistency::MinimizeLatency)
            .await?;
        let names = self
            .dm_names(&mut tx, ctx, rows.iter().map(|r| &r.channel))
            .await?;
        tx.commit().await?;
        let mut out = Vec::new();
        for (i, r) in rows.into_iter().enumerate() {
            if !ok[i * 3] || (r.hidden_at.is_some() && r.unread == 0) {
                continue;
            }
            let c = &r.channel;
            out.push(ChannelView {
                id: c.id,
                kind: c.kind(),
                name: display_name(c, &me, &names),
                topic: c.topic.clone(),
                announcement: c.announcement,
                archived: c.archived_at.is_some(),
                joined: true,
                is_admin: ok[i * 3 + 1],
                can_post: ok[i * 3 + 2] && c.archived_at.is_none(),
                dm_peer: c.dm_peer(&me),
                agent_dm: c.agent_dm_owner().is_some(),
                member_count: r.member_count,
                unread: if NotifyLevel::parse(&r.notify) == NotifyLevel::None {
                    0
                } else {
                    r.unread
                },
                mentions: r.mentions,
                notify: NotifyLevel::parse(&r.notify),
                last_message_at: r.last_message_at,
                created_at: c.created_at,
            });
        }
        Ok(out)
    }

    /// 組織の公開チャンネルを探す（参加していなくても読める・参加できる）。
    pub async fn browse_public(
        &self,
        ctx: &AuthContext,
        query: &str,
        include_archived: bool,
    ) -> Result<Vec<ChannelView>, Error> {
        self.require_member(ctx).await?;
        let me = ctx.principal().id.clone();
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let rows = store::public_channels(&mut tx, ctx, query, include_archived, 200).await?;
        let mut views = Vec::new();
        for (c, count) in rows {
            let member = store::member(&mut tx, ctx, c.id, &me).await?;
            views.push(ChannelView {
                id: c.id,
                kind: ChannelKind::Public,
                name: c.name.clone().unwrap_or_default(),
                topic: c.topic.clone(),
                announcement: c.announcement,
                archived: c.archived_at.is_some(),
                joined: member.is_some(),
                is_admin: false,
                can_post: false,
                dm_peer: None,
                agent_dm: false,
                member_count: count,
                unread: 0,
                mentions: 0,
                notify: NotifyLevel::All,
                last_message_at: None,
                created_at: c.created_at,
            });
        }
        tx.commit().await?;
        // 一覧は org_reader の写し（kind = public）から引いたので、FGA で読めるものだけに絞る。
        let items: Vec<CheckTuple> = views
            .iter()
            .map(|v| CheckTuple {
                subject: ctx.subject(),
                relation: Relation::CanRead,
                object: ctx.ns().channel(&v.id.to_string()),
            })
            .collect();
        let ok = self
            .authz
            .batch_check(&items, Consistency::MinimizeLatency)
            .await?;
        Ok(views
            .into_iter()
            .zip(ok)
            .filter_map(|(v, ok)| ok.then_some(v))
            .collect())
    }

    /// 1 つのチャンネル（自分から見た状態）。
    pub async fn channel_view(&self, ctx: &AuthContext, id: Uuid) -> Result<ChannelView, Error> {
        let me = ctx.principal().id.clone();
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let c = self.readable_channel(&mut tx, ctx, id).await?;
        let member = store::member(&mut tx, ctx, id, &me).await?;
        let count = store::members(&mut tx, ctx, id).await?.len() as i64;
        let names = self.dm_names(&mut tx, ctx, std::iter::once(&c)).await?;
        tx.commit().await?;
        let is_admin = self.check(ctx, Relation::CanManage, id).await?;
        let can_post = self.check(ctx, Relation::CanPost, id).await?;
        Ok(ChannelView {
            id,
            kind: c.kind(),
            name: display_name(&c, &me, &names),
            topic: c.topic.clone(),
            announcement: c.announcement,
            archived: c.archived_at.is_some(),
            joined: member.is_some(),
            is_admin,
            can_post: can_post && c.archived_at.is_none(),
            dm_peer: c.dm_peer(&me),
            agent_dm: c.agent_dm_owner().is_some(),
            member_count: count,
            unread: 0,
            mentions: 0,
            notify: member
                .map(|m| NotifyLevel::parse(&m.notify))
                .unwrap_or_default(),
            last_message_at: None,
            created_at: c.created_at,
        })
    }

    /// DM の表示名（相手の表示名・自分のエージェントとの DM はエージェント名）。
    async fn dm_names<'a>(
        &self,
        conn: &mut PgConnection,
        ctx: &AuthContext,
        rows: impl Iterator<Item = &'a ChannelRow>,
    ) -> Result<std::collections::HashMap<String, String>, Error> {
        let me = ctx.principal().id.clone();
        let mut ids: Vec<String> = Vec::new();
        let mut agent = false;
        for r in rows {
            if let Some(p) = r.dm_peer(&me) {
                ids.push(p);
            }
            agent |= r.agent_dm_owner().is_some();
        }
        let mut names = store::display_names(conn, ctx, &ids).await?;
        if agent {
            let name = crate::agent_profile::agent_name(conn, ctx, &me).await?;
            names.insert(format!("{AGENT_DM_PREFIX}{me}"), name);
        }
        Ok(names)
    }

    /// 名前・トピックを変える（admin）。
    pub async fn update_channel(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        input: UpdateChannel,
        trace: Option<&str>,
    ) -> Result<ChannelView, Error> {
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        self.managed_channel(&mut tx, ctx, id, trace).await?;
        let name = input.name.as_deref().map(validate_name).transpose()?;
        let topic = input.topic.as_deref().map(str::trim);
        if topic.is_some_and(|t| t.chars().count() > 250) {
            return Err(Error::Invalid("トピックは 250 文字までです".into()));
        }
        store::update_channel(&mut tx, ctx, id, name.as_deref(), topic).await?;
        audit(
            &mut tx,
            ctx,
            AuditAction::MessagingChannelUpdate,
            id,
            trace,
            serde_json::json!({ "name": name.is_some(), "topic": topic.is_some() }),
        )
        .await?;
        tx.commit().await?;
        self.hub.publish(ctx.tenant_id(), id, Signal::Channel).await;
        self.channel_view(ctx, id).await
    }

    /// アーカイブする／戻す（admin）。アーカイブ中は読み取り専用（投稿・編集・リアクション不可）。
    pub async fn set_archived(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        archived: bool,
        trace: Option<&str>,
    ) -> Result<ChannelView, Error> {
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        self.managed_channel(&mut tx, ctx, id, trace).await?;
        store::set_archived(&mut tx, ctx, id, archived).await?;
        let action = if archived {
            AuditAction::MessagingChannelArchive
        } else {
            AuditAction::MessagingChannelUnarchive
        };
        audit(&mut tx, ctx, action, id, trace, serde_json::json!({})).await?;
        tx.commit().await?;
        self.hub.publish(ctx.tenant_id(), id, Signal::Channel).await;
        self.channel_view(ctx, id).await
    }
}

/// チャンネル名を検証する（前後の空白を落とす・1〜80 文字・制御文字なし）。
pub(crate) fn validate_name(name: &str) -> Result<String, Error> {
    let name = name.trim().trim_start_matches('#').trim();
    let n = name.chars().count();
    if n == 0 || n > 80 {
        return Err(Error::Invalid("チャンネル名は 1〜80 文字です".into()));
    }
    if name.chars().any(char::is_control) {
        return Err(Error::Invalid(
            "チャンネル名に使えない文字が含まれています".into(),
        ));
    }
    Ok(name.to_string())
}

fn display_name(
    c: &ChannelRow,
    me: &str,
    names: &std::collections::HashMap<String, String>,
) -> String {
    if let Some(name) = &c.name {
        return name.clone();
    }
    if let Some(owner) = c.agent_dm_owner() {
        return names
            .get(&format!("{AGENT_DM_PREFIX}{owner}"))
            .cloned()
            .unwrap_or_else(|| crate::DEFAULT_AGENT_NAME.to_string());
    }
    match c.dm_peer(me) {
        Some(p) => names.get(&p).cloned().unwrap_or(p),
        None => String::new(),
    }
}

/// 変更系の監査（チェーンに入れる）。
pub(crate) async fn audit(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    action: AuditAction,
    id: Uuid,
    trace: Option<&str>,
    metadata: serde_json::Value,
) -> Result<(), Error> {
    record_on(
        conn,
        ctx,
        AuditEntry {
            action,
            object_type: "channel",
            object_id: &id.to_string(),
            decision: Decision::Allow,
            trace_id: trace,
            metadata,
        },
        Chain::Yes,
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_names_are_trimmed_and_bounded() {
        assert_eq!(validate_name("  #営業  ").unwrap(), "営業");
        assert!(validate_name("   ").is_err());
        assert!(validate_name(&"a".repeat(81)).is_err());
        assert!(validate_name("a\u{7}b").is_err());
    }
}

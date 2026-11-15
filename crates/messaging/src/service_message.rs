//! 発言の操作（投稿・編集・削除・スレッド・リアクション・既読・検索・メンション通知）。
//!
//! 読取はすべて [`store_message::fetch_messages`]（参加前の発言の境界・PIT-63）を通り、
//! 書込の前後で FGA を判定する。メンションの宛先は「チャンネルの参加者」に限る
//! （非公開チャンネルの存在と中身を参加していない人に漏らさない）。

use authz::{AuthContext, CheckTuple, Consistency, Relation};
use chat::model::{ContentBlock, Mention};
use chrono::{DateTime, Utc};
use sqlx::PgConnection;
use std::collections::{HashMap, HashSet};
use storage::audit::{record_on, AuditAction, AuditEntry, Chain, Decision};
use storage::db_tenant::begin_tenant_tx;
use uuid::Uuid;

use crate::model::{file_ids, normalize_blocks, plain_text, validate_blocks};
use crate::pubsub::Signal;
use crate::store::{self, ChannelRow};
use crate::store_message::{self as sm, InsertMessage, MessageFilter, MessageRow};
use crate::{
    AuthorKind, AuthorView, ChannelKind, Error, FileView, MentionNotice, MessageState, MessageView,
    MessagingService, NewMessage, NotifyLevel, SearchHit,
};

/// 検索語の最小・最大文字数。
const MIN_QUERY_CHARS: usize = 1;
const MAX_QUERY_CHARS: usize = 200;

impl MessagingService {
    /// 最上位の発言（新しい順・`before` より前）。
    pub async fn list_messages(
        &self,
        ctx: &AuthContext,
        channel: Uuid,
        before: Option<DateTime<Utc>>,
        limit: i64,
    ) -> Result<Vec<MessageView>, Error> {
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        self.readable_channel(&mut tx, ctx, channel).await?;
        let rows = sm::fetch_messages(
            &mut tx,
            ctx,
            &ctx.principal().id,
            MessageFilter::Timeline {
                channel,
                before,
                limit,
            },
        )
        .await?;
        let views = self.views(&mut tx, ctx, rows).await?;
        tx.commit().await?;
        Ok(views)
    }

    /// スレッド（親＋返信・古い順）。親が読めなければ返信だけ。
    pub async fn thread(
        &self,
        ctx: &AuthContext,
        channel: Uuid,
        parent: Uuid,
    ) -> Result<Vec<MessageView>, Error> {
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        self.readable_channel(&mut tx, ctx, channel).await?;
        let me = ctx.principal().id.clone();
        let ids = [parent];
        let mut rows =
            sm::fetch_messages(&mut tx, ctx, &me, MessageFilter::Ids { ids: &ids }).await?;
        rows.retain(|r| r.channel_id == channel && r.parent_id.is_none());
        if rows.is_empty() {
            return Err(Error::NotFound);
        }
        rows.extend(
            sm::fetch_messages(&mut tx, ctx, &me, MessageFilter::Thread { channel, parent })
                .await?,
        );
        let views = self.views(&mut tx, ctx, rows).await?;
        tx.commit().await?;
        Ok(views)
    }

    /// `since` より後に変わった発言（SSE の差分・古い順）。権限は毎回引き直す。
    pub async fn changes_since(
        &self,
        ctx: &AuthContext,
        channel: Uuid,
        since: DateTime<Utc>,
    ) -> Result<Vec<MessageView>, Error> {
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        self.readable_channel(&mut tx, ctx, channel).await?;
        let rows = sm::fetch_messages(
            &mut tx,
            ctx,
            &ctx.principal().id,
            MessageFilter::ChangedSince {
                channel,
                since,
                limit: 200,
            },
        )
        .await?;
        let views = self.views(&mut tx, ctx, rows).await?;
        tx.commit().await?;
        Ok(views)
    }

    /// 1 件（読めなければ NotFound）。
    pub async fn message(&self, ctx: &AuthContext, id: Uuid) -> Result<MessageView, Error> {
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let raw = sm::raw_message(&mut tx, ctx, id)
            .await?
            .ok_or(Error::NotFound)?;
        self.readable_channel(&mut tx, ctx, raw.channel_id).await?;
        let ids = [id];
        let rows = sm::fetch_messages(
            &mut tx,
            ctx,
            &ctx.principal().id,
            MessageFilter::Ids { ids: &ids },
        )
        .await?;
        let view = self
            .views(&mut tx, ctx, rows)
            .await?
            .into_iter()
            .next()
            .ok_or(Error::NotFound)?;
        tx.commit().await?;
        Ok(view)
    }

    /// 投稿する。`can_post`（poster ∩ member）が要る。アーカイブ中は投稿できない。
    pub async fn post(
        &self,
        ctx: &AuthContext,
        channel: Uuid,
        input: NewMessage,
        trace: Option<&str>,
    ) -> Result<MessageView, Error> {
        validate_blocks(&input.blocks)?;
        let blocks = normalize_blocks(input.blocks);
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let c = self.readable_channel(&mut tx, ctx, channel).await?;
        self.require_post(ctx, &c, trace).await?;
        if let Some(parent) = input.parent_id {
            let p = sm::raw_message(&mut tx, ctx, parent)
                .await?
                .ok_or(Error::NotFound)?;
            if p.channel_id != channel || p.parent_id.is_some() {
                return Err(Error::Invalid(
                    "返信できるのはチャンネルの最上位の発言だけです".into(),
                ));
            }
            let ids = [parent];
            if sm::fetch_messages(
                &mut tx,
                ctx,
                &ctx.principal().id,
                MessageFilter::Ids { ids: &ids },
            )
            .await?
            .is_empty()
            {
                return Err(Error::NotFound);
            }
        }
        let me = ctx.principal().id.clone();
        let id = Uuid::new_v4();
        let text = plain_text(&blocks);
        sm::insert_message(
            &mut tx,
            ctx,
            &InsertMessage {
                id,
                channel_id: channel,
                parent_id: input.parent_id,
                author_kind: AuthorKind::User.as_str(),
                author_id: &me,
                blocks: &blocks,
                search_text: &text,
            },
        )
        .await?;
        let targets = self.resolve_mentions(&mut tx, ctx, &c, &blocks).await?;
        sm::insert_mentions(&mut tx, ctx, id, channel, &targets).await?;
        store::unhide_all(&mut tx, ctx, channel).await?;
        store::mark_read(&mut tx, ctx, channel, &me, Utc::now()).await?;
        audit_message(
            &mut tx,
            ctx,
            AuditAction::MessagingMessagePost,
            id,
            channel,
            trace,
            serde_json::json!({
                "parent": input.parent_id,
                "files": file_ids(&blocks).len(),
                "mentions": targets.len(),
            }),
        )
        .await?;
        tx.commit().await?;
        self.hub
            .publish(ctx.tenant_id(), channel, Signal::Changed)
            .await;
        if let Some(previews) = &self.previews {
            previews.spawn(self, ctx, id, channel, &blocks);
        }
        self.message(ctx, id).await
    }

    pub(crate) async fn require_post(
        &self,
        ctx: &AuthContext,
        c: &ChannelRow,
        trace: Option<&str>,
    ) -> Result<(), Error> {
        if c.archived_at.is_some() {
            return Err(Error::Forbidden(
                "アーカイブされたチャンネルは読み取り専用です",
            ));
        }
        if !self.check(ctx, Relation::CanPost, c.id).await? {
            self.deny(ctx, "channel", c.id, "post", trace).await;
            return Err(Error::Forbidden("このチャンネルに投稿する権限がありません"));
        }
        Ok(())
    }

    /// メンションの宛先を解決する（参加者だけ・ミュートを除く・自分を除く）。
    ///
    /// - 個人: その人がチャンネルの参加者なら。
    /// - ロール: 参加者のうち、そのロールのメンバー（FGA で展開）。
    /// - `@channel`: admin と投稿者だけが使える（使えなければ宛先にしない）。
    async fn resolve_mentions(
        &self,
        conn: &mut PgConnection,
        ctx: &AuthContext,
        c: &ChannelRow,
        blocks: &[ContentBlock],
    ) -> Result<Vec<(String, &'static str)>, Error> {
        let mentions: Vec<&Mention> = blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Mention { mention } => Some(mention),
                _ => None,
            })
            .collect();
        if mentions.is_empty() {
            return Ok(Vec::new());
        }
        let me = ctx.principal().id.clone();
        let members = store::members(conn, ctx, c.id).await?;
        let notify: HashMap<&str, NotifyLevel> = members
            .iter()
            .map(|m| (m.user_id.as_str(), NotifyLevel::parse(&m.notify)))
            .collect();
        let mut out: Vec<(String, &'static str)> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut push = |user: &str, via: &'static str, out: &mut Vec<(String, &'static str)>| {
            if user != me
                && notify.get(user).is_some_and(|n| *n != NotifyLevel::None)
                && seen.insert(user.to_string())
            {
                out.push((user.to_string(), via));
            }
        };
        for m in &mentions {
            if let Mention::User { user_id } = m {
                push(user_id, "user", &mut out);
            }
        }
        for m in &mentions {
            let Mention::Role { role_id } = m else {
                continue;
            };
            if authz::validate_local_id(role_id).is_err() {
                continue;
            }
            let role = ctx.ns().role(role_id);
            let items: Vec<CheckTuple> = members
                .iter()
                .map(|u| CheckTuple {
                    subject: ctx.ns().user(&u.user_id),
                    relation: Relation::Member,
                    object: role.clone(),
                })
                .collect();
            let ok = self
                .authz
                .batch_check(&items, Consistency::MinimizeLatency)
                .await?;
            for (u, ok) in members.iter().zip(ok) {
                if ok {
                    push(&u.user_id, "role", &mut out);
                }
            }
        }
        // `@channel` は admin と投稿者だけが使える。投稿には can_post（poster ∩ member）が要り、
        // アナウンス専用チャンネルの poster は個別指定なので、投稿できた時点で条件を満たす。
        // エージェントの発言（呼んだ人の名義）からは @channel を広げない。
        if mentions.iter().any(|m| matches!(m, Mention::Channel))
            && c.kind() != ChannelKind::Dm
            && (self.check(ctx, Relation::CanPost, c.id).await?
                || self.check(ctx, Relation::CanManage, c.id).await?)
        {
            for u in &members {
                push(&u.user_id, "channel", &mut out);
            }
        }
        Ok(out)
    }

    /// 編集する（本人だけ・期限なし）。前の本文は管理者閲覧の手続でだけ読める改訂として残す。
    pub async fn edit(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        blocks: Vec<ContentBlock>,
        trace: Option<&str>,
    ) -> Result<MessageView, Error> {
        validate_blocks(&blocks)?;
        let blocks = normalize_blocks(blocks);
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let raw = sm::raw_message(&mut tx, ctx, id)
            .await?
            .ok_or(Error::NotFound)?;
        let c = self.readable_channel(&mut tx, ctx, raw.channel_id).await?;
        self.require_visible(&mut tx, ctx, id).await?;
        if raw.author_kind != AuthorKind::User.as_str() || raw.author_id != ctx.principal().id {
            self.deny(ctx, "message", id, "edit", trace).await;
            return Err(Error::Forbidden("編集できるのは本人だけです"));
        }
        if raw.deleted_at.is_some() || raw.expired_at.is_some() {
            return Err(Error::Forbidden("削除された発言は編集できません"));
        }
        if c.archived_at.is_some() {
            return Err(Error::Forbidden(
                "アーカイブされたチャンネルは読み取り専用です",
            ));
        }
        let text = plain_text(&blocks);
        sm::edit_message(&mut tx, ctx, &raw, &blocks, &text).await?;
        audit_message(
            &mut tx,
            ctx,
            AuditAction::MessagingMessageEdit,
            id,
            raw.channel_id,
            trace,
            serde_json::json!({}),
        )
        .await?;
        tx.commit().await?;
        self.hub
            .publish(ctx.tenant_id(), raw.channel_id, Signal::Changed)
            .await;
        if let Some(previews) = &self.previews {
            previews.spawn(self, ctx, id, raw.channel_id, &blocks);
        }
        self.message(ctx, id).await
    }

    /// 削除する（本人とチャンネルの admin）。論理削除で、返信は残す。
    pub async fn delete(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        trace: Option<&str>,
    ) -> Result<(), Error> {
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let raw = sm::raw_message(&mut tx, ctx, id)
            .await?
            .ok_or(Error::NotFound)?;
        let c = self.readable_channel(&mut tx, ctx, raw.channel_id).await?;
        self.require_visible(&mut tx, ctx, id).await?;
        let own = raw.author_id == ctx.principal().id;
        let admin = c.kind() != ChannelKind::Dm
            && self.check(ctx, Relation::CanManage, raw.channel_id).await?;
        if !own && !admin {
            self.deny(ctx, "message", id, "delete", trace).await;
            return Err(Error::Forbidden(
                "削除できるのは本人とチャンネルの管理者だけです",
            ));
        }
        if raw.deleted_at.is_some() || raw.expired_at.is_some() {
            return Ok(());
        }
        sm::delete_message(&mut tx, ctx, &raw).await?;
        audit_message(
            &mut tx,
            ctx,
            AuditAction::MessagingMessageDelete,
            id,
            raw.channel_id,
            trace,
            serde_json::json!({ "by_admin": !own }),
        )
        .await?;
        tx.commit().await?;
        self.hub
            .publish(ctx.tenant_id(), raw.channel_id, Signal::Changed)
            .await;
        Ok(())
    }

    /// 境界の内側の発言か（参加前の発言に書込で触らせない）。
    async fn require_visible(
        &self,
        conn: &mut PgConnection,
        ctx: &AuthContext,
        id: Uuid,
    ) -> Result<(), Error> {
        let ids = [id];
        if sm::fetch_messages(
            conn,
            ctx,
            &ctx.principal().id,
            MessageFilter::Ids { ids: &ids },
        )
        .await?
        .is_empty()
        {
            return Err(Error::NotFound);
        }
        Ok(())
    }

    /// リアクションを付ける／外す（Unicode 絵文字だけ・投稿できる人だけ）。
    pub async fn react(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        emoji: &str,
        on: bool,
        trace: Option<&str>,
    ) -> Result<MessageView, Error> {
        if !is_emoji(emoji) {
            return Err(Error::Invalid("リアクションは絵文字 1 つだけです".into()));
        }
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let raw = sm::raw_message(&mut tx, ctx, id)
            .await?
            .ok_or(Error::NotFound)?;
        let c = self.readable_channel(&mut tx, ctx, raw.channel_id).await?;
        self.require_visible(&mut tx, ctx, id).await?;
        self.require_post(ctx, &c, trace).await?;
        if raw.deleted_at.is_some() || raw.expired_at.is_some() {
            return Err(Error::Forbidden("削除された発言にはリアクションできません"));
        }
        sm::set_reaction(&mut tx, ctx, id, &ctx.principal().id, emoji, on).await?;
        tx.commit().await?;
        self.hub
            .publish(ctx.tenant_id(), raw.channel_id, Signal::Changed)
            .await;
        self.message(ctx, id).await
    }

    /// 発言を検索する（二段 authz・Task 14.7）。
    ///
    /// - **pre-filter**: 検索者が member のチャンネル（FGA の list_objects）に絞る。速度のための絞り込み。
    /// - **post-filter**: 結果の `channel_id` ごとに `can_read` を引き直し、参加前の発言の境界を
    ///   [`sm::fetch_messages`] で掛ける。pre-filter が壊れても post-filter だけで権限が守られる。
    /// - 索引（`search_text`）にはメンバーシップを焼き込まない（PIT-64）。
    pub async fn search(
        &self,
        ctx: &AuthContext,
        query: &str,
        limit: i64,
        trace: Option<&str>,
    ) -> Result<Vec<SearchHit>, Error> {
        self.require_member(ctx).await?;
        let q = query.trim();
        let n = q.chars().count();
        if !(MIN_QUERY_CHARS..=MAX_QUERY_CHARS).contains(&n) {
            return Err(Error::Invalid("検索語は 1〜200 文字です".into()));
        }
        let channels = self.prefilter_channels(ctx).await?;
        self.search_in(ctx, q, &channels, limit, trace).await
    }

    /// pre-filter: 検索者が member のチャンネル id（FGA の実効集合）。
    pub(crate) async fn prefilter_channels(&self, ctx: &AuthContext) -> Result<Vec<Uuid>, Error> {
        let ids = self
            .authz
            .list_objects(&ctx.subject(), Relation::Member, authz::ObjectType::Channel)
            .await?;
        Ok(ids
            .iter()
            .filter_map(|raw| {
                let (_, rest) = raw.split_once(':')?;
                Uuid::parse_str(ctx.ns().strip_object_id(rest)?).ok()
            })
            .collect())
    }

    /// 検索の本体（pre-filter の結果を受け取り、post-filter を掛ける）。
    ///
    /// テストで pre-filter を壊した（全チャンネルを渡した）ときも、ここだけで権限を守る。
    pub async fn search_in(
        &self,
        ctx: &AuthContext,
        query: &str,
        prefilter: &[Uuid],
        limit: i64,
        trace: Option<&str>,
    ) -> Result<Vec<SearchHit>, Error> {
        self.require_member(ctx).await?;
        let pattern = format!("%{}%", storage::directory::escape_like(query));
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let me = ctx.principal().id.clone();
        let rows = sm::fetch_messages(
            &mut tx,
            ctx,
            &me,
            MessageFilter::Search {
                channels: prefilter,
                pattern: &pattern,
                limit: limit.clamp(1, 100) * 2,
            },
        )
        .await?;
        // post-filter: 結果に現れたチャンネルごとに can_read を引き直す（正本は FGA）。
        let mut channel_ids: Vec<Uuid> = rows.iter().map(|r| r.channel_id).collect();
        channel_ids.sort();
        channel_ids.dedup();
        let items: Vec<CheckTuple> = channel_ids
            .iter()
            .map(|c| CheckTuple {
                subject: ctx.subject(),
                relation: Relation::CanRead,
                object: ctx.ns().channel(&c.to_string()),
            })
            .collect();
        let ok = self
            .authz
            .batch_check(&items, Consistency::HigherConsistency)
            .await?;
        let readable: HashSet<Uuid> = channel_ids
            .iter()
            .zip(ok)
            .filter_map(|(c, ok)| ok.then_some(*c))
            .collect();
        let mut channels: HashMap<Uuid, ChannelRow> = HashMap::new();
        for c in &readable {
            if let Some(row) = store::channel(&mut tx, ctx, *c).await? {
                channels.insert(*c, row);
            }
        }
        let rows: Vec<MessageRow> = rows
            .into_iter()
            .filter(|r| readable.contains(&r.channel_id))
            .take(usize::try_from(limit.clamp(1, 100)).unwrap_or(100))
            .collect();
        let views = self.views(&mut tx, ctx, rows).await?;
        let names = store::display_names(
            &mut tx,
            ctx,
            &channels
                .values()
                .filter_map(|c| c.dm_peer(&me))
                .collect::<Vec<_>>(),
        )
        .await?;
        record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: AuditAction::MessagingSearch,
                object_type: "channel_message",
                object_id: "",
                decision: Decision::Allow,
                trace_id: trace,
                metadata: serde_json::json!({ "hits": views.len() }),
            },
            Chain::No,
        )
        .await?;
        tx.commit().await?;
        Ok(views
            .into_iter()
            .filter_map(|m| {
                let c = channels.get(&m.channel_id)?;
                let name = c.name.clone().unwrap_or_else(|| {
                    c.dm_peer(&me).map_or_else(
                        || crate::DEFAULT_AGENT_NAME.to_string(),
                        |p| names.get(&p).cloned().unwrap_or(p),
                    )
                });
                Some(SearchHit {
                    channel_id: c.id,
                    channel_name: name,
                    channel_kind: c.kind(),
                    message: m,
                })
            })
            .collect())
    }

    /// 自分あてのメンション（読めなくなった発言は出さない）。
    pub async fn mentions(
        &self,
        ctx: &AuthContext,
        unread_only: bool,
    ) -> Result<Vec<MentionNotice>, Error> {
        self.require_member(ctx).await?;
        let me = ctx.principal().id.clone();
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let rows = sm::my_mentions(&mut tx, ctx, &me, unread_only, 50).await?;
        let mut channel_ids: Vec<Uuid> = rows.iter().map(|r| r.1).collect();
        channel_ids.sort();
        channel_ids.dedup();
        let items: Vec<CheckTuple> = channel_ids
            .iter()
            .map(|c| CheckTuple {
                subject: ctx.subject(),
                relation: Relation::CanRead,
                object: ctx.ns().channel(&c.to_string()),
            })
            .collect();
        let ok = self
            .authz
            .batch_check(&items, Consistency::MinimizeLatency)
            .await?;
        let readable: HashSet<Uuid> = channel_ids
            .iter()
            .zip(ok)
            .filter_map(|(c, ok)| ok.then_some(*c))
            .collect();
        let ids: Vec<Uuid> = rows
            .iter()
            .filter(|r| readable.contains(&r.1))
            .map(|r| r.0)
            .collect();
        let msgs = sm::fetch_messages(&mut tx, ctx, &me, MessageFilter::Ids { ids: &ids }).await?;
        let views: HashMap<Uuid, MessageView> = self
            .views(&mut tx, ctx, msgs)
            .await?
            .into_iter()
            .map(|v| (v.id, v))
            .collect();
        let mut names: HashMap<Uuid, String> = HashMap::new();
        for c in &readable {
            if let Some(row) = store::channel(&mut tx, ctx, *c).await? {
                let peer = row.dm_peer(&me);
                let name = match (&row.name, peer) {
                    (Some(n), _) => n.clone(),
                    (None, Some(p)) => store::display_names(&mut tx, ctx, std::slice::from_ref(&p))
                        .await?
                        .remove(&p)
                        .unwrap_or(p),
                    (None, None) => crate::DEFAULT_AGENT_NAME.to_string(),
                };
                names.insert(*c, name);
            }
        }
        tx.commit().await?;
        Ok(rows
            .into_iter()
            .filter_map(|(mid, cid, via, read_at, created_at)| {
                let v = views.get(&mid)?;
                if v.state != MessageState::Normal {
                    return None;
                }
                Some(MentionNotice {
                    message_id: mid,
                    channel_id: cid,
                    channel_name: names.get(&cid).cloned().unwrap_or_default(),
                    parent_id: v.parent_id,
                    author: v.author.clone(),
                    excerpt: plain_text(&v.blocks).chars().take(140).collect(),
                    via,
                    read: read_at.is_some(),
                    created_at,
                })
            })
            .collect())
    }

    pub async fn mark_mentions_read(&self, ctx: &AuthContext, ids: &[Uuid]) -> Result<(), Error> {
        self.require_member(ctx).await?;
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        sm::mark_mentions_read(&mut tx, ctx, &ctx.principal().id, ids).await?;
        tx.commit().await?;
        Ok(())
    }

    /// 行を閲覧者から見た形にする（file_ref を閲覧者の権限で解決・リアクション・書き手の表示）。
    pub(crate) async fn views(
        &self,
        conn: &mut PgConnection,
        ctx: &AuthContext,
        rows: Vec<MessageRow>,
    ) -> Result<Vec<MessageView>, Error> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let me = ctx.principal().id.clone();
        let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
        let mut reactions = sm::reactions(conn, ctx, &me, &ids).await?;
        let mut authors: Vec<String> = rows.iter().map(|r| r.author_id.clone()).collect();
        authors.sort();
        authors.dedup();
        let names = store::display_names(conn, ctx, &authors).await?;
        let agents = crate::agent_profile::profiles(conn, ctx, &authors).await?;
        let active = self.active_users(ctx, &authors).await?;
        let all_files: Vec<Uuid> = rows
            .iter()
            .flat_map(|r| file_ids(&r.blocks.0).into_iter().map(|(_, id)| id))
            .collect();
        let readable = if all_files.is_empty() {
            HashMap::new()
        } else {
            self.files.readable(ctx, &all_files).await
        };
        Ok(rows
            .into_iter()
            .map(|r| {
                let kind = AuthorKind::parse(&r.author_kind);
                let owner = names.get(&r.author_id).cloned().unwrap_or_default();
                let author = match kind {
                    AuthorKind::User => AuthorView {
                        kind,
                        user_id: r.author_id.clone(),
                        display_name: owner,
                        owner_name: None,
                        has_icon: false,
                        active: active.get(&r.author_id).copied().unwrap_or(false),
                    },
                    AuthorKind::Assistant => {
                        let profile = agents.get(&r.author_id);
                        AuthorView {
                            kind,
                            user_id: r.author_id.clone(),
                            display_name: profile.map_or_else(
                                || crate::DEFAULT_AGENT_NAME.to_string(),
                                |p| p.name.clone(),
                            ),
                            owner_name: Some(owner),
                            has_icon: profile.is_some_and(|p| p.has_icon),
                            active: active.get(&r.author_id).copied().unwrap_or(false),
                        }
                    }
                };
                let mut blocks = r.blocks.0;
                let mut files = Vec::new();
                for (i, node) in file_ids(&blocks) {
                    if let Some(meta) = readable.get(&node) {
                        files.push(FileView {
                            block_index: i,
                            available: true,
                            node_id: Some(node.to_string()),
                            name: Some(meta.name.clone()),
                            size_bytes: meta.size_bytes,
                            content_type: meta.content_type.clone(),
                        });
                    } else {
                        // 読めない添付は node_id も伏せる（「参照できない添付」・メタデータを漏らさない）。
                        blocks[i] = ContentBlock::FileRef {
                            node_id: String::new(),
                            name: String::new(),
                        };
                        files.push(FileView {
                            block_index: i,
                            available: false,
                            node_id: None,
                            name: None,
                            size_bytes: None,
                            content_type: None,
                        });
                    }
                }
                let state = if r.expired_at.is_some() {
                    MessageState::Expired
                } else if r.deleted_at.is_some() {
                    MessageState::Deleted
                } else {
                    MessageState::Normal
                };
                MessageView {
                    id: r.id,
                    channel_id: r.channel_id,
                    parent_id: r.parent_id,
                    author,
                    blocks,
                    files,
                    reactions: reactions.remove(&r.id).unwrap_or_default(),
                    link_previews: r.link_previews.0,
                    reply_count: r.reply_count,
                    last_reply_at: r.last_reply_at,
                    state,
                    edited: r.edited_at.is_some() && state == MessageState::Normal,
                    created_at: r.created_at,
                    updated_at: r.updated_at,
                }
            })
            .collect())
    }
}

/// 発言の監査（チェーンに入れる）。本文は載せない。
pub(crate) async fn audit_message(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    action: AuditAction,
    id: Uuid,
    channel: Uuid,
    trace: Option<&str>,
    mut metadata: serde_json::Value,
) -> Result<(), Error> {
    if let Some(obj) = metadata.as_object_mut() {
        obj.insert("channel_id".into(), serde_json::json!(channel));
    }
    record_on(
        conn,
        ctx,
        AuditEntry {
            action,
            object_type: "channel_message",
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

/// Unicode 絵文字 1 つ（結合・異体字セレクタ・肌色・ZWJ 列を含む）か。
#[must_use]
pub fn is_emoji(s: &str) -> bool {
    let n = s.chars().count();
    if n == 0 || n > 16 || s.len() > 64 {
        return false;
    }
    let mut has_pictograph = false;
    for c in s.chars() {
        let u = c as u32;
        let pictograph = matches!(u,
            0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF | 0x2300..=0x23FF
            | 0x2190..=0x21FF | 0x3030 | 0x303D | 0x3297 | 0x3299 | 0x00A9 | 0x00AE | 0x203C
            | 0x2049 | 0x2122 | 0x2139 | 0x24C2 | 0x25AA..=0x25FE);
        let modifier = matches!(u,
            0x200D | 0xFE0E | 0xFE0F | 0x20E3 | 0x1F3FB..=0x1F3FF | 0xE0020..=0xE007F
            | 0x0030..=0x0039 | 0x0023 | 0x002A);
        if !(pictograph || modifier) {
            return false;
        }
        has_pictograph |= pictograph;
    }
    has_pictograph
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emoji_only() {
        for ok in ["👍", "🎉", "❤️", "👍🏽", "👨‍👩‍👧", "✅", "🇯🇵"] {
            assert!(is_emoji(ok), "{ok}");
        }
        for ng in ["", "a", "いいね", ":+1:", "<script>", "1", "👍 a"] {
            assert!(!is_emoji(ng), "{ng}");
        }
    }
}

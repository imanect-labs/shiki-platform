//! メッセージからの AI 呼出し（パーソナルエージェント・design §4.14・Task 14.8 / 14.17 / 14.20）。
//!
//! # 不変条件
//!
//! - **エージェントは独自の権限を持たない。** 新しい `PrincipalKind` を作らず、`AuthContext` は常に
//!   呼んだ人（照会者）のもの。呼べるのは持ち主本人だけ（メンションは書いた人自身のエージェントを指す）。
//! - **文脈を組む関数は [`MessagingService::agent_context`] の 1 か所。** 照会者の `AuthContext` で
//!   [`crate::store_message::fetch_messages`]（参加前の発言の境界つき）を読み直した結果だけを渡す。
//!   [`AgentRequest`] はこのクレートの外から組み立てられない（呼び出し側が発言の配列を持ち回れない・PIT-63）。
//! - **公開返信はチャンネル内の発言だけを根拠にする。** 照会者の権限で答える回答は「自分だけに表示」に
//!   回し、照会者が投稿するときに、参照した資料を読めない参加者がいれば知らせる（止めはしない）。

use async_trait::async_trait;
use authz::{AuthContext, CheckTuple, Consistency, Relation};
use chat::model::ContentBlock;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use std::sync::Arc;
use storage::db_tenant::begin_tenant_tx;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::agent_profile::{agent_name, normalize_name, profiles};
use crate::model::plain_text;
use crate::pubsub::Signal;
use crate::service_message::audit_message;
use crate::store;
use crate::store_message::{self as sm, InsertMessage, MessageFilter};
use crate::{AuthorKind, Error, MessagingService, DEFAULT_AGENT_NAME};
use storage::audit::AuditAction;

/// 文脈に入れる発言の最大件数（スレッド＋直近）。
const CONTEXT_MESSAGES: i64 = 40;
/// 1 発言の文脈への最大文字数。
const CONTEXT_CHARS_PER_MESSAGE: usize = 1_500;

/// 回答の公開範囲。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReplyMode {
    /// チャンネルに返信する（チャンネル内の発言だけを根拠にする）。
    Public,
    /// 呼んだ人だけに見せる（照会者の権限で社内文書も使う）。
    Private,
}

/// 文脈の 1 発言（照会者が読めるものだけ）。
#[derive(Debug, Clone)]
pub struct ContextLine {
    pub author: String,
    pub text: String,
    pub at: DateTime<Utc>,
}

/// エージェントへの依頼。**このクレートの外では組み立てられない**（フィールドは非公開）。
#[derive(Debug, Clone)]
pub struct AgentRequest {
    mode: ReplyMode,
    channel_name: String,
    transcript: Vec<ContextLine>,
    question: String,
    agent_name: String,
    force_chat: bool,
}

impl AgentRequest {
    pub fn mode(&self) -> ReplyMode {
        self.mode
    }
    pub fn channel_name(&self) -> &str {
        &self.channel_name
    }
    /// 照会者が読める発言だけ（古い順）。
    pub fn transcript(&self) -> &[ContextLine] {
        &self.transcript
    }
    pub fn question(&self) -> &str {
        &self.question
    }
    pub fn agent_name(&self) -> &str {
        &self.agent_name
    }
    /// 照会者が明示的にチャットでの作業を頼んだか。
    pub fn force_chat(&self) -> bool {
        self.force_chat
    }
}

/// エージェントの回答。
#[derive(Debug, Clone, Default)]
pub struct AgentAnswer {
    pub text: String,
    /// 参照した社内文書（file の node_id・自分だけに表示の回答だけが持つ）。
    pub sources: Vec<Uuid>,
    /// チャットを作って作業を移すべきと判断したときの題名（Task 14.17）。
    pub create_chat: Option<String>,
}

/// 作ったチャットの、人の承認待ち 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingApprovalRef {
    pub approval_id: String,
    pub tool_name: String,
}

/// LLM とチャットへの窓口（実装は api 側・llm-gateway と chat を経由する）。
#[async_trait]
pub trait AgentRuntime: Send + Sync {
    /// 回答を作る。`ctx` は照会者。
    async fn answer(&self, ctx: &AuthContext, req: &AgentRequest) -> Result<AgentAnswer, Error>;

    /// 照会者のチャットを作り、依頼を投入する（持ち主・実行者は照会者・承認モードは照会者の設定）。
    async fn start_chat(
        &self,
        ctx: &AuthContext,
        title: &str,
        req: &AgentRequest,
    ) -> Result<Uuid, Error>;

    /// チャットの人の承認待ちと、まだ動いているか。
    async fn chat_status(
        &self,
        ctx: &AuthContext,
        thread: Uuid,
    ) -> Result<(Vec<PendingApprovalRef>, bool), Error>;
}

/// 自分だけに表示中の回答（呼んだ人だけが読める）。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AgentReplyView {
    pub id: Uuid,
    pub channel_id: Uuid,
    pub trigger_message_id: Uuid,
    pub visibility: String,
    pub blocks: Vec<ContentBlock>,
    /// 作業を移したチャット（`/c/{id}` で開く）。
    pub chat_thread_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// 投稿の直前の確認が要る（参照資料を読めない参加者がいる）。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublishWarning {
    /// 参照した資料のうち、一部の参加者が読めないものの数。
    pub unreadable_sources: usize,
    /// 読めない参加者の人数（誰かは出さない）。
    pub affected_members: usize,
}

/// チャットでの作業を明示的に頼んでいるか（「チャットで」「チャットを作って」など）。
#[must_use]
pub fn asks_for_chat(question: &str) -> bool {
    let q = normalize_name(question);
    [
        "チャットで",
        "チャットを作",
        "チャットを開",
        "チャットに移",
        "チャットで作業",
    ]
    .iter()
    .any(|k| q.contains(&normalize_name(k)))
}

impl MessagingService {
    // ---- 文脈の組み立て（唯一の入口・PIT-63） ----

    /// 照会者の `AuthContext` で、呼び出しの発言の周辺（スレッド＋直近の発言）を読み直す。
    pub(crate) async fn agent_context(
        &self,
        conn: &mut PgConnection,
        ctx: &AuthContext,
        channel: Uuid,
        trigger: &sm::MessageRow,
        mode: ReplyMode,
    ) -> Result<AgentRequest, Error> {
        let me = ctx.principal().id.clone();
        let c = self.readable_channel(conn, ctx, channel).await?;
        let mut rows = if let Some(parent) = trigger.parent_id {
            let ids = [parent];
            let mut r =
                sm::fetch_messages(conn, ctx, &me, MessageFilter::Ids { ids: &ids }).await?;
            r.extend(
                sm::fetch_messages(conn, ctx, &me, MessageFilter::Thread { channel, parent })
                    .await?,
            );
            r
        } else {
            let mut r = sm::fetch_messages(
                conn,
                ctx,
                &me,
                MessageFilter::Timeline {
                    channel,
                    before: Some(trigger.created_at + chrono::Duration::microseconds(1)),
                    limit: CONTEXT_MESSAGES,
                },
            )
            .await?;
            r.reverse();
            r
        };
        rows.retain(|r| r.deleted_at.is_none() && r.expired_at.is_none());
        if rows.len() > CONTEXT_MESSAGES as usize {
            rows.drain(..rows.len() - CONTEXT_MESSAGES as usize);
        }
        let authors: Vec<String> = rows.iter().map(|r| r.author_id.clone()).collect();
        let names = store::display_names(conn, ctx, &authors).await?;
        let agents = profiles(conn, ctx, &authors).await?;
        let agent_name = agent_name(conn, ctx, &me).await?;
        let transcript = rows
            .iter()
            .filter(|r| r.id != trigger.id)
            .map(|r| {
                let owner = names.get(&r.author_id).cloned().unwrap_or_default();
                let author = if r.author_kind == AuthorKind::Assistant.as_str() {
                    let n = agents
                        .get(&r.author_id)
                        .map_or(DEFAULT_AGENT_NAME, |p| p.name.as_str());
                    format!("{n}（{owner} のエージェント）")
                } else {
                    owner
                };
                ContextLine {
                    author,
                    text: plain_text(&r.blocks.0)
                        .chars()
                        .take(CONTEXT_CHARS_PER_MESSAGE)
                        .collect(),
                    at: r.created_at,
                }
            })
            .collect();
        let question: String = plain_text(&trigger.blocks.0)
            .chars()
            .take(CONTEXT_CHARS_PER_MESSAGE * 2)
            .collect();
        Ok(AgentRequest {
            mode,
            channel_name: c.name.clone().unwrap_or_else(|| "DM".into()),
            transcript,
            force_chat: asks_for_chat(&question),
            question,
            agent_name,
        })
    }

    /// 自分のエージェントを呼ぶ（呼び出しの発言を書いた本人だけ・`ctx` は照会者）。
    ///
    /// 公開返信はチャンネルに書く。自分だけに表示・チャットの作成は照会者だけに見せる。
    pub async fn invoke_agent(
        self: &Arc<Self>,
        ctx: &AuthContext,
        runtime: &Arc<dyn AgentRuntime>,
        trigger_id: Uuid,
        mode: ReplyMode,
        trace: Option<&str>,
    ) -> Result<(), Error> {
        let me = ctx.principal().id.clone();
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let ids = [trigger_id];
        let trigger = sm::fetch_messages(&mut tx, ctx, &me, MessageFilter::Ids { ids: &ids })
            .await?
            .into_iter()
            .next()
            .ok_or(Error::NotFound)?;
        // 呼べるのは持ち主本人だけ（他人の発言のメンションで他人のエージェントを起動させない）。
        if trigger.author_kind != AuthorKind::User.as_str()
            || trigger.author_id != me
            || !crate::model::mentions_agent(&trigger.blocks.0)
        {
            return Err(Error::Forbidden("呼べるのは自分のエージェントだけです"));
        }
        let channel = trigger.channel_id;
        let c = self.readable_channel(&mut tx, ctx, channel).await?;
        let req = self
            .agent_context(&mut tx, ctx, channel, &trigger, mode)
            .await?;
        audit_message(
            &mut tx,
            ctx,
            AuditAction::MessagingAgentInvoke,
            trigger_id,
            channel,
            trace,
            serde_json::json!({ "mode": mode, "context_messages": req.transcript.len() }),
        )
        .await?;
        tx.commit().await?;

        let answer = runtime.answer(ctx, &req).await?;
        let thread_root = trigger.parent_id.unwrap_or(trigger.id);
        let wants_chat = req.force_chat || answer.create_chat.is_some();
        if wants_chat {
            let title = answer
                .create_chat
                .clone()
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| req.question.chars().take(40).collect());
            let thread = runtime.start_chat(ctx, &title, &req).await?;
            let text = format!(
                "チャット「{title}」を作成して作業を始めました。人の承認が必要になったら、あなたへの DM でお知らせします。"
            );
            self.save_private_reply(
                ctx,
                channel,
                trigger_id,
                vec![ContentBlock::Text { text }],
                &[],
                Some(thread),
            )
            .await?;
            self.spawn_approval_watch(ctx.clone(), Arc::clone(runtime), thread, title);
            return Ok(());
        }
        let blocks = vec![ContentBlock::Text {
            text: answer.text.clone(),
        }];
        match mode {
            ReplyMode::Private => {
                self.save_private_reply(ctx, channel, trigger_id, blocks, &answer.sources, None)
                    .await?;
            }
            ReplyMode::Public => {
                // 公開返信は投稿の権限（can_post）で書く（照会者が投稿できないチャンネルには書かない）。
                self.require_post(ctx, &c, trace).await?;
                let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
                let id = Uuid::new_v4();
                let text = plain_text(&blocks);
                sm::insert_message(
                    &mut tx,
                    ctx,
                    &InsertMessage {
                        id,
                        channel_id: channel,
                        parent_id: Some(thread_root),
                        author_kind: AuthorKind::Assistant.as_str(),
                        author_id: &me,
                        blocks: &blocks,
                        search_text: &text,
                    },
                )
                .await?;
                insert_reply(
                    &mut tx,
                    ctx,
                    channel,
                    trigger_id,
                    "public",
                    &blocks,
                    &[],
                    None,
                    Some(id),
                )
                .await?;
                tx.commit().await?;
                self.hub
                    .publish(ctx.tenant_id(), channel, Signal::Changed)
                    .await;
            }
        }
        Ok(())
    }

    /// エージェントが応答できなかったことを、呼んだ人だけに知らせる（失敗は黙らない）。
    pub async fn agent_failed(&self, ctx: &AuthContext, trigger: Uuid, err: &Error) {
        let channel = {
            let Ok(mut tx) = begin_tenant_tx(&self.pool, ctx.tenant()).await else {
                return;
            };
            let Ok(Some(raw)) = sm::raw_message(&mut tx, ctx, trigger).await else {
                return;
            };
            let _ = tx.commit().await;
            raw.channel_id
        };
        let reason = match err {
            Error::Unavailable(m) | Error::Forbidden(m) => (*m).to_string(),
            Error::Invalid(m) => m.clone(),
            _ => "一時的なエラーです。しばらくしてから呼び直してください".to_string(),
        };
        let text = format!("エージェントが応答できませんでした（{reason}）。");
        let _ = self
            .save_private_reply(
                ctx,
                channel,
                trigger,
                vec![ContentBlock::Text { text }],
                &[],
                None,
            )
            .await;
    }

    async fn save_private_reply(
        &self,
        ctx: &AuthContext,
        channel: Uuid,
        trigger: Uuid,
        blocks: Vec<ContentBlock>,
        sources: &[Uuid],
        chat: Option<Uuid>,
    ) -> Result<(), Error> {
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        insert_reply(
            &mut tx, ctx, channel, trigger, "private", &blocks, sources, chat, None,
        )
        .await?;
        tx.commit().await?;
        self.hub
            .publish(
                ctx.tenant_id(),
                channel,
                Signal::AgentReply {
                    user_id: ctx.principal().id.clone(),
                },
            )
            .await;
        Ok(())
    }

    /// 自分だけに表示中の回答（このチャンネル・自分が呼んだもの・新しい順）。
    pub async fn agent_replies(
        &self,
        ctx: &AuthContext,
        channel: Uuid,
    ) -> Result<Vec<AgentReplyView>, Error> {
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        self.readable_channel(&mut tx, ctx, channel).await?;
        let rows: Vec<ReplyRow> = sqlx::query_as(
            "SELECT id, channel_id, trigger_message_id, visibility, blocks, sources, chat_thread_id, created_at \
             FROM channel_agent_reply WHERE tenant_id = $1 AND org = $2 AND channel_id = $3 \
               AND invoked_by = $4 AND visibility = 'private' ORDER BY created_at DESC LIMIT 50",
        )
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(channel)
        .bind(&ctx.principal().id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows.into_iter().map(ReplyRow::view).collect())
    }

    /// 自分だけに表示中の回答をチャンネルに投稿する。
    ///
    /// 参照した資料を読めない参加者がいて `confirmed = false` なら投稿せず [`PublishWarning`] を返す
    /// （止めはしない — 確認のうえ `confirmed = true` で投稿できる）。
    pub async fn publish_agent_reply(
        &self,
        ctx: &AuthContext,
        reply: Uuid,
        confirmed: bool,
        trace: Option<&str>,
    ) -> Result<Option<PublishWarning>, Error> {
        let me = ctx.principal().id.clone();
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let row: ReplyRow = sqlx::query_as(
            "SELECT id, channel_id, trigger_message_id, visibility, blocks, sources, chat_thread_id, created_at \
             FROM channel_agent_reply WHERE tenant_id = $1 AND org = $2 AND id = $3 AND invoked_by = $4 \
             FOR UPDATE",
        )
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(reply)
        .bind(&me)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(Error::NotFound)?;
        if row.visibility != "private" {
            return Err(Error::Conflict("すでに投稿済みです"));
        }
        if row.chat_thread_id.is_some() {
            return Err(Error::Invalid("チャットの案内は投稿できません".into()));
        }
        let c = self.readable_channel(&mut tx, ctx, row.channel_id).await?;
        self.require_post(ctx, &c, trace).await?;
        if !confirmed && !row.sources.0.is_empty() {
            let members = store::members(&mut tx, ctx, row.channel_id).await?;
            let warning = self
                .unreadable_sources(ctx, &members, &row.sources.0)
                .await?;
            if warning.unreadable_sources > 0 {
                return Ok(Some(warning));
            }
        }
        let ids = [row.trigger_message_id];
        let parent = sm::fetch_messages(&mut tx, ctx, &me, MessageFilter::Ids { ids: &ids })
            .await?
            .into_iter()
            .next()
            .map(|t| t.parent_id.unwrap_or(t.id));
        let id = Uuid::new_v4();
        let text = plain_text(&row.blocks.0);
        sm::insert_message(
            &mut tx,
            ctx,
            &InsertMessage {
                id,
                channel_id: row.channel_id,
                parent_id: parent,
                author_kind: AuthorKind::Assistant.as_str(),
                author_id: &me,
                blocks: &row.blocks.0,
                search_text: &text,
            },
        )
        .await?;
        sqlx::query(
            "UPDATE channel_agent_reply SET visibility = 'posted', posted_message_id = $4 \
             WHERE tenant_id = $1 AND org = $2 AND id = $3",
        )
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(reply)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        audit_message(
            &mut tx,
            ctx,
            AuditAction::MessagingAgentReplyPost,
            id,
            row.channel_id,
            trace,
            serde_json::json!({ "reply": reply, "confirmed": confirmed, "sources": row.sources.0.len() }),
        )
        .await?;
        tx.commit().await?;
        self.hub
            .publish(ctx.tenant_id(), row.channel_id, Signal::Changed)
            .await;
        Ok(None)
    }

    /// 自分だけに表示中の回答を捨てる。
    pub async fn dismiss_agent_reply(&self, ctx: &AuthContext, reply: Uuid) -> Result<(), Error> {
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let n = sqlx::query(
            "DELETE FROM channel_agent_reply WHERE tenant_id = $1 AND org = $2 AND id = $3 \
               AND invoked_by = $4 AND visibility = 'private'",
        )
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(reply)
        .bind(&ctx.principal().id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
        if n == 0 {
            return Err(Error::NotFound);
        }
        Ok(())
    }

    /// 参照資料ごとに、参加者のうち読めない人がいるか（誰かは返さない）。
    async fn unreadable_sources(
        &self,
        ctx: &AuthContext,
        members: &[store::MemberRow],
        sources: &[Uuid],
    ) -> Result<PublishWarning, Error> {
        let mut unreadable = 0usize;
        let mut affected: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for s in sources {
            let file = ctx.ns().file(&s.to_string());
            let items: Vec<CheckTuple> = members
                .iter()
                .map(|m| CheckTuple {
                    subject: ctx.ns().user(&m.user_id),
                    relation: Relation::Viewer,
                    object: file.clone(),
                })
                .collect();
            let ok = self
                .authz
                .batch_check(&items, Consistency::HigherConsistency)
                .await?;
            let mut any = false;
            for (m, ok) in members.iter().zip(ok) {
                if !ok {
                    any = true;
                    affected.insert(&m.user_id);
                }
            }
            unreadable += usize::from(any);
        }
        Ok(PublishWarning {
            unreadable_sources: unreadable,
            affected_members: affected.len(),
        })
    }

    /// 作ったチャットの承認待ちを見張り、新しい承認待ちを照会者への DM で知らせる（Task 14.17）。
    ///
    /// 承認できるのは従来どおり照会者本人だけで、DM は承認カードへの案内だけを運ぶ。
    fn spawn_approval_watch(
        self: &Arc<Self>,
        ctx: AuthContext,
        runtime: Arc<dyn AgentRuntime>,
        thread: Uuid,
        title: String,
    ) {
        let svc = Arc::clone(self);
        tokio::spawn(async move {
            let mut notified: Vec<String> = Vec::new();
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_hours(6);
            let mut idle_rounds = 0;
            while tokio::time::Instant::now() < deadline {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let Ok((pending, active)) = runtime.chat_status(&ctx, thread).await else {
                    continue;
                };
                for p in pending {
                    if notified.contains(&p.approval_id) {
                        continue;
                    }
                    let text = format!(
                        "チャット「{title}」で承認が必要です（{}）。内容を確かめてから承認してください: /c/{thread}",
                        p.tool_name
                    );
                    if svc.notify_owner(&ctx, &text).await.is_ok() {
                        notified.push(p.approval_id);
                    }
                }
                if active {
                    idle_rounds = 0;
                } else {
                    idle_rounds += 1;
                    // 終わったチャットは少し様子を見てから見張りをやめる（続きの run を拾う）。
                    if idle_rounds > 24 {
                        break;
                    }
                }
            }
        });
    }

    /// 自分のエージェントから自分への DM に書く（承認の通知など）。
    pub async fn notify_owner(&self, ctx: &AuthContext, text: &str) -> Result<Uuid, Error> {
        let channel = self.agent_dm(ctx).await?;
        let me = ctx.principal().id.clone();
        let blocks = vec![ContentBlock::Text {
            text: text.to_string(),
        }];
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let id = Uuid::new_v4();
        sm::insert_message(
            &mut tx,
            ctx,
            &InsertMessage {
                id,
                channel_id: channel,
                parent_id: None,
                author_kind: AuthorKind::Assistant.as_str(),
                author_id: &me,
                blocks: &blocks,
                search_text: text,
            },
        )
        .await?;
        store::unhide_all(&mut tx, ctx, channel).await?;
        sm::insert_mentions(&mut tx, ctx, id, channel, &[(me.clone(), "user")]).await?;
        tx.commit().await?;
        self.hub
            .publish(ctx.tenant_id(), channel, Signal::Changed)
            .await;
        Ok(id)
    }
}

#[derive(sqlx::FromRow)]
struct ReplyRow {
    id: Uuid,
    channel_id: Uuid,
    trigger_message_id: Uuid,
    visibility: String,
    blocks: sqlx::types::Json<Vec<ContentBlock>>,
    sources: sqlx::types::Json<Vec<Uuid>>,
    chat_thread_id: Option<Uuid>,
    created_at: DateTime<Utc>,
}

impl ReplyRow {
    fn view(self) -> AgentReplyView {
        AgentReplyView {
            id: self.id,
            channel_id: self.channel_id,
            trigger_message_id: self.trigger_message_id,
            visibility: self.visibility,
            blocks: self.blocks.0,
            chat_thread_id: self.chat_thread_id,
            created_at: self.created_at,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn insert_reply(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    channel: Uuid,
    trigger: Uuid,
    visibility: &str,
    blocks: &[ContentBlock],
    sources: &[Uuid],
    chat: Option<Uuid>,
    posted: Option<Uuid>,
) -> Result<(), Error> {
    sqlx::query(
        "INSERT INTO channel_agent_reply (tenant_id, org, id, channel_id, trigger_message_id, invoked_by, \
            visibility, blocks, sources, chat_thread_id, posted_message_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(Uuid::new_v4())
    .bind(channel)
    .bind(trigger)
    .bind(&ctx.principal().id)
    .bind(visibility)
    .bind(sqlx::types::Json(blocks))
    .bind(sqlx::types::Json(sources))
    .bind(chat)
    .bind(posted)
    .execute(conn)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_chat_requests() {
        assert!(asks_for_chat("これチャットで調べて"));
        assert!(asks_for_chat("チャットを作って進めて"));
        assert!(!asks_for_chat("要約して"));
    }
}

//! 発言・リアクション・メンションの SQL。
//!
//! # 参加前の発言の境界（PIT-63）
//!
//! 通常の読取経路（一覧・スレッド・配信の差分・検索・メンション通知・AI の文脈）は**すべて**
//! [`fetch_messages`] を通る。境界（非公開・DM は `channel_member.visible_from` より後だけ）と
//! 「削除・期限切れの本文を返さない」はこの関数の SQL に 1 か所で掛けており、呼び出し側からは
//! 外せない。管理者の閲覧（eDiscovery）は [`crate::access`] の別の関数で読み、ここには相乗りしない
//! （PIT-65）。

use authz::AuthContext;
use chat::model::ContentBlock;
use chrono::{DateTime, Utc};
use sqlx::{PgConnection, Postgres, QueryBuilder};
use std::collections::HashMap;
use uuid::Uuid;

use crate::{Error, LinkPreview, ReactionView};

/// `channel_message` の閲覧者向けの 1 行（削除・期限切れは本文が空）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MessageRow {
    pub id: Uuid,
    pub channel_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub author_kind: String,
    pub author_id: String,
    pub blocks: sqlx::types::Json<Vec<ContentBlock>>,
    pub link_previews: sqlx::types::Json<Vec<LinkPreview>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub edited_at: Option<DateTime<Utc>>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub expired_at: Option<DateTime<Utc>>,
    pub reply_count: i64,
    pub last_reply_at: Option<DateTime<Utc>>,
}

/// [`fetch_messages`] の絞り込み。
#[derive(Debug, Clone)]
pub enum MessageFilter<'a> {
    /// 最上位の発言を新しい順に（`before` より前）。
    Timeline {
        channel: Uuid,
        before: Option<DateTime<Utc>>,
        limit: i64,
    },
    /// スレッドの返信（古い順）。
    Thread { channel: Uuid, parent: Uuid },
    /// `since` より後に変わった発言（配信の差分・古い順）。
    ChangedSince {
        channel: Uuid,
        since: DateTime<Utc>,
        limit: i64,
        /// 送った発言の版（id・`updated_at`）。同じ版は SQL の段階で除く（上限を送済みで埋めない）。
        sent: &'a [(Uuid, DateTime<Utc>)],
    },
    /// 指定の発言（チャンネルを問わない・境界は掛かる）。
    Ids { ids: &'a [Uuid] },
    /// 全文検索（pre-filter = 指定チャンネルに限る・新しい順）。
    Search {
        channels: &'a [Uuid],
        pattern: &'a str,
        limit: i64,
    },
}

/// 境界（PIT-63）を `alias` の発言に掛ける: 公開チャンネルは制約しない。非公開・DM は閲覧者の
/// 参加の時刻より後だけ。参加していなければ（visible_from が NULL）比較が NULL になり 1 件も返らない。
fn push_visible(q: &mut QueryBuilder<'_, Postgres>, alias: &str, viewer: &str) {
    q.push(format!(
        "(c.kind = 'public' OR {alias}.created_at >= (SELECT vm.visible_from FROM channel_member vm \
         WHERE vm.tenant_id = {alias}.tenant_id AND vm.org = {alias}.org \
         AND vm.channel_id = {alias}.channel_id AND vm.user_id = "
    ))
    .push_bind(viewer.to_string())
    .push("))");
}

/// 閲覧者が読める発言を読む（通常の読取経路の唯一の入口・境界は外せない）。
///
/// **FGA の `can_read` は呼び出し側（service）が先に判定する。** この関数は、FGA では表せない
/// 参加前の発言の境界と、削除・期限切れの本文の秘匿を掛ける。
pub async fn fetch_messages(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    viewer: &str,
    filter: MessageFilter<'_>,
) -> Result<Vec<MessageRow>, Error> {
    let mut q: QueryBuilder<'_, Postgres> = QueryBuilder::new(
        "SELECT m.id, m.channel_id, m.parent_id, m.author_kind, m.author_id, \
           CASE WHEN m.deleted_at IS NULL AND m.expired_at IS NULL THEN m.blocks ELSE '[]'::jsonb END AS blocks, \
           CASE WHEN m.deleted_at IS NULL AND m.expired_at IS NULL THEN m.link_previews ELSE '[]'::jsonb END AS link_previews, \
           m.created_at, m.updated_at, m.edited_at, m.deleted_at, m.expired_at, \
           (SELECT count(*) FROM channel_message r WHERE r.tenant_id = m.tenant_id AND r.org = m.org \
              AND r.channel_id = m.channel_id AND r.parent_id = m.id AND ",
    );
    push_visible(&mut q, "r", viewer);
    q.push(
        ") AS reply_count, \
           (SELECT max(r.created_at) FROM channel_message r WHERE r.tenant_id = m.tenant_id AND r.org = m.org \
              AND r.channel_id = m.channel_id AND r.parent_id = m.id AND ",
    );
    push_visible(&mut q, "r", viewer);
    q.push(
        ") AS last_reply_at \
         FROM channel_message m \
         JOIN channel c ON c.tenant_id = m.tenant_id AND c.org = m.org AND c.id = m.channel_id \
         WHERE m.tenant_id = ",
    )
    .push_bind(ctx.tenant_id().to_string())
    .push(" AND m.org = ")
    .push_bind(ctx.org().to_string())
    .push(" AND ");
    push_visible(&mut q, "m", viewer);
    match filter {
        MessageFilter::Timeline {
            channel,
            before,
            limit,
        } => {
            q.push(" AND m.parent_id IS NULL AND m.channel_id = ")
                .push_bind(channel);
            if let Some(before) = before {
                q.push(" AND m.created_at < ").push_bind(before);
            }
            q.push(" ORDER BY m.created_at DESC, m.id DESC LIMIT ")
                .push_bind(limit.clamp(1, 200));
        }
        MessageFilter::Thread { channel, parent } => {
            q.push(" AND m.channel_id = ")
                .push_bind(channel)
                .push(" AND m.parent_id = ")
                .push_bind(parent)
                .push(" ORDER BY m.created_at, m.id LIMIT 1000");
        }
        MessageFilter::ChangedSince {
            channel,
            since,
            limit,
            sent,
        } => {
            let (ids, ats): (Vec<Uuid>, Vec<DateTime<Utc>>) = sent.iter().copied().unzip();
            q.push(" AND m.channel_id = ")
                .push_bind(channel)
                .push(" AND m.updated_at > ")
                .push_bind(since)
                .push(" AND NOT EXISTS (SELECT 1 FROM unnest(")
                .push_bind(ids)
                .push("::uuid[], ")
                .push_bind(ats)
                .push("::timestamptz[]) AS s(id, at) WHERE s.id = m.id AND s.at >= m.updated_at)")
                .push(" ORDER BY m.updated_at, m.id LIMIT ")
                .push_bind(limit.clamp(1, 500));
        }
        MessageFilter::Ids { ids } => {
            q.push(" AND m.id = ANY(")
                .push_bind(ids.to_vec())
                .push(") LIMIT 1000");
        }
        MessageFilter::Search {
            channels,
            pattern,
            limit,
        } => {
            q.push(" AND m.channel_id = ANY(")
                .push_bind(channels.to_vec())
                .push(") AND m.search_text ILIKE ")
                .push_bind(pattern.to_string())
                .push(" ESCAPE '\\' ORDER BY m.created_at DESC, m.id DESC LIMIT ")
                .push_bind(limit.clamp(1, 200));
        }
    }
    Ok(q.build_query_as::<MessageRow>().fetch_all(conn).await?)
}

/// 書き込み用の 1 行（本文の生の状態・境界を掛けない）。service が FGA を判定した後でだけ使う。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RawMessage {
    pub id: Uuid,
    pub channel_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub author_kind: String,
    pub author_id: String,
    pub blocks: sqlx::types::Json<Vec<ContentBlock>>,
    pub created_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub expired_at: Option<DateTime<Utc>>,
}

pub async fn raw_message(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    id: Uuid,
) -> Result<Option<RawMessage>, Error> {
    Ok(sqlx::query_as(
        "SELECT id, channel_id, parent_id, author_kind, author_id, blocks, created_at, deleted_at, expired_at \
         FROM channel_message WHERE tenant_id = $1 AND org = $2 AND id = $3 FOR UPDATE",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(id)
    .fetch_optional(conn)
    .await?)
}

/// 読むだけの用途の [`raw_message`]（行ロックを取らない・読取の API で書込を待たせない）。
pub async fn peek_message(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    id: Uuid,
) -> Result<Option<RawMessage>, Error> {
    Ok(sqlx::query_as(
        "SELECT id, channel_id, parent_id, author_kind, author_id, blocks, created_at, deleted_at, expired_at \
         FROM channel_message WHERE tenant_id = $1 AND org = $2 AND id = $3",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(id)
    .fetch_optional(conn)
    .await?)
}

/// 新しい発言の値。
pub struct InsertMessage<'a> {
    pub id: Uuid,
    pub channel_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub author_kind: &'a str,
    pub author_id: &'a str,
    pub blocks: &'a [ContentBlock],
    pub search_text: &'a str,
}

/// 発言を書く。スレッドの返信は親の `updated_at` も進める（返信数の配信）。
///
/// 時刻は挿入した瞬間（`clock_timestamp()`）にする。参加の境界（`visible_from`）も挿入した瞬間なので、
/// 同じ基準で比べられる（`now()` だとトランザクション開始時刻になり、参加の確定後に書いた発言が
/// 参加前扱いで新しい参加者から見えなくなる）。
pub async fn insert_message(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    m: &InsertMessage<'_>,
) -> Result<DateTime<Utc>, Error> {
    let at: DateTime<Utc> = sqlx::query_scalar(
        "INSERT INTO channel_message (tenant_id, org, id, channel_id, parent_id, author_kind, author_id, \
            blocks, search_text, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, clock_timestamp(), clock_timestamp()) RETURNING created_at",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(m.id)
    .bind(m.channel_id)
    .bind(m.parent_id)
    .bind(m.author_kind)
    .bind(m.author_id)
    .bind(sqlx::types::Json(m.blocks))
    .bind(m.search_text)
    .fetch_one(&mut *conn)
    .await?;
    if let Some(parent) = m.parent_id {
        touch(conn, ctx, parent).await?;
    }
    Ok(at)
}

/// 発言の `updated_at` を進める（リアクション・返信・プレビュー）。
pub async fn touch(conn: &mut PgConnection, ctx: &AuthContext, id: Uuid) -> Result<(), Error> {
    sqlx::query(
        "UPDATE channel_message SET updated_at = clock_timestamp() \
         WHERE tenant_id = $1 AND org = $2 AND id = $3",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(id)
    .execute(conn)
    .await?;
    Ok(())
}

/// 改訂前の本文を残す（編集・削除。管理者閲覧の手続でだけ読む）。
async fn record_revision(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    id: Uuid,
    blocks: &[ContentBlock],
    reason: &str,
) -> Result<(), Error> {
    sqlx::query(
        "INSERT INTO channel_message_revision (tenant_id, org, message_id, rev, blocks, reason, recorded_by) \
         VALUES ($1, $2, $3, (SELECT COALESCE(max(rev), 0) + 1 FROM channel_message_revision \
            WHERE tenant_id = $1 AND org = $2 AND message_id = $3), $4, $5, $6)",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(id)
    .bind(sqlx::types::Json(blocks))
    .bind(reason)
    .bind(&ctx.principal().id)
    .execute(conn)
    .await?;
    Ok(())
}

/// 編集する（前の本文は改訂として残す）。
pub async fn edit_message(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    old: &RawMessage,
    blocks: &[ContentBlock],
    search_text: &str,
) -> Result<(), Error> {
    record_revision(conn, ctx, old.id, &old.blocks.0, "edit").await?;
    sqlx::query(
        "UPDATE channel_message SET blocks = $4, search_text = $5, link_previews = '[]'::jsonb, \
            edited_at = clock_timestamp(), updated_at = clock_timestamp() \
         WHERE tenant_id = $1 AND org = $2 AND id = $3",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(old.id)
    .bind(sqlx::types::Json(blocks))
    .bind(search_text)
    .execute(conn)
    .await?;
    Ok(())
}

/// 論理削除する（本文は改訂として残し、通常の読取・検索からは消す）。
pub async fn delete_message(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    old: &RawMessage,
) -> Result<(), Error> {
    record_revision(conn, ctx, old.id, &old.blocks.0, "delete").await?;
    sqlx::query(
        "UPDATE channel_message SET deleted_at = clock_timestamp(), deleted_by = $4, search_text = NULL, \
            updated_at = clock_timestamp() WHERE tenant_id = $1 AND org = $2 AND id = $3",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(old.id)
    .bind(&ctx.principal().id)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "DELETE FROM channel_mention WHERE tenant_id = $1 AND org = $2 AND message_id = $3",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(old.id)
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn set_link_previews(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    id: Uuid,
    previews: &[LinkPreview],
    expected_text: &str,
) -> Result<(), Error> {
    // 取得の間に編集されていたら書かない（編集で消えた URL のプレビューを残さない）。
    sqlx::query(
        "UPDATE channel_message SET link_previews = $4, updated_at = clock_timestamp() \
         WHERE tenant_id = $1 AND org = $2 AND id = $3 AND deleted_at IS NULL AND expired_at IS NULL \
           AND search_text = $5",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(id)
    .bind(sqlx::types::Json(previews))
    .bind(expected_text)
    .execute(conn)
    .await?;
    Ok(())
}

/// リアクションを付ける／外す。変わったら `true`。
pub async fn set_reaction(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    id: Uuid,
    user: &str,
    emoji: &str,
    on: bool,
) -> Result<bool, Error> {
    let n = if on {
        sqlx::query(
            "INSERT INTO channel_message_reaction (tenant_id, org, message_id, user_id, emoji) \
             VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
        )
    } else {
        sqlx::query(
            "DELETE FROM channel_message_reaction WHERE tenant_id = $1 AND org = $2 \
             AND message_id = $3 AND user_id = $4 AND emoji = $5",
        )
    }
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(id)
    .bind(user)
    .bind(emoji)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    if n > 0 {
        touch(conn, ctx, id).await?;
    }
    Ok(n > 0)
}

/// リアクションの集計（発言ごと・同じ絵文字は 1 行）。
pub async fn reactions(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    viewer: &str,
    ids: &[Uuid],
) -> Result<HashMap<Uuid, Vec<ReactionView>>, Error> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<(Uuid, String, i64, bool)> = sqlx::query_as(
        "SELECT message_id, emoji, count(*), bool_or(user_id = $4) FROM channel_message_reaction \
         WHERE tenant_id = $1 AND org = $2 AND message_id = ANY($3) \
         GROUP BY message_id, emoji ORDER BY message_id, min(created_at)",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(ids)
    .bind(viewer)
    .fetch_all(conn)
    .await?;
    let mut out: HashMap<Uuid, Vec<ReactionView>> = HashMap::new();
    for (id, emoji, count, me) in rows {
        out.entry(id)
            .or_default()
            .push(ReactionView { emoji, count, me });
    }
    Ok(out)
}

/// メンションの宛先を書く（同じ発言・同じ人は 1 件）。
pub async fn insert_mentions(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    message: Uuid,
    channel: Uuid,
    targets: &[(String, &'static str)],
) -> Result<(), Error> {
    if targets.is_empty() {
        return Ok(());
    }
    let users: Vec<&str> = targets.iter().map(|(u, _)| u.as_str()).collect();
    let vias: Vec<&str> = targets.iter().map(|(_, v)| *v).collect();
    sqlx::query(
        "INSERT INTO channel_mention (tenant_id, org, user_id, message_id, channel_id, via) \
         SELECT $1, $2, u, $3, $4, v FROM unnest($5::text[], $6::text[]) AS t(u, v) \
         ON CONFLICT DO NOTHING",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(message)
    .bind(channel)
    .bind(&users)
    .bind(&vias)
    .execute(conn)
    .await?;
    Ok(())
}

/// 自分あてのメンション（新しい順）。読めるかは呼び出し側が [`fetch_messages`] で確かめ直す。
pub async fn my_mentions(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    user: &str,
    unread_only: bool,
    limit: i64,
) -> Result<Vec<(Uuid, Uuid, String, Option<DateTime<Utc>>, DateTime<Utc>)>, Error> {
    Ok(sqlx::query_as(
        "SELECT message_id, channel_id, via, read_at, created_at FROM channel_mention \
         WHERE tenant_id = $1 AND org = $2 AND user_id = $3 AND (NOT $4 OR read_at IS NULL) \
         ORDER BY created_at DESC LIMIT $5",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(user)
    .bind(unread_only)
    .bind(limit.clamp(1, 200))
    .fetch_all(conn)
    .await?)
}

pub async fn mark_mentions_read(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    user: &str,
    ids: &[Uuid],
) -> Result<(), Error> {
    sqlx::query(
        "UPDATE channel_mention SET read_at = now() WHERE tenant_id = $1 AND org = $2 AND user_id = $3 \
           AND read_at IS NULL AND (cardinality($4::uuid[]) = 0 OR message_id = ANY($4))",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(user)
    .bind(ids)
    .execute(conn)
    .await?;
    Ok(())
}

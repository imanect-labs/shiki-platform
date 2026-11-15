//! チャンネル・参加者・設定の SQL（発言は [`crate::store_message`]）。
//!
//! 全関数は [`storage::db_tenant::TenantTx`]（テナント文脈を設定済みのトランザクション）の接続と
//! [`AuthContext`] を受け取り、`tenant_id` と `org` は ctx からだけ取る（呼び出し側から渡せない）。
//! 認可（FGA）はここでは判定しない — service 層が判定してから呼ぶ。

use authz::AuthContext;
use chrono::{DateTime, Utc};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::{ChannelKind, Error, NotifyLevel};

/// `channel` の 1 行。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ChannelRow {
    pub id: Uuid,
    pub kind: String,
    pub name: Option<String>,
    pub topic: String,
    pub announcement: bool,
    pub dm_key: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
}

impl ChannelRow {
    pub fn kind(&self) -> ChannelKind {
        ChannelKind::parse(&self.kind).unwrap_or(ChannelKind::Private)
    }

    /// 利用者と自分のエージェントの DM か（`dm_key = agent:<user>`）。
    pub fn agent_dm_owner(&self) -> Option<&str> {
        self.dm_key.as_deref()?.strip_prefix(AGENT_DM_PREFIX)
    }

    /// DM の相手（自分以外）。
    pub fn dm_peer(&self, me: &str) -> Option<String> {
        let key = self.dm_key.as_deref()?;
        if key.starts_with(AGENT_DM_PREFIX) {
            return None;
        }
        let (a, b) = key.split_once(DM_SEP)?;
        Some(if a == me { b } else { a }.to_string())
    }
}

/// `channel_member` の 1 行。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MemberRow {
    pub user_id: String,
    pub visible_from: DateTime<Utc>,
    pub notify: String,
    pub hidden_at: Option<DateTime<Utc>>,
}

/// 一覧の 1 行（自分が参加しているチャンネル＋未読）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MyChannelRow {
    #[sqlx(flatten)]
    pub channel: ChannelRow,
    pub notify: String,
    pub hidden_at: Option<DateTime<Utc>>,
    pub member_count: i64,
    pub unread: i64,
    pub mentions: i64,
    pub last_message_at: Option<DateTime<Utc>>,
}

/// DM の冪等キーの区切り（利用者 id は `|` を含まない・OIDC sub の検証は authz::ident）。
pub const DM_SEP: char = '|';
/// 自分のエージェントとの DM の冪等キーの接頭辞。
pub const AGENT_DM_PREFIX: &str = "agent:";

/// 2 人の DM の冪等キー（順序に依らない）。
#[must_use]
pub fn dm_key(a: &str, b: &str) -> String {
    if a <= b {
        format!("{a}{DM_SEP}{b}")
    } else {
        format!("{b}{DM_SEP}{a}")
    }
}

const CHANNEL_COLS: &str =
    "c.id, c.kind, c.name, c.topic, c.announcement, c.dm_key, c.created_by, c.created_at, c.archived_at";

/// チャンネルを作る。名前の重複は [`Error::Conflict`]。
pub async fn insert_channel(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    row: &ChannelRow,
) -> Result<(), Error> {
    let res = sqlx::query(
        "INSERT INTO channel (tenant_id, org, id, kind, name, topic, announcement, dm_key, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(row.id)
    .bind(&row.kind)
    .bind(&row.name)
    .bind(&row.topic)
    .bind(row.announcement)
    .bind(&row.dm_key)
    .bind(&row.created_by)
    .execute(conn)
    .await;
    match res {
        Ok(_) => Ok(()),
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            Err(Error::Conflict("同じ名前のチャンネルがすでにあります"))
        }
        Err(e) => Err(e.into()),
    }
}

pub async fn channel(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    id: Uuid,
) -> Result<Option<ChannelRow>, Error> {
    Ok(sqlx::query_as(&format!(
        "SELECT {CHANNEL_COLS} FROM channel c WHERE c.tenant_id = $1 AND c.org = $2 AND c.id = $3"
    ))
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(id)
    .fetch_optional(conn)
    .await?)
}

pub async fn channel_by_dm_key(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    key: &str,
) -> Result<Option<ChannelRow>, Error> {
    Ok(sqlx::query_as(&format!(
        "SELECT {CHANNEL_COLS} FROM channel c \
         WHERE c.tenant_id = $1 AND c.org = $2 AND c.kind = 'dm' AND c.dm_key = $3"
    ))
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(key)
    .fetch_optional(conn)
    .await?)
}

pub async fn update_channel(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    id: Uuid,
    name: Option<&str>,
    topic: Option<&str>,
) -> Result<(), Error> {
    let res = sqlx::query(
        "UPDATE channel SET name = COALESCE($4, name), topic = COALESCE($5, topic) \
         WHERE tenant_id = $1 AND org = $2 AND id = $3 AND kind <> 'dm'",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(id)
    .bind(name)
    .bind(topic)
    .execute(conn)
    .await;
    match res {
        Ok(_) => Ok(()),
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            Err(Error::Conflict("同じ名前のチャンネルがすでにあります"))
        }
        Err(e) => Err(e.into()),
    }
}

pub async fn set_archived(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    id: Uuid,
    archived: bool,
) -> Result<(), Error> {
    sqlx::query(
        "UPDATE channel SET archived_at = CASE WHEN $4 THEN COALESCE(archived_at, now()) ELSE NULL END \
         WHERE tenant_id = $1 AND org = $2 AND id = $3 AND kind <> 'dm'",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(id)
    .bind(archived)
    .execute(conn)
    .await?;
    Ok(())
}

/// 参加させる（入り直しは `visible_from` を今に更新する＝参加前の発言の境界を引き直す）。
pub async fn upsert_member(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    channel: Uuid,
    user: &str,
) -> Result<(), Error> {
    sqlx::query(
        "INSERT INTO channel_member (tenant_id, org, channel_id, user_id) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (tenant_id, org, channel_id, user_id) DO NOTHING",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(channel)
    .bind(user)
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn delete_member(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    channel: Uuid,
    user: &str,
) -> Result<bool, Error> {
    let n = sqlx::query(
        "DELETE FROM channel_member WHERE tenant_id = $1 AND org = $2 AND channel_id = $3 AND user_id = $4",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(channel)
    .bind(user)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    // 退出したら既読位置とメンションの通知も消す（戻ったときに過去の通知が見えないように）。
    sqlx::query(
        "DELETE FROM channel_read_state WHERE tenant_id = $1 AND org = $2 AND channel_id = $3 AND user_id = $4",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(channel)
    .bind(user)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "DELETE FROM channel_mention WHERE tenant_id = $1 AND org = $2 AND channel_id = $3 AND user_id = $4",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(channel)
    .bind(user)
    .execute(conn)
    .await?;
    Ok(n > 0)
}

pub async fn member(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    channel: Uuid,
    user: &str,
) -> Result<Option<MemberRow>, Error> {
    Ok(sqlx::query_as(
        "SELECT user_id, visible_from, notify, hidden_at FROM channel_member \
         WHERE tenant_id = $1 AND org = $2 AND channel_id = $3 AND user_id = $4",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(channel)
    .bind(user)
    .fetch_optional(conn)
    .await?)
}

pub async fn members(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    channel: Uuid,
) -> Result<Vec<MemberRow>, Error> {
    Ok(sqlx::query_as(
        "SELECT user_id, visible_from, notify, hidden_at FROM channel_member \
         WHERE tenant_id = $1 AND org = $2 AND channel_id = $3 ORDER BY visible_from, user_id LIMIT 5000",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(channel)
    .fetch_all(conn)
    .await?)
}

pub async fn set_notify(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    channel: Uuid,
    user: &str,
    level: NotifyLevel,
) -> Result<bool, Error> {
    Ok(sqlx::query(
        "UPDATE channel_member SET notify = $5 \
         WHERE tenant_id = $1 AND org = $2 AND channel_id = $3 AND user_id = $4",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(channel)
    .bind(user)
    .bind(level.as_str())
    .execute(conn)
    .await?
    .rows_affected()
        > 0)
}

pub async fn set_hidden(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    channel: Uuid,
    user: &str,
    hidden: bool,
) -> Result<bool, Error> {
    Ok(sqlx::query(
        "UPDATE channel_member SET hidden_at = CASE WHEN $5 THEN now() ELSE NULL END \
         WHERE tenant_id = $1 AND org = $2 AND channel_id = $3 AND user_id = $4",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(channel)
    .bind(user)
    .bind(hidden)
    .execute(conn)
    .await?
    .rows_affected()
        > 0)
}

/// 新しい発言が来た DM の非表示を解く（相手から届いたら一覧に戻す）。
pub async fn unhide_all(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    channel: Uuid,
) -> Result<(), Error> {
    sqlx::query(
        "UPDATE channel_member SET hidden_at = NULL \
         WHERE tenant_id = $1 AND org = $2 AND channel_id = $3 AND hidden_at IS NOT NULL",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(channel)
    .execute(conn)
    .await?;
    Ok(())
}

/// 既読位置を進める（戻さない）。
pub async fn mark_read(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    channel: Uuid,
    user: &str,
    at: DateTime<Utc>,
) -> Result<(), Error> {
    sqlx::query(
        "INSERT INTO channel_read_state (tenant_id, org, channel_id, user_id, last_read_at) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (tenant_id, org, channel_id, user_id) \
         DO UPDATE SET last_read_at = GREATEST(channel_read_state.last_read_at, EXCLUDED.last_read_at)",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(channel)
    .bind(user)
    .bind(at)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "UPDATE channel_mention SET read_at = now() WHERE tenant_id = $1 AND org = $2 \
         AND channel_id = $3 AND user_id = $4 AND read_at IS NULL AND created_at <= $5",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(channel)
    .bind(user)
    .bind(at)
    .execute(conn)
    .await?;
    Ok(())
}

/// 自分が参加しているチャンネル（未読・メンション数つき）。
///
/// 未読は `last_read_at` と参加の時刻（非公開・DM）の遅い方より新しい、他人の最上位の発言数
/// （100 で打ち切る）。行ごとの既読フラグは持たない（design §4.14 ドメイン）。
pub async fn my_channels(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    user: &str,
) -> Result<Vec<MyChannelRow>, Error> {
    Ok(sqlx::query_as(&format!(
        "SELECT {CHANNEL_COLS}, m.notify, m.hidden_at, \
           (SELECT count(*) FROM channel_member x WHERE x.tenant_id = c.tenant_id AND x.org = c.org \
              AND x.channel_id = c.id) AS member_count, \
           (SELECT count(*) FROM (SELECT 1 FROM channel_message msg \
              WHERE msg.tenant_id = c.tenant_id AND msg.org = c.org AND msg.channel_id = c.id \
                AND msg.parent_id IS NULL AND msg.deleted_at IS NULL AND msg.expired_at IS NULL \
                AND NOT (msg.author_kind = 'user' AND msg.author_id = $3) \
                AND msg.created_at > GREATEST(COALESCE(rs.last_read_at, '-infinity'::timestamptz), \
                      CASE WHEN c.kind = 'public' THEN '-infinity'::timestamptz ELSE m.visible_from END) \
              LIMIT 100) u) AS unread, \
           (SELECT count(*) FROM channel_mention mm WHERE mm.tenant_id = c.tenant_id AND mm.org = c.org \
              AND mm.channel_id = c.id AND mm.user_id = $3 AND mm.read_at IS NULL) AS mentions, \
           (SELECT max(msg.created_at) FROM channel_message msg WHERE msg.tenant_id = c.tenant_id \
              AND msg.org = c.org AND msg.channel_id = c.id AND msg.parent_id IS NULL \
              AND (c.kind = 'public' OR msg.created_at >= m.visible_from)) AS last_message_at \
         FROM channel_member m \
         JOIN channel c ON c.tenant_id = m.tenant_id AND c.org = m.org AND c.id = m.channel_id \
         LEFT JOIN channel_read_state rs ON rs.tenant_id = m.tenant_id AND rs.org = m.org \
              AND rs.channel_id = m.channel_id AND rs.user_id = m.user_id \
         WHERE m.tenant_id = $1 AND m.org = $2 AND m.user_id = $3 \
         ORDER BY c.kind, lower(COALESCE(c.name, '')), c.created_at LIMIT 1000"
    ))
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(user)
    .fetch_all(conn)
    .await?)
}

/// 組織の公開チャンネル（参加していなくても見える・アーカイブ済みは `include_archived` のときだけ）。
pub async fn public_channels(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    query: &str,
    include_archived: bool,
    limit: i64,
) -> Result<Vec<(ChannelRow, i64)>, Error> {
    #[derive(sqlx::FromRow)]
    struct Row {
        #[sqlx(flatten)]
        channel: ChannelRow,
        member_count: i64,
    }
    let pattern = format!("%{}%", storage::directory::escape_like(query.trim()));
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "SELECT {CHANNEL_COLS}, (SELECT count(*) FROM channel_member x WHERE x.tenant_id = c.tenant_id \
            AND x.org = c.org AND x.channel_id = c.id) AS member_count \
         FROM channel c WHERE c.tenant_id = $1 AND c.org = $2 AND c.kind = 'public' \
           AND ($3 OR c.archived_at IS NULL) AND (c.name ILIKE $4 ESCAPE '\\' OR c.topic ILIKE $4 ESCAPE '\\') \
         ORDER BY lower(c.name) LIMIT $5"
    ))
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(include_archived)
    .bind(&pattern)
    .bind(limit)
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.channel, r.member_count))
        .collect())
}

/// 表示名（ディレクトリ）。見つからない id は含まない。
pub async fn display_names(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    ids: &[String],
) -> Result<std::collections::HashMap<String, String>, Error> {
    if ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT user_id, display_name FROM directory_user \
         WHERE tenant_id = $1 AND org = $2 AND user_id = ANY($3)",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(ids)
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().collect())
}

/// 組織の利用者の表示名（エージェント名のなりすまし判定用・上限つき）。
pub async fn all_display_names(
    conn: &mut PgConnection,
    ctx: &AuthContext,
) -> Result<Vec<String>, Error> {
    Ok(sqlx::query_scalar(
        "SELECT display_name FROM directory_user WHERE tenant_id = $1 AND org = $2 LIMIT 50000",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .fetch_all(conn)
    .await?)
}

/// org の設定（行が無ければ既定）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SettingsRow {
    pub public_channel_creators: String,
    pub retention_days: Option<i32>,
    pub access_approvers: Vec<String>,
    pub rev: i64,
}

impl Default for SettingsRow {
    fn default() -> Self {
        Self {
            public_channel_creators: "everyone".into(),
            retention_days: None,
            access_approvers: Vec::new(),
            rev: 0,
        }
    }
}

pub async fn settings(conn: &mut PgConnection, ctx: &AuthContext) -> Result<SettingsRow, Error> {
    Ok(sqlx::query_as(
        "SELECT public_channel_creators, retention_days, access_approvers, rev FROM messaging_settings \
         WHERE tenant_id = $1 AND org = $2",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .fetch_optional(conn)
    .await?
    .unwrap_or_default())
}

/// 設定を書く（`rev` が一致したときだけ・楽観ロック）。書けたら新しい rev。
pub async fn put_settings(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    s: &SettingsRow,
) -> Result<i64, Error> {
    let rev: Option<i64> = sqlx::query_scalar(
        "INSERT INTO messaging_settings (tenant_id, org, public_channel_creators, retention_days, \
            access_approvers, rev, updated_by) VALUES ($1, $2, $3, $4, $5, 1, $7) \
         ON CONFLICT (tenant_id, org) DO UPDATE SET public_channel_creators = $3, retention_days = $4, \
            access_approvers = $5, rev = messaging_settings.rev + 1, updated_by = $7, updated_at = now() \
            WHERE messaging_settings.tenant_id = $1 AND messaging_settings.org = $2 \
              AND messaging_settings.rev = $6 \
         RETURNING rev",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(&s.public_channel_creators)
    .bind(s.retention_days)
    .bind(&s.access_approvers)
    .bind(s.rev)
    .bind(&ctx.principal().id)
    .fetch_optional(conn)
    .await?;
    // 初回（行なし）は rev = 0 を期待値とし、INSERT で 1 になる。
    match rev {
        Some(r) if s.rev == 0 || r == s.rev + 1 => Ok(r),
        _ => Err(Error::Conflict(
            "設定が他の人に変更されました。読み込み直してください",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dm_key_is_order_independent() {
        assert_eq!(dm_key("b", "a"), dm_key("a", "b"));
        assert_eq!(dm_key("a", "b"), "a|b");
    }

    #[test]
    fn dm_peer_and_agent_dm() {
        let mut row = ChannelRow {
            id: Uuid::nil(),
            kind: "dm".into(),
            name: None,
            topic: String::new(),
            announcement: false,
            dm_key: Some(dm_key("u1", "u2")),
            created_by: "u1".into(),
            created_at: Utc::now(),
            archived_at: None,
        };
        assert_eq!(row.dm_peer("u1").as_deref(), Some("u2"));
        assert_eq!(row.dm_peer("u2").as_deref(), Some("u1"));
        assert_eq!(row.agent_dm_owner(), None);
        row.dm_key = Some(format!("{AGENT_DM_PREFIX}u1"));
        assert_eq!(row.agent_dm_owner(), Some("u1"));
        assert_eq!(row.dm_peer("u1"), None);
    }
}

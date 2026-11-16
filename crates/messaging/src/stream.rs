//! 配信（SSE）の読み取り許可（PIT-64）。
//!
//! 購読中の接続は、合図のたびに FGA を引き直すと 1,000 接続 × 発言数の check になる。そこで
//! 購読の開始時（と剥奪の合図・上限 TTL ごと）に [`MessagingService::authorize_read`] で FGA を引き、
//! その結果を [`ReadGrant`] として持ち回る。`ReadGrant` はこのクレートの外では作れず、
//! [`GRANT_TTL`] を過ぎたら [`MessagingService::changes_with`] が自動で引き直す。
//! 剥奪は FGA から先に消し、加えて参加前の発言の境界（`channel_member`）も消えるので、非公開・DM は
//! TTL を待たず次の読み直しから 1 件も返らない。

use authz::AuthContext;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use storage::db_tenant::begin_tenant_tx;
use uuid::Uuid;

use crate::store_message::{self as sm, MessageFilter};
use crate::{Error, MessageView, MessagingService};

/// 読み取り許可の有効期間（購読の再評価の上限 TTL）。
pub const GRANT_TTL: Duration = Duration::from_secs(30);

/// 購読者がチャンネルを読めることを確かめた記録（サービスだけが作る）。
#[derive(Debug, Clone)]
pub struct ReadGrant {
    tenant: String,
    user: String,
    channel: Uuid,
    checked_at: Instant,
}

impl ReadGrant {
    pub fn channel(&self) -> Uuid {
        self.channel
    }
}

impl MessagingService {
    /// 読めることを FGA で確かめ、読み取り許可を出す（読めなければ NotFound）。
    pub async fn authorize_read(
        &self,
        ctx: &AuthContext,
        channel: Uuid,
    ) -> Result<ReadGrant, Error> {
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        self.readable_channel(&mut tx, ctx, channel).await?;
        tx.commit().await?;
        Ok(ReadGrant {
            tenant: ctx.tenant_id().to_string(),
            user: ctx.principal().id.clone(),
            channel,
            checked_at: Instant::now(),
        })
    }

    /// 許可を使って、`since` より後に変わった発言を読む（TTL を過ぎていれば FGA を引き直す）。
    ///
    /// `sent` は送った発言の版（`updated_at`）。同じ版は閲覧者向けの形（添付の権限評価・書き手の表示）を
    /// 組む前に落とす（重なりのある読み直しで、送済みの発言の認可と監査をやり直さない）。
    pub async fn changes_with(
        &self,
        ctx: &AuthContext,
        grant: &mut ReadGrant,
        since: DateTime<Utc>,
        sent: &mut HashMap<Uuid, DateTime<Utc>>,
    ) -> Result<Vec<MessageView>, Error> {
        // 別の利用者・テナントの許可を流用させない。
        if grant.tenant != ctx.tenant_id() || grant.user != ctx.principal().id {
            return Err(Error::Forbidden("読み取り許可の持ち主が違います"));
        }
        if grant.checked_at.elapsed() >= GRANT_TTL {
            *grant = self.authorize_read(ctx, grant.channel).await?;
        }
        sent.retain(|_, at| *at > since);
        let sent_pairs: Vec<(Uuid, DateTime<Utc>)> = sent.iter().map(|(k, v)| (*k, *v)).collect();
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let rows = sm::fetch_messages(
            &mut tx,
            ctx,
            &grant.user,
            MessageFilter::ChangedSince {
                channel: grant.channel,
                since,
                limit: 200,
                sent: &sent_pairs,
            },
        )
        .await?;
        let versions: Vec<(Uuid, DateTime<Utc>)> =
            rows.iter().map(|r| (r.id, r.updated_at)).collect();
        let views = self.views(&mut tx, ctx, rows).await?;
        tx.commit().await?;
        // 返せたことが確定してから「送った」と記録する（失敗した版を次の読み直しで落とさない）。
        sent.extend(versions);
        Ok(views)
    }
}

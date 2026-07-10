//! アプリ→ユーザー通知の台帳（Task 9.8・`app_notification`）。
//!
//! notify.send の永続先。アルファでは記録＋監査のみ（web の通知一覧/既読 UI は後続 PR）。
//! 送信主体は常に**呼出ユーザー**（`created_by`）＝アプリ単独では送れない（confused-deputy 防御）。

use authz::AuthContext;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{map_db, GatewayError};

/// 通知 1 件。
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct AppNotification {
    pub id: Uuid,
    pub app_id: Uuid,
    pub recipient: String,
    pub title: String,
    pub body: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

/// `app_notification` への単一チョークポイント。
#[derive(Clone)]
pub struct NotificationStore {
    db: PgPool,
}

const MAX_TITLE: usize = 200;
const MAX_BODY: usize = 4000;

impl NotificationStore {
    pub fn new(db: PgPool) -> Self {
        NotificationStore { db }
    }

    /// 通知を記録する（入力検証込み・宛先の存在検証は行わない＝存在オラクルを作らない）。
    pub async fn send(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
        recipient: &str,
        title: &str,
        body: Option<&str>,
    ) -> Result<AppNotification, GatewayError> {
        let recipient = recipient.trim();
        let title = title.trim();
        if recipient.is_empty() {
            return Err(GatewayError::Invalid("recipient が空です".into()));
        }
        if title.is_empty() || title.chars().count() > MAX_TITLE {
            return Err(GatewayError::Invalid(format!(
                "title は 1〜{MAX_TITLE} 文字で指定してください"
            )));
        }
        if body.is_some_and(|b| b.chars().count() > MAX_BODY) {
            return Err(GatewayError::Invalid(format!(
                "body は {MAX_BODY} 文字以内で指定してください"
            )));
        }
        let row = sqlx::query_as(
            "INSERT INTO app_notification \
                 (tenant_id, org, app_id, recipient, title, body, created_by) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             RETURNING id, app_id, recipient, title, body, created_by, created_at",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(app_id)
        .bind(recipient)
        .bind(title)
        .bind(body)
        .bind(&ctx.principal.id)
        .fetch_one(&self.db)
        .await
        .map_err(map_db)?;
        Ok(row)
    }

    /// 受信者本人の通知一覧（新しい順・後続 PR の UI/API 用に用意）。
    pub async fn list_for_recipient(
        &self,
        ctx: &AuthContext,
        limit: i64,
    ) -> Result<Vec<AppNotification>, GatewayError> {
        let rows = sqlx::query_as(
            "SELECT id, app_id, recipient, title, body, created_by, created_at \
             FROM app_notification \
             WHERE tenant_id = $1 AND recipient = $2 \
             ORDER BY created_at DESC, id DESC LIMIT $3",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.principal.id)
        .bind(limit.clamp(1, 200))
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(rows)
    }
}

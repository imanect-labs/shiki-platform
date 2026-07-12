//! notify.send 能力アダプタ（Task 9.8）。
//!
//! `app_notification` 台帳へ記録し監査を残す（配信 UI は後続 PR）。宛先ユーザーの
//! 存在検証は行わない（テナント内ユーザー列挙のオラクルを作らない・不達は無害）。

use axum::{extract::State, Extension, Json};
use serde::{Deserialize, Serialize};
use serde_json::json;
use storage::audit::{AuditEntry, Decision};
use uuid::Uuid;

use crate::{
    router::{GatewayCtx, GatewayState},
    GatewayError,
};

#[derive(Debug, Deserialize)]
pub(crate) struct SendNotificationRequest {
    /// 宛先ユーザー（principal id・OIDC sub）。
    pub recipient: String,
    pub title: String,
    pub body: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SendNotificationResponse {
    pub id: Uuid,
}

pub(crate) async fn send(
    State(state): State<GatewayState>,
    Extension(ctx): Extension<GatewayCtx>,
    Json(req): Json<SendNotificationRequest>,
) -> Result<Json<SendNotificationResponse>, GatewayError> {
    let n = state
        .caps
        .notifications
        .send(
            &ctx.auth,
            ctx.installation.app_id,
            &req.recipient,
            &req.title,
            req.body.as_deref(),
        )
        .await?;
    // 監査（best-effort・宛先を記録。dual_gate の gateway.call とは別に宛先付きで残す）。
    if let Err(e) = state
        .audit
        .record(
            &ctx.auth,
            AuditEntry {
                action: "gateway.notify.send",
                object_type: "miniapp",
                object_id: &ctx.installation.app_id.to_string(),
                decision: Decision::Allow,
                trace_id: None,
                metadata: json!({ "recipient": n.recipient, "notification_id": n.id }),
            },
        )
        .await
    {
        tracing::warn!(error = %e, "notify.send の監査記録に失敗");
    }
    Ok(Json(SendNotificationResponse { id: n.id }))
}

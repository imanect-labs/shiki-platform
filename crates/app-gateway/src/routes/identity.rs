//! identity.read 能力アダプタ（Task 9.8）。
//!
//! 呼出ユーザー本人の最小 identity（sub・テナント・直接ロール）のみを返す。
//! 他ユーザーの参照・ディレクトリ列挙は提供しない（アプリに org 全体を見せない）。

use authz::ObjectType;
use axum::{extract::State, Extension, Json};
use serde::Serialize;

use crate::{
    router::{GatewayCtx, GatewayState},
    GatewayError,
};

#[derive(Debug, Serialize)]
pub(crate) struct GwIdentity {
    pub user_sub: String,
    pub tenant: String,
    /// 直接メンバーのロール（ローカル ID・サブツリー展開なし）。
    pub roles: Vec<String>,
}

pub(crate) async fn me(
    State(state): State<GatewayState>,
    Extension(ctx): Extension<GatewayCtx>,
) -> Result<Json<GwIdentity>, GatewayError> {
    let raw = state
        .authz
        .read_subject_objects(&ctx.auth.subject(), ObjectType::Role)
        .await
        .map_err(|e| GatewayError::Internal(format!("authz: {e}")))?;
    let ns = ctx.auth.ns();
    let mut roles = Vec::with_capacity(raw.len());
    for o in raw {
        // "role:<tenant>|<local>" → ローカル ID。他テナントのタプルは strip 失敗＝除外。
        let Some((_, id_part)) = o.split_once(':') else {
            continue;
        };
        if let Some(local) = ns.strip_object_id(id_part) {
            roles.push(local.to_string());
        }
    }
    roles.sort_unstable();
    Ok(Json(GwIdentity {
        user_sub: ctx.identity.user_sub.clone(),
        tenant: ctx.auth.tenant_id.clone(),
        roles,
    }))
}

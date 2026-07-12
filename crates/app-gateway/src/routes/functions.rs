//! B2 関数のユーザー起点起動（Task 9.12・`POST /gw/apps/functions/{function}/invoke`）。
//!
//! 二重ゲート通過後、[`crate::FunctionPort`]（api 配線実装）へ委譲する。実装側は
//! **RFC 8693 token-exchange（sub=ユーザー維持）**で B2 confidential client のトークンに
//! 交換し、サンドボックス実行のホスト委譲へ渡す（ゲストは token 非保持・INV-1）。
//! 関数名は**インストール時ピン**（server_spec.functions）に照合し宣言外は 404。

use axum::{
    extract::{Path, State},
    http::header::AUTHORIZATION,
    http::HeaderMap,
    Extension, Json,
};

use crate::{
    ports::FunctionInvokeSpec,
    router::{GatewayCtx, GatewayState},
    GatewayError,
};

pub(crate) async fn invoke(
    State(state): State<GatewayState>,
    Extension(ctx): Extension<GatewayCtx>,
    Path(function): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, GatewayError> {
    // 宣言外の関数は存在秘匿 404（インストール時ピンの functions のみ）。
    let declared = ctx
        .installation
        .server_spec
        .as_ref()
        .and_then(|s| s.get("functions"))
        .and_then(|f| f.as_array())
        .is_some_and(|fs| fs.iter().any(|f| f.as_str() == Some(function.as_str())));
    if !declared {
        return Err(GatewayError::NotFound);
    }
    // exchange の subject には受信した生トークンを使う（middleware で検証済み）。
    let subject_token = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| GatewayError::Unauthenticated("Bearer トークンがありません".into()))?
        .to_string();
    let value = state
        .caps
        .functions
        .invoke(
            &ctx.auth,
            FunctionInvokeSpec {
                app_id: ctx.installation.app_id,
                function,
                payload,
                subject_token,
                server_bundle: ctx.installation.server_bundle.clone(),
                server_spec: ctx.installation.server_spec.clone(),
                client_id_b2: ctx.installation.client_id_b2.clone(),
            },
        )
        .await?;
    Ok(Json(value))
}

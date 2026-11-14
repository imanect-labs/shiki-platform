//! AI エージェント（agent.invoke）の設定の天井をエディタへ返す（BUG-6・D-29）。
//!
//! エディタの「読んでよいサイト」は管理者が決める天井（`workflow.agent.web_allowlist`）から選ばせる
//! （自由入力にしない）。天井が空なら web.fetch の道具を選べないと表示する。手順の上限の既定も
//! デプロイ設定から返す。保存時の照合（V3 の `ir.host_outside_ceiling`）も同じ値を使う。

use axum::{extract::State, Json};
use serde::Serialize;
use utoipa::ToSchema;

use crate::{extract::AuthContextExt, state::AppState};

/// AI エージェントの設定の天井。
#[derive(Debug, Serialize, ToSchema)]
pub struct AgentSettingsResponse {
    /// web.fetch が読んでよいサイト（ホスト名か `*.example.com`）。空なら web.fetch は使えない。
    pub web_allowlist: Vec<String>,
    /// 手順の上限を省略したときの値。
    pub default_max_steps: u32,
    /// 手順の上限の最大。
    pub max_steps_limit: u32,
    /// 実行時間の上限の既定（秒）。
    pub default_max_duration_sec: u32,
    /// 実行時間の上限の範囲（秒）。
    pub min_duration_sec: u32,
    pub max_duration_sec: u32,
    /// ブロックの時間の上限を、作業時間の上限にこの秒を足した時間より短くしない（エンジンの打ち切りと同じ値・
    /// `run::step_timeout::AGENT_STOP_MARGIN_SEC`）。エディタが時間の上限の既定を言うときに足す。
    pub stop_margin_sec: u32,
}

/// AI エージェントの設定の天井（ログインしていれば誰でも読める・組織の秘密は含まない）。
#[utoipa::path(
    get,
    path = "/workflows/agent-settings",
    responses((status = 200, description = "AI エージェントの設定の天井", body = AgentSettingsResponse)),
    tag = "workflows"
)]
pub async fn get_agent_settings(
    State(state): State<AppState>,
    AuthContextExt(_ctx): AuthContextExt,
) -> Json<AgentSettingsResponse> {
    use workflow_engine::vocab::{
        AGENT_DURATION_RANGE_SEC, DEFAULT_AGENT_DURATION_SEC, MAX_AGENT_STEPS,
    };
    let limits = state.config.workflow.agent_limits();
    let (min_duration_sec, max_duration_sec) = AGENT_DURATION_RANGE_SEC;
    Json(AgentSettingsResponse {
        web_allowlist: limits.web_ceiling,
        default_max_steps: limits.default_max_steps.clamp(1, MAX_AGENT_STEPS),
        max_steps_limit: MAX_AGENT_STEPS,
        default_max_duration_sec: DEFAULT_AGENT_DURATION_SEC,
        min_duration_sec,
        max_duration_sec,
        stop_margin_sec: workflow_engine::run::step_timeout::AGENT_STOP_MARGIN_SEC,
    })
}

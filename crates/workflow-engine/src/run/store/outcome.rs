//! 起動した本人に返す run の結果と状態の読取口（GAP-09・D-28）。
//!
//! - [`RunOutcomeReader::live_states`]: 状態と、待機中なら何を待っているか（engine.md §3.1 の導出値 waiting）。
//!   [`super::RunStatusReader`] と同じく「状態だけ」で、呼び出し側が自分の境界で認可した run だけを渡す。
//! - [`RunOutcomeReader::own_outcomes`]: 結果（workflow.reply の節）と失敗の理由。**本人が画面・会話から
//!   起動した run だけ**を SQL で絞る（principal = 本人・principal_kind = user・org の一致・
//!   trigger_kind = interactive）。呼び出し側が他人の run_id を渡しても何も返らない（二重の関門の二段目）。
//!   schedule・event の run（実行主体は workflow プリンシパル）は構造的に返らない。
//!
//! 返すのは reply の節と失敗の分類だけで、他の step の出力・run の入力・IR は返さない。interactive の run の
//! 値はすべて本人の権限（本人の ReBAC ∩ declared_scopes・secret は本人の can_use）で読んだものなので、
//! 本人に返しても新たに読めるようになるものは無い（作成者に由来するのは固定の文言だけ）。

use std::collections::HashMap;

use serde_json::Value;
use sqlx::types::Json;
use sqlx::PgPool;
use uuid::Uuid;

use super::{map_db, RunStoreError};
use crate::run::failure::run_failure;
use crate::run::outcome::{ReplySection, RunFailure, RunReply, WaitingFor};
use crate::run::RunStatus;

/// run 全体で返す節の上限。
pub const MAX_REPLY_SECTIONS: usize = 10;
/// run 全体で返す節の大きさの上限（UTF-8 のバイト数）。
pub const MAX_REPLY_BYTES: usize = 32 * 1024;

/// run の今の状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveState {
    pub status: RunStatus,
    /// 待機中（running で、実行中・実行待ちの step が無く、wait で止まっている）なら待っているもの。
    pub waiting_for: Option<WaitingFor>,
}

/// 本人が起動した run の結果。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OwnOutcome {
    /// 失敗した run の理由（利用者向け）。
    pub failure: Option<RunFailure>,
    /// 返した節があるか（`reply` を求めなかった run でも分かる）。
    pub has_reply: bool,
    /// 返した節（`with_reply` に含めた run だけ）。
    pub reply: Option<RunReply>,
}

/// 結果と状態の読取口（上のモジュールの説明）。
#[derive(Clone)]
pub struct RunOutcomeReader {
    db: PgPool,
}

/// 待機中かどうかと、何を待っているか（承認 → できごと → 時間の順に見る）。別名 `r` の run に対する式
/// （engine.md §3.1 の導出値 waiting の 1 つの定義・実行履歴の一覧と詳細も同じ式を使う・BUG-119）。
pub(super) const WAITING_FOR: &str = "CASE WHEN r.status = 'running' AND NOT EXISTS ( \
         SELECT 1 FROM step_execution s WHERE s.tenant_id = r.tenant_id AND s.run_id = r.run_id \
           AND s.status IN ('ready', 'running')) \
     THEN CASE \
         WHEN EXISTS (SELECT 1 FROM step_execution s JOIN workflow_approval a \
                        ON a.tenant_id = s.tenant_id AND a.run_id = s.run_id \
                       AND a.step_path = s.step_path AND a.status = 'pending' \
                      WHERE s.tenant_id = r.tenant_id AND s.run_id = r.run_id \
                        AND s.status = 'waiting_event') THEN 'approval' \
         WHEN EXISTS (SELECT 1 FROM step_execution s WHERE s.tenant_id = r.tenant_id \
                        AND s.run_id = r.run_id AND s.status = 'waiting_event') THEN 'event' \
         WHEN EXISTS (SELECT 1 FROM step_execution s WHERE s.tenant_id = r.tenant_id \
                        AND s.run_id = r.run_id AND s.status = 'waiting_timer') THEN 'timer' \
     END END";

/// 本人が画面・会話から起動した run だけ（別名 `r`・$1 テナント・$3 本人・$4 org）。
const OWN_RUN: &str = "r.tenant_id = $1 AND r.principal = $3 AND r.principal_kind = 'user' \
     AND r.org = $4 AND r.trigger_kind = 'interactive'";

type OwnRow = (
    Uuid,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    bool,
);

impl RunOutcomeReader {
    pub fn new(db: PgPool) -> Self {
        RunOutcomeReader { db }
    }

    /// 複数の run の今の状態（無い run は結果に含めない）。
    pub async fn live_states(
        &self,
        ctx: &authz::AuthContext,
        run_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, LiveState>, RunStoreError> {
        if run_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows: Vec<(Uuid, String, Option<String>)> = sqlx::query_as(&format!(
            "SELECT r.run_id, r.status, {WAITING_FOR} FROM workflow_run r \
             WHERE r.tenant_id = $1 AND r.run_id = ANY($2)"
        ))
        .bind(&ctx.tenant_id)
        .bind(run_ids)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(rows
            .into_iter()
            .filter_map(|(id, status, waiting)| {
                let status = RunStatus::parse(&status)?;
                let waiting_for = waiting.as_deref().and_then(WaitingFor::parse);
                Some((
                    id,
                    LiveState {
                        status,
                        waiting_for,
                    },
                ))
            })
            .collect())
    }

    /// 本人が起動した run の結果と失敗の理由（上のモジュールの説明）。
    ///
    /// 節（`reply`）を組むのは `with_reply` に含めた run だけ（一覧の応答を小さく保つ）。他の run は
    /// `has_reply` だけを返す。
    pub async fn own_outcomes(
        &self,
        ctx: &authz::AuthContext,
        run_ids: &[Uuid],
        with_reply: &[Uuid],
    ) -> Result<HashMap<Uuid, OwnOutcome>, RunStoreError> {
        if run_ids.is_empty() {
            return Ok(HashMap::new());
        }
        // 失敗の理由は「最初の未処理の失敗 step」（error の出口を取っていない・map の要素ではない）の code。
        // map の要素の失敗で map が落ちたときは、要素の側の最初の失敗の code で分類する。
        let rows: Vec<OwnRow> = sqlx::query_as(&format!(
            "SELECT r.run_id, r.status, r.fail_reason, \
                CASE WHEN r.status = 'failed' THEN ( \
                    SELECT s.error->>'code' FROM step_execution s \
                    WHERE s.tenant_id = r.tenant_id AND s.run_id = r.run_id AND s.status = 'failed' \
                      AND s.taken_ports = '{{}}' AND strpos(s.step_path, '[') = 0 \
                    ORDER BY s.updated_at, s.step_path LIMIT 1) END, \
                CASE WHEN r.status = 'failed' THEN ( \
                    SELECT s.error->>'code' FROM step_execution s \
                    WHERE s.tenant_id = r.tenant_id AND s.run_id = r.run_id AND s.status = 'failed' \
                      AND strpos(s.step_path, '[') > 0 \
                    ORDER BY s.updated_at, s.step_path LIMIT 1) END, \
                EXISTS (SELECT 1 FROM step_execution s \
                        WHERE s.tenant_id = r.tenant_id AND s.run_id = r.run_id \
                          AND s.step_path = ANY(r.reply_nodes) AND s.status = 'succeeded') \
             FROM workflow_run r WHERE r.run_id = ANY($2) AND {OWN_RUN}"
        ))
        .bind(&ctx.tenant_id)
        .bind(run_ids)
        .bind(&ctx.principal.id)
        .bind(&ctx.org)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        let mut out: HashMap<Uuid, OwnOutcome> = HashMap::new();
        for (run_id, status, fail_reason, step_code, item_code, has_reply) in rows {
            let failure = (status == "failed").then(|| {
                let code = match step_code.as_deref() {
                    Some("map_item_failed" | "item_failed") | None => {
                        item_code.as_deref().or(step_code.as_deref())
                    }
                    other => other,
                };
                run_failure(run_id, code, fail_reason.as_deref())
            });
            out.insert(
                run_id,
                OwnOutcome {
                    failure,
                    has_reply,
                    reply: None,
                },
            );
        }
        let wanted: Vec<Uuid> = with_reply
            .iter()
            .copied()
            .filter(|id| out.get(id).is_some_and(|o| o.has_reply))
            .collect();
        for (run_id, reply) in self.replies(ctx, &wanted).await? {
            if let Some(o) = out.get_mut(&run_id) {
                o.reply = Some(reply);
            }
        }
        Ok(out)
    }

    /// run の節を実行順に組む（10 節・32KB まで。超えた分と読めない節は `omitted` に数える）。
    async fn replies(
        &self,
        ctx: &authz::AuthContext,
        run_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, RunReply>, RunStoreError> {
        if run_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows: Vec<(Uuid, Option<Json<Value>>)> = sqlx::query_as(&format!(
            "SELECT r.run_id, s.output FROM workflow_run r \
             JOIN step_execution s ON s.tenant_id = r.tenant_id AND s.run_id = r.run_id \
              AND s.step_path = ANY(r.reply_nodes) \
             WHERE r.run_id = ANY($2) AND {OWN_RUN} AND s.status = 'succeeded' \
             ORDER BY r.run_id, array_position(r.reply_nodes, s.step_path)"
        ))
        .bind(&ctx.tenant_id)
        .bind(run_ids)
        .bind(&ctx.principal.id)
        .bind(&ctx.org)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        let mut out: HashMap<Uuid, (RunReply, usize)> = HashMap::new();
        for (run_id, output) in rows {
            let (reply, used) = out.entry(run_id).or_default();
            let section = output.and_then(|j| serde_json::from_value::<ReplySection>(j.0).ok());
            let size = section
                .as_ref()
                .and_then(|s| serde_json::to_vec(s).ok())
                .map_or(usize::MAX, |v| v.len());
            match section {
                Some(s)
                    if reply.sections.len() < MAX_REPLY_SECTIONS
                        && used.saturating_add(size) <= MAX_REPLY_BYTES =>
                {
                    *used += size;
                    reply.sections.push(s);
                }
                _ => reply.omitted += 1,
            }
        }
        Ok(out.into_iter().map(|(id, (r, _))| (id, r)).collect())
    }
}

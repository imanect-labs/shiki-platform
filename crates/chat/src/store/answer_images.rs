//! 回答の画像プロキシの出所判定（`ChatStore` の一部・#1044）。
//!
//! 規則は [`crate::answer_images`]。ここは「どの本文を出所として数えるか」を、閲覧者の権限で決める:
//! - スレッドの viewer 認可（[`ChatStore::get_message`] が掛ける。拒否は監査に残る。許可された代理取得は
//!   監査に書かず、取得の失敗だけを `answer_image` のログに残す）。
//! - ツール結果は **run の実行者本人にだけ**数える（`get_messages` が他の閲覧者に伏せるのと同じ規則・
//!   #386）。他の閲覧者は Web の引用の抜粋に現れた画像だけ。
//! - 生成中はメッセージ本体がまだ空なので、進行中の run のイベント（ツール呼び出し・結果・引用）も見る。
//!   確定後は本体に同じものが写っているのでイベントは読まない（出所に無い URL の問い合わせで重い読み取りをさせない）。
//!   ただし**サブエージェントのツール実行は本体に写らない**（子のイベントは sink を通らず run のイベントへ直接書かれる・
//!   `worker/toolset.rs`）。本体にサブエージェントの呼び出しがある回答だけは、確定後もイベントを見る。

use uuid::Uuid;

use authz::AuthContext;
use sqlx::types::Json;

use super::threads::map_db;
use super::ChatStore;
use crate::answer_images::{
    collect_image_sources, page_texts_from_blocks, page_texts_from_events, resolve_source,
};
use crate::error::ChatError;
use crate::model::{ContentBlock, Role, RunStatus, StreamEventKind};

/// 出所を探すときに読むイベントの上限（1 回答のツール呼び出し・結果・引用）。
/// 極端に長い run でも 1 リクエストの読み取りを有界にする。
const MAX_SOURCE_EVENTS: i64 = 2_000;

impl ChatStore {
    /// 回答 `message_id` の出所に `requested` の画像があれば、**取得に使う**（出典に書かれていた）URL を返す。
    ///
    /// 出所に無ければ `Ok(None)`（プロキシは取得しない）。認可されていなければ Forbidden / NotFound。
    ///
    /// # Errors
    /// 認可の拒否・スレッド/メッセージが無い・DB の失敗。
    pub async fn answer_image_source(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        message_id: Uuid,
        requested: &str,
        trace_id: Option<&str>,
    ) -> Result<Option<String>, ChatError> {
        let message = self
            .get_message(ctx, thread_id, message_id, trace_id)
            .await?;
        if message.role != Role::Assistant {
            return Ok(None);
        }

        // この回答を生成した run と実行者。actor が一意に定まらなければ「本人と確定できない」側に倒す
        // （`redact_tool_results_for_viewer` と同じ fail-closed）。
        let runs: Vec<(Uuid, String, String)> = sqlx::query_as(
            "SELECT run_id, actor, status FROM generation_run WHERE message_id = $1 AND tenant_id = $2",
        )
        .bind(message_id)
        .bind(&ctx.tenant_id)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        let is_actor = !runs.is_empty() && runs.iter().all(|(_, a, _)| *a == ctx.principal.id);

        // 保存済みの本体（確定後）。見つかればイベントは読まない。
        let texts = page_texts_from_blocks(&message.content, is_actor);
        let sources = collect_image_sources(texts.iter().map(String::as_str));
        if let Some(found) = resolve_source(&sources, requested) {
            return Ok(Some(found));
        }
        // イベントを見るのは、生成中の run と、本体に写らないサブエージェントの実行を含む回答だけ。
        // 状態が読めない行は進行中とはみなさない。
        let delegated = message.content.iter().any(|b| {
            matches!(b, ContentBlock::ToolCall { name, .. }
                if name == agent_core::ToolName::Subagent.as_str())
        });
        let run_ids: Vec<Uuid> = runs
            .iter()
            .filter(|(_, _, status)| {
                delegated || RunStatus::parse(status).is_some_and(|s| !s.is_terminal())
            })
            .map(|(id, _, _)| *id)
            .collect();
        if run_ids.is_empty() {
            return Ok(None);
        }

        // 種別で絞り、件数を有界にする。
        let rows: Vec<(Json<StreamEventKind>,)> = sqlx::query_as(
            "SELECT payload FROM generation_event \
             WHERE tenant_id = $1 AND run_id = ANY($2) \
               AND type IN ('tool_call', 'tool_result', 'citation') \
             ORDER BY seq LIMIT $3",
        )
        .bind(&ctx.tenant_id)
        .bind(&run_ids)
        .bind(MAX_SOURCE_EVENTS)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        let events: Vec<StreamEventKind> = rows.into_iter().map(|(Json(e),)| e).collect();
        let texts = page_texts_from_events(&events, is_actor);
        let sources = collect_image_sources(texts.iter().map(String::as_str));
        Ok(resolve_source(&sources, requested))
    }
}

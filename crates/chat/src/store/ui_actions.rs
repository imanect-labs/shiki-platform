//! 単発 UI アクションの実行台帳（#410）。
//!
//! 「どのメッセージのどの action が実行されたか」を一級の状態として持つ。用途は 2 つ:
//!   ① 実行前に確保して**二重送信を入口で潰す**（[`ChatStore::claim_ui_action`]）
//!   ② メッセージ取得時に同梱してカードの「送信済み」表示の根拠にする
//!      （[`ChatStore::invoked_ui_actions`]）
//!
//! 確保（claim）と完了（complete）は分けてある。確保はハンドラ実行の**前**に取るので、
//! 「確保はしたが実行が終わっていない」中間状態が存在する。**確保は決して奪わない**
//! （後述）ので、行が 1 つでもあれば UI は送信済みとして扱う。
//!
//! ### 完了しない確保を引き継がない理由
//!
//! `chat.submit` は非冪等（実行のたびに発話と生成 run が増える）。確保を「古いから」と
//! 引き継ぐと、**post_message がコミットした直後に落ちた**ケースで副作用が二度走る
//! ——この台帳が防ぐはずのものそのものになる。副作用が起きたかどうかを外から確実に
//! 見分ける方法は無いので、詰まりの自己回復は諦め、**二度実行しない**方を取る。
//!
//! 確保だけ残って完了しなかった行の実害は「そのカードが押せないまま残る」ことに限られ、
//! 発話や run が壊れることはない（ユーザーは普通に発話すれば会話を続けられる）。
//! 完了できなかった事実は error ログに残し、`completed_at IS NULL` の古い行として
//! 運用側から見えるようにしてある（回復は当該行の削除）。
//!
//! 同じ事実は監査（`ui_action.invoke` の Allow）にも残るが、あれは追記専用の台帳で保持期間も
//! アクセス経路も別物なので、UI の描画根拠にはしない。

use std::collections::HashMap;

use authz::{AuthContext, Relation};
use uuid::Uuid;

use super::ChatStore;
use crate::ChatError;

/// 完了記録の再試行回数（1 回目を含む）。
///
/// ここを落とすと「実行されたのに未完了の行」が残る。実害は運用上の見え方だけだが、
/// 一過性の DB エラーで残すのはもったいないので数回粘る。
const COMPLETE_ATTEMPTS: u32 = 3;

impl ChatStore {
    /// 単発アクションを実行前に確保する。既に確保されていれば `false`（実行してはいけない）。
    ///
    /// 競合は主キーで潰す（`on conflict do nothing`）ため、同時押し・二重 POST でも実行に
    /// 進めるのは高々 1 つ。**一度取られた確保は奪わない**（モジュール冒頭の理由）。
    ///
    /// 認可は **thread editor** を要求する。単発束縛は `chat.submit`＝発話であり、実行時に
    /// `post_message` が editor を要求する。そこまで待つと**閲覧者でも確保だけは取れて**
    /// しまい、正規の編集者が 409 で弾かれる（実行が失敗して解放されるまでの間、共有相手が
    /// カードを塞げる）。副作用の無い認可はここで先に通す。
    pub async fn claim_ui_action(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        message_id: Uuid,
        action_id: &str,
        trace_id: Option<&str>,
    ) -> Result<bool, ChatError> {
        self.require_thread(
            ctx,
            thread_id,
            Relation::Editor,
            "thread.ui_action",
            trace_id,
        )
        .await?;
        // メッセージの実在（＋テナント一致）を insert 側で確かめる。存在しない id で
        // 台帳だけ作らせない（FK 違反ではなく 0 行として静かに落とす）。
        let claimed = sqlx::query(
            "INSERT INTO ui_action_invocation \
                 (tenant_id, thread_id, message_id, action_id, org, invoked_by) \
             SELECT $1, $2, $3, $4, $5, $6 \
             FROM message m \
             WHERE m.id = $3 AND m.thread_id = $2 AND m.tenant_id = $1 \
             ON CONFLICT DO NOTHING",
        )
        .bind(&ctx.tenant_id)
        .bind(thread_id)
        .bind(message_id)
        .bind(action_id)
        .bind(&ctx.org)
        .bind(&ctx.principal.id)
        .execute(&self.db)
        .await
        .map_err(|e| ChatError::Internal(format!("ui_action claim: {e}")))?;
        Ok(claimed.rows_affected() > 0)
    }

    /// 実行が完了したことを記録する（`run_id` があれば紐づける）。
    ///
    /// 一過性の DB エラーで未完了の行を残さないよう数回まで再試行する。
    pub async fn complete_ui_action(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        message_id: Uuid,
        action_id: &str,
        run_id: Option<Uuid>,
    ) -> Result<(), ChatError> {
        let mut last = None;
        for attempt in 1..=COMPLETE_ATTEMPTS {
            match self
                .mark_ui_action_complete(ctx, thread_id, message_id, action_id, run_id)
                .await
            {
                Ok(()) => return Ok(()),
                Err(e) => {
                    tracing::warn!(error = %e, action_id, attempt, "UI アクションの完了記録に失敗（再試行）");
                    last = Some(e);
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(50 * u64::from(attempt))).await;
        }
        Err(last.unwrap_or_else(|| ChatError::Internal("ui_action complete".into())))
    }

    async fn mark_ui_action_complete(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        message_id: Uuid,
        action_id: &str,
        run_id: Option<Uuid>,
    ) -> Result<(), ChatError> {
        sqlx::query(
            "UPDATE ui_action_invocation SET completed_at = now(), run_id = $5 \
             WHERE tenant_id = $1 AND thread_id = $2 AND message_id = $3 AND action_id = $4",
        )
        .bind(&ctx.tenant_id)
        .bind(thread_id)
        .bind(message_id)
        .bind(action_id)
        .bind(run_id)
        .execute(&self.db)
        .await
        .map_err(|e| ChatError::Internal(format!("ui_action complete: {e}")))?;
        Ok(())
    }

    /// 確保を解く（実行に失敗したときだけ・押し直せる状態へ戻す）。
    pub async fn release_ui_action(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        message_id: Uuid,
        action_id: &str,
    ) -> Result<(), ChatError> {
        // 完了済みの行は消さない（別の実行を巻き込まないための保険）。
        sqlx::query(
            "DELETE FROM ui_action_invocation \
             WHERE tenant_id = $1 AND thread_id = $2 AND message_id = $3 AND action_id = $4 \
               AND completed_at IS NULL",
        )
        .bind(&ctx.tenant_id)
        .bind(thread_id)
        .bind(message_id)
        .bind(action_id)
        .execute(&self.db)
        .await
        .map_err(|e| ChatError::Internal(format!("ui_action release: {e}")))?;
        Ok(())
    }

    /// スレッド内の確保済み action を message ごとに引く（主キー先頭 2 列でそのまま効く）。
    ///
    /// 完了前の行も含める。確保は奪われないので、行があるカードは**もう押せない**——
    /// そこで「未送信」と描くと、押せないのに押せそうに見える（＝直そうとしている症状に
    /// 戻る）。実行中の数百 ms も送信済みに見えるが、それは事実として正しい。
    pub(super) async fn invoked_ui_actions(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
    ) -> Result<HashMap<Uuid, Vec<String>>, ChatError> {
        let rows: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT message_id, action_id FROM ui_action_invocation \
             WHERE tenant_id = $1 AND thread_id = $2 \
             ORDER BY message_id, action_id",
        )
        .bind(&ctx.tenant_id)
        .bind(thread_id)
        .fetch_all(&self.db)
        .await
        .map_err(|e| ChatError::Internal(format!("ui_action list: {e}")))?;
        let mut by_message: HashMap<Uuid, Vec<String>> = HashMap::new();
        for (message_id, action_id) in rows {
            by_message.entry(message_id).or_default().push(action_id);
        }
        Ok(by_message)
    }
}

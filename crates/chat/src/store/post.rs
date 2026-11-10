//! `ChatStore::post_message` — 発話投入の Transactional Outbox（Task 3.11・design §4.4.1）。
//!
//! message 保存＋run 行＋jobq enqueue を**単一 Postgres TX**で行い、202 で run_id を即返す
//! （同期実行しない）。claim/リース/fencing 等の run 操作は [`super::runs`]。

#[allow(clippy::wildcard_imports)]
use super::*;
use storage::audit::AuditAction;

use authz::{AuthContext, Relation};
use serde_json::json;
use sqlx::types::Json;
use uuid::Uuid;

use super::runs::{map_db, CHAT_GENERATION_QUEUE};
use crate::model::{Attachment, ContentBlock, RunModel, SkillPin};

/// `post_message` の結果（202 で返す）。
#[derive(Debug, Clone)]
pub struct PostResult {
    pub run_id: Uuid,
    pub user_message_id: Uuid,
    pub assistant_message_id: Uuid,
    pub agent_mode: bool,
}

impl ChatStore {
    /// ユーザー発話を投入する（**単一 TX**で user/assistant message 保存＋run 行＋jobq enqueue）。
    ///
    /// editor 権限を要求する（viewer は投稿不可）。同期実行はせず run_id を返す（202・Task 3.5）。
    #[allow(clippy::too_many_arguments)] // 発話の全構成要素（呼び出し元は api 1 箇所＋UI アクション）。
    pub async fn post_message(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        text: &str,
        attachments: &[Attachment],
        selection: Option<crate::model::SelectionContext>,
        agent_mode_override: Option<bool>,
        autonomous: bool,
        once_skills: &[SkillPin],
        trace_id: Option<&str>,
    ) -> Result<PostResult, ChatError> {
        self.post_message_with_model(
            ctx,
            thread_id,
            text,
            attachments,
            selection,
            agent_mode_override,
            autonomous,
            once_skills,
            None,
            &[],
            trace_id,
        )
        .await
    }

    /// [`Self::post_message`] にモデル指定（#553）と発話ごとのツール指定（#734）を足したもの。
    /// `model` が `None` の run は従来どおり（skill のピン → 既定モデル）で生成される。
    #[allow(clippy::too_many_arguments)] // 発話の全構成要素（呼び出し元は api 1 箇所＋UI アクション）。
    pub async fn post_message_with_model(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        text: &str,
        attachments: &[Attachment],
        selection: Option<crate::model::SelectionContext>,
        agent_mode_override: Option<bool>,
        autonomous: bool,
        once_skills: &[SkillPin],
        model: Option<&RunModel>,
        requested_tools: &[agent_core::ToolName],
        trace_id: Option<&str>,
    ) -> Result<PostResult, ChatError> {
        self.post_message_on_branch(
            ctx,
            thread_id,
            text,
            attachments,
            selection,
            agent_mode_override,
            autonomous,
            once_skills,
            model,
            requested_tools,
            BranchTarget::default(),
            trace_id,
        )
        .await
    }

    /// [`Self::post_message_with_model`] に**編集元の発話**（`edit_of`・#1166）を足したもの。
    ///
    /// `edit_of` が無ければ表示中の枝の葉に続ける。あれば編集元と同じ親に新しい発話を置く
    /// （編集元とその続きは消さずに別の枝として残る）。どちらでも新しい応答が表示中の枝になる。
    #[allow(clippy::too_many_arguments)] // 発話の全構成要素（呼び出し元は api 1 箇所）。
    pub async fn post_message_on_branch(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        text: &str,
        attachments: &[Attachment],
        // エディタの選択コンテキスト（Task 11.10）。node_id の可読性検証は api 層の責務
        // （storage.get_metadata の viewer 判定・監査つき）。ここでは上限の切り詰めのみ行う。
        selection: Option<crate::model::SelectionContext>,
        agent_mode_override: Option<bool>,
        autonomous: bool,
        // `once_skills`: **この run にだけ**適用する skill（スラッシュコマンド起動・#387）。
        // thread のピン（＝「最初からロード済み」）とは別で、次の発話には持ち越さない
        // （積み上がる「専門家モード」ではなく「1 発の指示」という体験にするため）。
        once_skills: &[SkillPin],
        // モデル指定（API がテナント設定で解決・検証済み）。run にスナップショットする。
        model: Option<&RunModel>,
        // `requested_tools`: ユーザーが**この発話にだけ**使ってほしいと選んだツール（#734）。
        // 語彙照合・重複除去・件数上限は API 層で済んでいる。run にスナップショットするだけで、
        // 提示ツール・承認・認可は変えない（ワーカーが誘導＝常時ロード＋注記に使う）。
        requested_tools: &[agent_core::ToolName],
        // 親の指定（#1166）: 編集元の user 発話、または続ける先（画面で表示している枝の末尾）。
        branch: BranchTarget,
        trace_id: Option<&str>,
    ) -> Result<PostResult, ChatError> {
        let post = AuditAction::ThreadPost;
        self.require_thread(ctx, thread_id, Relation::Editor, post, trace_id)
            .await?;
        let text = text.trim();
        if text.is_empty() && attachments.is_empty() {
            return Err(ChatError::Invalid("empty message".into()));
        }

        // 実効エージェントモード（メッセージ上書き or スレッド既定）＋承認モード＋skill/
        // ミニアプリのピン（thread のピンを run へ **jsonb スナップショット**でコピーする・
        // 0027 の autonomous と同パターン。run 行が生成材料の単一ソース＝claim が 1 行で完結する・#344）。
        /// thread の生成材料（agent_mode 既定＋承認モード＋skill ピン集合＋mini_app のピン）。
        type ThreadDefaults = (bool, String, Json<Vec<SkillPin>>, Option<Uuid>, Option<i64>);
        let thread_row: Option<ThreadDefaults> = sqlx::query_as(
            "SELECT agent_mode, autonomous_mode, \
                    (SELECT coalesce(jsonb_agg(jsonb_build_object( \
                        'skill_id', p.skill_id, 'skill_version', p.skill_version) \
                        ORDER BY p.position, p.skill_id), '[]'::jsonb) \
                     FROM thread_skill_pin p WHERE p.thread_id = thread.id) AS skill_pins, \
                    mini_app_id, mini_app_version \
             FROM thread WHERE id = $1 AND tenant_id = $2",
        )
        .bind(thread_id)
        .bind(ctx.tenant_id())
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        let (thread_default, autonomous_mode, mut skill_pins, mini_app_id, mini_app_version) =
            thread_row.ok_or(ChatError::NotFound)?;
        // run 単位の skill を**スナップショットへ追記**する（thread のピンは変えない）。
        // run 行が生成材料の単一ソースなので、ここに載れば claim 1 行で解決でき、
        // 適用の認可・fail-closed も既存のピン経路（AppliedSkill::load_pins）がそのまま効く。
        for once in once_skills {
            // 同一 skill が thread に古い version でピン済みでも、**発話単位の指定を優先**する
            // （そうしないと「version 指定」「省略なら current」という要求が run へ届かない）。
            // thread の永続ピンそのものは変更しない。
            if let Some(existing) = skill_pins
                .0
                .iter_mut()
                .find(|p| p.skill_id == once.skill_id)
            {
                *existing = once.clone();
            } else {
                skill_pins.0.push(once.clone());
            }
        }
        // 自律プロファイルはエージェントモードを含意する（ツールループが前提）。
        let agent_mode = agent_mode_override.unwrap_or(thread_default) || autonomous;

        // user メッセージ content: 選択コンテキスト＋添付（file_ref）＋text。
        let mut user_content: Vec<ContentBlock> = Vec::new();
        if let Some(selection) = selection {
            user_content.push(ContentBlock::SelectionContext {
                context: selection.clamped(),
            });
        }
        user_content.extend(attachments.iter().map(|a| ContentBlock::FileRef {
            node_id: a.node_id.clone(),
            name: a.name.clone(),
        }));
        if !text.is_empty() {
            user_content.push(ContentBlock::Text {
                text: text.to_string(),
            });
        }

        let mut tx = self.db.begin().await.map_err(map_db)?;
        let active = touch_live_thread(&mut tx, thread_id, ctx.tenant_id()).await?;
        // 親を決める（スレッドの行ロックの後＝並行する投稿・枝の切り替えと直列）。
        let parent = parent_for_post(&mut tx, thread_id, ctx.tenant_id(), active, branch).await?;
        let user_id: Uuid = sqlx::query_scalar(
            "INSERT INTO message (thread_id, org, tenant_id, role, content, agent_mode, parent_id) \
             VALUES ($1, $2, $3, 'user', $4, $5, $6) RETURNING id",
        )
        .bind(thread_id)
        .bind(ctx.org())
        .bind(ctx.tenant_id())
        .bind(Json(&user_content))
        .bind(agent_mode)
        .bind(parent)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db)?;

        let asst_id: Uuid = sqlx::query_scalar(
            "INSERT INTO message (thread_id, org, tenant_id, role, content, agent_mode, parent_id) \
             VALUES ($1, $2, $3, 'assistant', '[]'::jsonb, $4, $5) RETURNING id",
        )
        .bind(thread_id)
        .bind(ctx.org())
        .bind(ctx.tenant_id())
        .bind(agent_mode)
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db)?;
        // 新しい応答を表示中の枝にする（編集で作った枝へ画面が移る）。
        super::branches::set_active_leaf(&mut tx, thread_id, ctx.tenant_id(), asst_id).await?;

        // 承認モードは投入時点の thread 値をスナップショットする（発話者はモードを見て投稿する
        // ＝その水準での実行に同意・実行中の緩和は actor 本人設定のみ有効・#350）。
        let run_id: Uuid = sqlx::query_scalar(
            "INSERT INTO generation_run (message_id, thread_id, org, tenant_id, actor, agent_mode, status, trace_id, autonomous, \
                                         autonomous_mode, skill_pins, mini_app_id, mini_app_version, \
                                         model, effort, model_auto, requested_tools, requested_choice) \
             VALUES ($1, $2, $3, $4, $5, $6, 'queued', $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17) RETURNING run_id",
        )
        .bind(asst_id)
        .bind(thread_id)
        .bind(ctx.org())
        .bind(ctx.tenant_id())
        .bind(&ctx.principal().id)
        .bind(agent_mode)
        .bind(trace_id)
        .bind(autonomous)
        .bind(&autonomous_mode)
        .bind(&skill_pins)
        .bind(mini_app_id)
        .bind(mini_app_version)
        .bind(model.map(|m| m.model.as_str()))
        .bind(model.and_then(|m| m.effort).map(llm_gateway::Effort::as_str))
        .bind(model.is_none_or(|m| m.auto))
        .bind(Json(requested_tools))
        .bind(model.and_then(|m| m.requested.as_ref()).map(Json))
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db)?;

        // outbox: 同一 TX で jobq へ生成ジョブを enqueue（run_id を payload）。
        jobq::enqueue_on(
            &mut tx,
            jobq::NewJob {
                queue: CHAT_GENERATION_QUEUE,
                tenant_id: ctx.tenant_id(),
                payload: &json!({ "run_id": run_id }),
                trace_id,
                max_attempts: 3,
            },
        )
        .await
        .map_err(|e| ChatError::Internal(format!("enqueue: {e}")))?;

        tx.commit().await.map_err(map_db)?;
        Ok(PostResult {
            run_id,
            user_message_id: user_id,
            assistant_message_id: asst_id,
            agent_mode,
        })
    }
}

/// 新しい発話をどこに置くか（#1166）。既定は表示中の枝の続き。
#[derive(Debug, Clone, Copy, Default)]
pub struct BranchTarget {
    /// 編集して送り直す元の user 発話（同じ親に新しい版を置く）。
    pub edit_of: Option<Uuid>,
    /// 続ける先のメッセージ（送った人の画面に出ていた枝の末尾）。別のタブ・共同編集者が
    /// 枝を切り替えていても、送った人が見ていた会話に続ける。生成中は無視して順番待ちにする。
    pub after: Option<Uuid>,
}

/// 新しい user 発話の親（編集なら編集元の親、続ける先の指定があればそこ、無ければ表示中の枝の葉）。
async fn parent_for_post(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    thread_id: Uuid,
    tenant_id: &str,
    active: Option<Uuid>,
    branch: BranchTarget,
) -> Result<Option<Uuid>, ChatError> {
    use super::branches::{busy_error, has_unfinished_run, load_tree};
    let busy = has_unfinished_run(tx, thread_id, tenant_id).await?;
    if let Some(edit_of) = branch.edit_of {
        if busy {
            return Err(busy_error());
        }
        // 編集できるのは同じスレッドの user 発話だけ（応答や他スレッドの id は存在秘匿で 404）。
        let row: Option<Option<Uuid>> = sqlx::query_scalar(
            "SELECT parent_id FROM message \
             WHERE id = $1 AND thread_id = $2 AND tenant_id = $3 AND role = 'user'",
        )
        .bind(edit_of)
        .bind(thread_id)
        .bind(tenant_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_db)?;
        return row.ok_or(ChatError::NotFound);
    }
    // 続ける先（生成中は表示中の枝の後ろへ順番待ちにするので使わない）。
    let after = branch.after.filter(|_| !busy);
    // 多くの投稿は「指定（または表示中）の葉に子が無い」ので、木を引かずに 1 問で確かめる。
    if let Some(leaf) = after.or(active) {
        let (exists, has_child): (bool, bool) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM message WHERE id = $1 AND thread_id = $2 AND tenant_id = $3), \
                    EXISTS (SELECT 1 FROM message WHERE thread_id = $2 AND parent_id = $1 AND tenant_id = $3)",
        )
        .bind(leaf)
        .bind(thread_id)
        .bind(tenant_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(map_db)?;
        if after.is_some() {
            // 続ける先はそのまま親にする（子があれば新しい枝になる）。無ければ 404。
            return if exists {
                Ok(after)
            } else {
                Err(ChatError::NotFound)
            };
        }
        if exists && !has_child {
            return Ok(Some(leaf));
        }
    }
    let (tree, active) = load_tree(tx, thread_id, tenant_id).await?;
    Ok(tree.active_leaf(active))
}

/// スレッドの `updated_at` を更新して行ロックを取る（削除と直列化する・投稿の最初に呼ぶ）。
///
/// 削除が先に確定していれば 0 行で NotFound。こちらが先なら削除は commit を待ってから、
/// ここで積む run も停止要求の対象にする（[`ChatStore::delete_thread`]）。
///
/// 返り値は表示中の枝の葉の指定（`thread.active_message_id`・#1166）。
async fn touch_live_thread(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    thread_id: Uuid,
    tenant_id: &str,
) -> Result<Option<Uuid>, ChatError> {
    let touched: Option<Option<Uuid>> = sqlx::query_scalar(
        "UPDATE thread SET updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 AND deleted_at IS NULL \
         RETURNING active_message_id",
    )
    .bind(thread_id)
    .bind(tenant_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_db)?;
    touched.ok_or(ChatError::NotFound)
}

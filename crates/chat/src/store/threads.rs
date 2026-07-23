//! `ChatStore`: thread / message CRUD・線形取得（Task 3.1）。共有系は [`super::sharing`]。

#[allow(clippy::wildcard_imports)]
use super::*;

use authz::{AuthContext, Consistency, Relation};
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::types::Json;
use storage::audit::{AuditEntry, Decision};
use uuid::Uuid;

use crate::model::{AutonomousMode, ContentBlock, Message, Role, Thread};

/// thread 行。
#[derive(sqlx::FromRow)]
struct ThreadRow {
    id: Uuid,
    title: String,
    agent_mode: bool,
    autonomous_mode: String,
    skill_id: Option<Uuid>,
    skill_version: Option<i64>,
    mini_app_id: Option<Uuid>,
    mini_app_version: Option<i64>,
    origin_note_id: Option<Uuid>,
    origin_note_name: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl ThreadRow {
    fn into_thread(self) -> Thread {
        Thread {
            id: self.id,
            title: self.title,
            agent_mode: self.agent_mode,
            // CHECK 制約で閉じている（乖離時は既定＝承認必須へ・fail-closed）。
            autonomous_mode: AutonomousMode::parse(&self.autonomous_mode).unwrap_or_default(),
            skill_id: self.skill_id,
            skill_version: self.skill_version,
            mini_app_id: self.mini_app_id,
            mini_app_version: self.mini_app_version,
            origin_note_id: self.origin_note_id,
            origin_note_name: self.origin_note_name,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// スレッドの由来ノート（ノートの分割ビューから作られたスレッド・issue #282）。
#[derive(Debug, Clone)]
pub struct ThreadOrigin {
    pub note_id: Uuid,
    pub note_name: String,
}

/// message 行。
#[derive(sqlx::FromRow)]
struct MessageRow {
    id: Uuid,
    role: String,
    content: Json<Vec<ContentBlock>>,
    agent_mode: bool,
    parent_id: Option<Uuid>,
    created_at: DateTime<Utc>,
}

impl ChatStore {
    /// スレッドを新規作成する（作成者を owner タプルで付与）。
    pub async fn create_thread(
        &self,
        ctx: &AuthContext,
        title: &str,
        agent_mode: bool,
        origin: Option<ThreadOrigin>,
        trace_id: Option<&str>,
    ) -> Result<Thread, ChatError> {
        let id = Uuid::new_v4();
        let title = {
            let t = title.trim();
            if t.is_empty() {
                "新しいチャット"
            } else {
                t
            }
        };
        let (origin_note_id, origin_note_name) = match &origin {
            Some(o) => (Some(o.note_id), Some(o.note_name.as_str())),
            None => (None, None),
        };
        let row: ThreadRow = sqlx::query_as(
            "INSERT INTO thread (id, org, tenant_id, owner, title, agent_mode, origin_note_id, origin_note_name) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             RETURNING id, title, agent_mode, autonomous_mode, skill_id, skill_version, mini_app_id, mini_app_version, origin_note_id, origin_note_name, created_at, updated_at",
        )
        .bind(id)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .bind(&ctx.principal.id)
        .bind(title)
        .bind(agent_mode)
        .bind(origin_note_id)
        .bind(origin_note_name)
        .fetch_one(&self.db)
        .await
        .map_err(map_db)?;

        // 作成者を owner に（FGA）。失敗したら行を補償削除して漏れを残さない。
        let obj = ctx.ns().thread(&id.to_string());
        if let Err(e) = self
            .authz
            .write_tuple(&ctx.subject(), Relation::Owner, &obj)
            .await
        {
            let _ = sqlx::query("DELETE FROM thread WHERE id = $1 AND tenant_id = $2")
                .bind(id)
                .bind(&ctx.tenant_id)
                .execute(&self.db)
                .await;
            return Err(ChatError::Internal(format!("owner tuple: {e}")));
        }
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: "thread.create",
                    object_type: "thread",
                    object_id: &id.to_string(),
                    decision: Decision::Allow,
                    trace_id,
                    metadata: json!({ "agent_mode": agent_mode, "origin_note_id": origin_note_id }),
                },
            )
            .await
            .map_err(map_storage)?;
        Ok(row.into_thread())
    }

    /// スレッドに skill / ミニアプリのバージョンピンを設定する（作成直後・owner のみ・Task 6.7/6.10）。
    ///
    /// 参照の存在・kind・viewer 検証は API 層（SkillStore/MiniAppStore）が**設定者の権限**で
    /// 済ませてから呼ぶこと。ピンは再現性のため version 込みで固定される。
    pub async fn set_thread_pins(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        skill: Option<(Uuid, i64)>,
        mini_app: Option<(Uuid, i64)>,
        trace_id: Option<&str>,
    ) -> Result<(), ChatError> {
        self.require_thread(ctx, thread_id, Relation::Owner, "thread.set_pins", trace_id)
            .await?;
        let updated = sqlx::query(
            "UPDATE thread SET skill_id = $3, skill_version = $4, \
                    mini_app_id = $5, mini_app_version = $6, updated_at = now() \
             WHERE id = $1 AND tenant_id = $2 AND deleted_at IS NULL",
        )
        .bind(thread_id)
        .bind(&ctx.tenant_id)
        .bind(skill.map(|(id, _)| id))
        .bind(skill.map(|(_, v)| v))
        .bind(mini_app.map(|(id, _)| id))
        .bind(mini_app.map(|(_, v)| v))
        .execute(&self.db)
        .await
        .map_err(map_db)?;
        if updated.rows_affected() == 0 {
            return Err(ChatError::NotFound);
        }
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: "thread.set_pins",
                    object_type: "thread",
                    object_id: &thread_id.to_string(),
                    decision: Decision::Allow,
                    trace_id,
                    metadata: json!({
                        "skill": skill.map(|(id, v)| json!({ "artifact_id": id, "version": v })),
                        "mini_app": mini_app.map(|(id, v)| json!({ "artifact_id": id, "version": v })),
                    }),
                },
            )
            .await
            .map_err(map_storage)?;
        Ok(())
    }

    /// スレッドの由来ノートを（後付けで）設定する（下書き確定→ノート実体化の紐付け・issue #282）。
    ///
    /// チャットで作った下書きを「ドライブに保存」したとき、その会話を新しく実体化したノートへ
    /// 紐付けて「ノート由来」にする。owner のみ（自分の会話の紐付け）。ノートの存在/閲覧可否は
    /// API 層が**発話ユーザーの viewer 権限**で検証してから呼ぶこと（見えないノートに紐づけない）。
    pub async fn set_thread_origin_note(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        note_id: Uuid,
        note_name: &str,
        trace_id: Option<&str>,
    ) -> Result<(), ChatError> {
        self.require_thread(
            ctx,
            thread_id,
            Relation::Owner,
            "thread.set_origin",
            trace_id,
        )
        .await?;
        let updated = sqlx::query(
            "UPDATE thread SET origin_note_id = $3, origin_note_name = $4, updated_at = now() \
             WHERE id = $1 AND tenant_id = $2 AND deleted_at IS NULL",
        )
        .bind(thread_id)
        .bind(&ctx.tenant_id)
        .bind(note_id)
        .bind(note_name)
        .execute(&self.db)
        .await
        .map_err(map_db)?;
        if updated.rows_affected() == 0 {
            return Err(ChatError::NotFound);
        }
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: "thread.set_origin",
                    object_type: "thread",
                    object_id: &thread_id.to_string(),
                    decision: Decision::Allow,
                    trace_id,
                    metadata: json!({ "origin_note_id": note_id.to_string() }),
                },
            )
            .await
            .map_err(map_storage)?;
        Ok(())
    }

    /// 自分が owner のスレッド一覧（更新日降順・keyset ページング）。
    ///
    /// `origin_note_id` を渡すと当該ノート由来のスレッドのみに絞る（ノート側の会話一覧・issue #282）。
    /// 未指定（None）はサイドバー履歴の全件（現行挙動）。
    pub async fn list_threads(
        &self,
        ctx: &AuthContext,
        before_updated_at: Option<DateTime<Utc>>,
        before_id: Option<Uuid>,
        origin_note_id: Option<Uuid>,
        limit: i64,
    ) -> Result<Vec<Thread>, ChatError> {
        let limit = limit.clamp(1, 100);
        let rows: Vec<ThreadRow> = sqlx::query_as(
            "SELECT id, title, agent_mode, autonomous_mode, skill_id, skill_version, mini_app_id, mini_app_version, origin_note_id, origin_note_name, created_at, updated_at FROM thread \
             WHERE tenant_id = $1 AND org = $2 AND owner = $3 AND deleted_at IS NULL \
               AND ($4::timestamptz IS NULL OR (updated_at, id) < ($4::timestamptz, $5)) \
               AND ($7::uuid IS NULL OR origin_note_id = $7) \
             ORDER BY updated_at DESC, id DESC LIMIT $6",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(&ctx.principal.id)
        .bind(before_updated_at)
        .bind(before_id)
        .bind(limit)
        .bind(origin_note_id)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(rows.into_iter().map(ThreadRow::into_thread).collect())
    }

    /// スレッドを取得する（viewer 認可・剥奪即時反映のため HigherConsistency）。
    pub async fn get_thread(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Thread, ChatError> {
        self.require_thread(ctx, thread_id, Relation::Viewer, "thread.get", trace_id)
            .await?;
        let row: Option<ThreadRow> = sqlx::query_as(
            "SELECT id, title, agent_mode, autonomous_mode, skill_id, skill_version, mini_app_id, mini_app_version, origin_note_id, origin_note_name, created_at, updated_at FROM thread \
             WHERE id = $1 AND tenant_id = $2 AND deleted_at IS NULL",
        )
        .bind(thread_id)
        .bind(&ctx.tenant_id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        row.map(ThreadRow::into_thread).ok_or(ChatError::NotFound)
    }

    /// スレッドのメッセージを線形取得する（viewer 認可・作成順）。
    pub async fn get_messages(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Vec<Message>, ChatError> {
        self.require_thread(
            ctx,
            thread_id,
            Relation::Viewer,
            "thread.messages",
            trace_id,
        )
        .await?;
        let rows: Vec<MessageRow> = sqlx::query_as(
            "SELECT id, role, content, agent_mode, parent_id, created_at FROM message \
             WHERE thread_id = $1 AND tenant_id = $2 ORDER BY created_at, id",
        )
        .bind(thread_id)
        .bind(&ctx.tenant_id)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        let mut messages: Vec<Message> = rows
            .into_iter()
            .map(|r| {
                Ok(Message {
                    id: r.id,
                    role: Role::parse(&r.role)
                        .ok_or_else(|| ChatError::Internal(format!("bad role: {}", r.role)))?,
                    content: r.content.0,
                    agent_mode: r.agent_mode,
                    parent_id: r.parent_id,
                    created_at: r.created_at,
                })
            })
            .collect::<Result<_, ChatError>>()?;

        // 共有されたスレッドの閲覧は**閲覧者自身の権限で引用を再評価**する（#37・
        // 「他人の引用をそのまま見せない」）。閲覧者が読めない引用チャンクは落とす
        // （所有者は自分の引用を全て読めるため実質そのまま）。
        self.filter_citations_for_viewer(ctx, &mut messages).await?;
        Ok(messages)
    }

    /// 単一メッセージを取得する（thread viewer 認可・UI アクションの束縛照合用・Task 6.5）。
    pub async fn get_message(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        message_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Message, ChatError> {
        self.require_thread(ctx, thread_id, Relation::Viewer, "thread.message", trace_id)
            .await?;
        let row: Option<MessageRow> = sqlx::query_as(
            "SELECT id, role, content, agent_mode, parent_id, created_at FROM message \
             WHERE id = $1 AND thread_id = $2 AND tenant_id = $3",
        )
        .bind(message_id)
        .bind(thread_id)
        .bind(&ctx.tenant_id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        let r = row.ok_or(ChatError::NotFound)?;
        Ok(Message {
            id: r.id,
            role: Role::parse(&r.role)
                .ok_or_else(|| ChatError::Internal(format!("bad role: {}", r.role)))?,
            content: r.content.0,
            agent_mode: r.agent_mode,
            parent_id: r.parent_id,
            created_at: r.created_at,
        })
    }

    /// 各メッセージの citation ブロックを閲覧者の viewer 権限で再評価し、読めない引用を落とす。
    async fn filter_citations_for_viewer(
        &self,
        ctx: &AuthContext,
        messages: &mut [Message],
    ) -> Result<(), ChatError> {
        use std::collections::HashMap;
        // 引用対象ファイルの重複を除いて一括判定（同一ファイルの複数引用を一度に）。
        let mut decisions: HashMap<String, bool> = HashMap::new();
        for m in messages.iter() {
            for b in &m.content {
                if let ContentBlock::Citation(c) = b {
                    if !decisions.contains_key(&c.node_id) {
                        let allowed = self.can_view_file(ctx, &c.node_id).await;
                        decisions.insert(c.node_id.clone(), allowed);
                    }
                }
            }
        }
        if decisions.values().all(|v| *v) {
            return Ok(()); // 全て閲覧可（所有者/十分な権限）なら何もしない
        }
        for m in messages.iter_mut() {
            m.content.retain(|b| match b {
                ContentBlock::Citation(c) => *decisions.get(&c.node_id).unwrap_or(&false),
                _ => true,
            });
        }
        Ok(())
    }

    /// 閲覧者が該当ファイルを閲覧できるか（citation 再評価用・失敗時は保守的に false）。
    async fn can_view_file(&self, ctx: &AuthContext, node_id: &str) -> bool {
        let obj = ctx.ns().file(node_id);
        self.authz
            .check(
                &ctx.subject(),
                Relation::Viewer,
                &obj,
                Consistency::MinimizeLatency,
            )
            .await
            .unwrap_or(false)
    }

    /// スレッドへの relation を要求し、FGA object を返す（不足は監査 deny＋Forbidden）。
    pub(super) async fn require_thread(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        relation: Relation,
        action: &str,
        trace_id: Option<&str>,
    ) -> Result<authz::FgaObject, ChatError> {
        let obj = ctx.ns().thread(&thread_id.to_string());
        let ok = self
            .authz
            .check(
                &ctx.subject(),
                relation,
                &obj,
                Consistency::HigherConsistency,
            )
            .await
            .map_err(|e| ChatError::Internal(e.to_string()))?;
        if !ok {
            let _ = self
                .audit
                .record(
                    ctx,
                    AuditEntry {
                        action,
                        object_type: "thread",
                        object_id: &thread_id.to_string(),
                        decision: Decision::Deny,
                        trace_id,
                        metadata: json!({ "relation": relation.as_str() }),
                    },
                )
                .await;
            return Err(ChatError::Forbidden);
        }
        Ok(obj)
    }
}

#[allow(clippy::needless_pass_by_value)]
fn map_db(e: sqlx::Error) -> ChatError {
    ChatError::Internal(format!("db: {e}"))
}

#[allow(clippy::needless_pass_by_value)]
pub(super) fn map_storage(e: storage::StorageError) -> ChatError {
    ChatError::Internal(format!("audit: {e}"))
}

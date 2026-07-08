//! `ChatStore`: スレッド共有（ReBAC タプル付与/剥奪・一覧）と自律ワークスペースの権限伝播（Task 3.7 / 5.6(a)）。
//!
//! `threads.rs`（thread CRUD）から共有系を分離。struct/フィールド/自由関数は `use super::*` で総取りする。

#[allow(clippy::wildcard_imports)]
use super::*;

use authz::{AuthContext, Relation};
use serde_json::json;
use storage::audit::{AuditEntry, Decision};
use storage::model::ShareTarget;
use uuid::Uuid;

use super::threads::map_storage;
use crate::model::ThreadRole;

impl ChatStore {
    /// スレッドを共有する（owner 権限・viewer/commenter/editor）。監査失敗時は付与を補償剥奪。
    ///
    /// editor 共有時は**自律ワークスペースフォルダにも editor を伝播**する（Task 5.6(a)）。共有相手の
    /// 自律 run がワークスペース（起案者所有の Drive フォルダ）を読み書きできるようにするため。
    pub async fn share_thread(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        target: &ShareTarget,
        role: ThreadRole,
        trace_id: Option<&str>,
    ) -> Result<(), ChatError> {
        let obj = self
            .require_thread(ctx, thread_id, Relation::Owner, "thread.share", trace_id)
            .await?;
        let subject = target.subject(&ctx.ns());
        let granted = self
            .authz
            .write_tuple(&subject, role.relation(), &obj)
            .await
            .map_err(|e| ChatError::Internal(e.to_string()))?;
        // editor だけが自律 run を起こせる（post は editor 必須）ため editor のみワークスペースへ伝播。
        let ws_granted = if matches!(role, ThreadRole::Editor) {
            self.set_workspace_editor(ctx, thread_id, &subject, true)
                .await?
        } else {
            false
        };
        if let Err(e) = self
            .record_share_audit(ctx, thread_id, "thread.share", target, role, trace_id)
            .await
        {
            if granted {
                let _ = self
                    .authz
                    .delete_tuple(&subject, role.relation(), &obj)
                    .await;
            }
            if ws_granted {
                let _ = self
                    .set_workspace_editor(ctx, thread_id, &subject, false)
                    .await;
            }
            return Err(e);
        }
        Ok(())
    }

    /// thread の自律ワークスペースフォルダへ **editor** を付与/剥奪する（Task 5.6(a)）。
    ///
    /// workspace 未作成なら no-op（`false`）。thread のタプルと同じく chat が直接 FGA を操作する既存
    /// パターンに乗る（フォルダは起案者所有のため storage 側 `share_node`＝owner ゲートは通せない）。
    /// **egress/破壊操作の実行権限ではなくフォルダ read/write の ReBAC** であり、実行時は各ユーザーの
    /// AuthContext を素通す（confused-deputy 防御は維持）。
    pub(crate) async fn set_workspace_editor(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        subject: &authz::Subject,
        grant: bool,
    ) -> Result<bool, ChatError> {
        let Some(folder_id) = self.workspace_folder_id(thread_id, &ctx.tenant_id).await? else {
            return Ok(false);
        };
        let folder = ctx.ns().folder(&folder_id.to_string());
        let changed = if grant {
            self.authz
                .write_tuple(subject, Relation::Editor, &folder)
                .await
        } else {
            self.authz
                .delete_tuple(subject, Relation::Editor, &folder)
                .await
        }
        .map_err(|e| ChatError::Internal(e.to_string()))?;
        Ok(changed)
    }

    /// 共有を解除する（owner 権限・冪等）。監査失敗時は剥奪を補償付与。
    pub async fn unshare_thread(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        target: &ShareTarget,
        role: ThreadRole,
        trace_id: Option<&str>,
    ) -> Result<(), ChatError> {
        let obj = self
            .require_thread(ctx, thread_id, Relation::Owner, "thread.unshare", trace_id)
            .await?;
        let subject = target.subject(&ctx.ns());
        let revoked = self
            .authz
            .delete_tuple(&subject, role.relation(), &obj)
            .await
            .map_err(|e| ChatError::Internal(e.to_string()))?;
        // editor 剥奪時は自律ワークスペースフォルダの editor も即時剥奪する（残留アクセス防止・(a)）。
        let ws_revoked = if matches!(role, ThreadRole::Editor) {
            self.set_workspace_editor(ctx, thread_id, &subject, false)
                .await?
        } else {
            false
        };
        if let Err(e) = self
            .record_share_audit(ctx, thread_id, "thread.unshare", target, role, trace_id)
            .await
        {
            if revoked {
                let _ = self
                    .authz
                    .write_tuple(&subject, role.relation(), &obj)
                    .await;
            }
            if ws_revoked {
                let _ = self
                    .set_workspace_editor(ctx, thread_id, &subject, true)
                    .await;
            }
            return Err(e);
        }
        Ok(())
    }

    /// thread の全 owner/editor に自律ワークスペースフォルダの editor を付与する（backfill・(a)）。
    ///
    /// ワークスペース作成**前**に共有された相手や、フォルダを所有しない thread owner
    /// （＝別の editor が先に自律 run を回してフォルダを作った場合）にもアクセスを行き渡らせる。
    /// 自律 run 開始時（`ensure_workspace`）に冪等に呼ぶ。viewer/commenter は post 不可＝自律 run を
    /// 起こせないので対象外。
    pub async fn grant_workspace_to_members(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
    ) -> Result<(), ChatError> {
        let Some(folder_id) = self.workspace_folder_id(thread_id, &ctx.tenant_id).await? else {
            return Ok(());
        };
        let folder = ctx.ns().folder(&folder_id.to_string());
        let thread_obj = ctx.ns().thread(&thread_id.to_string());
        let tuples = self
            .authz
            .read_tuples(&thread_obj, None)
            .await
            .map_err(|e| ChatError::Internal(e.to_string()))?;
        for t in tuples {
            let Some(rel) = Relation::parse(&t.relation) else {
                continue;
            };
            if !matches!(rel, Relation::Owner | Relation::Editor) {
                continue;
            }
            let Some(target) = ShareTarget::parse_subject(&ctx.ns(), &t.user) else {
                continue;
            };
            self.authz
                .write_tuple(&target.subject(&ctx.ns()), Relation::Editor, &folder)
                .await
                .map_err(|e| ChatError::Internal(e.to_string()))?;
        }
        Ok(())
    }

    /// 共有相手一覧（owner 権限・直接タプルのみ）。
    pub async fn list_thread_shares(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Vec<(ShareTarget, ThreadRole)>, ChatError> {
        let obj = self
            .require_thread(
                ctx,
                thread_id,
                Relation::Owner,
                "thread.shares.list",
                trace_id,
            )
            .await?;
        let tuples = self
            .authz
            .read_tuples(&obj, None)
            .await
            .map_err(|e| ChatError::Internal(e.to_string()))?;
        let mut out = Vec::new();
        for t in tuples {
            let Some(role) = Relation::parse(&t.relation).and_then(ThreadRole::from_relation)
            else {
                continue;
            };
            let Some(target) = ShareTarget::parse_subject(&ctx.ns(), &t.user) else {
                continue;
            };
            out.push((target, role));
        }
        Ok(out)
    }

    async fn record_share_audit(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        action: &str,
        target: &ShareTarget,
        role: ThreadRole,
        trace_id: Option<&str>,
    ) -> Result<(), ChatError> {
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action,
                    object_type: "thread",
                    object_id: &thread_id.to_string(),
                    decision: Decision::Allow,
                    trace_id,
                    metadata: json!({ "target": target, "role": role }),
                },
            )
            .await
            .map_err(map_storage)
    }
}

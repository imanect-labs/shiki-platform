//! `ChatStore`: スレッド共有（ReBAC タプル付与/剥奪・一覧）と自律ワークスペースの権限伝播（Task 3.7 / 5.6(a)）。
//! 会話に話した人のほかの相手がいるかの判定（[`ChatStore::thread_audience`]・PIT-84・D-214）もここに置く。
//!
//! `threads.rs`（thread CRUD）から共有系を分離。struct/フィールド/自由関数は `use super::*` で総取りする。

#[allow(clippy::wildcard_imports)]
use super::*;
use storage::audit::AuditAction;

use authz::{AuthContext, AuthzError, ReadTupleKey, Relation};
use serde_json::json;
use storage::audit::{AuditEntry, Decision};
use storage::model::ShareTarget;
use uuid::Uuid;

use super::threads::map_storage;
use crate::model::ThreadRole;

/// 会話を、話した人のほかに読める人がいるか（[`ChatStore::thread_audience`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ThreadAudience {
    /// スレッドの直接のタプルが 1 本以上あって、どれも話した人の owner・editor・commenter・viewer。
    SpeakerOnly,
    /// ほかの人かロールのタプルがある・タプルが無い・解釈できないタプルがある・読めなかった（fail-closed）。
    Shared,
}

impl ThreadAudience {
    /// 相手がいる（か、いないと確かめられなかった）。
    pub(crate) fn is_shared(self) -> bool {
        self == ThreadAudience::Shared
    }
}

/// スレッドの直接のタプルから相手の有無を決める（`me` は話した人の subject・`user:<tenant>|<id>`）。
///
/// [`ThreadAudience::SpeakerOnly`] は、タプルが 1 本以上あって、どれも主体が `me` で relation が thread の 4 つ
/// （owner・editor・commenter・viewer）のどれかのときだけ。ほかは全部 [`ThreadAudience::Shared`] にする。
/// - 空: 本物の会話には必ず owner のタプルがある（create_thread は書けなければ行を消して補償する）ので、異常として相手が
///   いる側に倒す。
/// - 持ち主でない話し手から見た owner も相手として数える。
/// - `role#member` は、話した人だけのロールでも数える（所属はスレッドの外で増減する）。
/// - 解釈できない主体と、知らない relation も数える。
pub(crate) fn audience_of(tuples: &[ReadTupleKey], me: &str) -> ThreadAudience {
    let only_me = !tuples.is_empty()
        && tuples.iter().all(|t| {
            t.user == me
                && matches!(
                    Relation::parse(&t.relation),
                    Some(
                        Relation::Owner | Relation::Editor | Relation::Commenter | Relation::Viewer
                    )
                )
        });
    if only_me {
        ThreadAudience::SpeakerOnly
    } else {
        ThreadAudience::Shared
    }
}

/// 読み取りの結果から相手の有無を決める（読めなければ [`ThreadAudience::Shared`]・fail-closed）。
pub(crate) fn audience_or_shared(
    read: Result<Vec<ReadTupleKey>, AuthzError>,
    me: &str,
) -> ThreadAudience {
    match read {
        Ok(tuples) => audience_of(&tuples, me),
        Err(_) => ThreadAudience::Shared,
    }
}

impl ChatStore {
    /// 会話（スレッド）に、話した人（`ctx` の主体）のほかに読める人がいるか（PIT-84・D-214）。
    ///
    /// スレッドに直接書いたタプルを全部読み、[`audience_of`] で決める。thread 型は owner・editor・commenter・viewer の
    /// 直接の relation だけを持ち、主体は user か `role#member` で、親も組織も持たない（共有リンクの付与も無い）ので、
    /// 直接のタプルで相手を数え尽くせる。relation は問わずに数える（閲覧だけの相手も相手）。
    ///
    /// - 読めなければ warn を残して [`ThreadAudience::Shared`] を返す。fail-closed を口の中に閉じ、呼び手が誤りを
    ///   「相手がいない」側に倒せないようにする。
    /// - 監査は書かない（前例: [`Self::grant_workspace_to_members`] も owner かを確かめずに読み、監査を残さない）。
    /// - [`Self::list_thread_shares`] は使わない。Owner を要求するので持ち主でない editor が話すと deny の監査を残して
    ///   断られ、呼ぶたびに一覧の監査を書くため。
    pub(crate) async fn thread_audience(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
    ) -> ThreadAudience {
        let read = self
            .authz
            .read_tuples(&ctx.ns().thread(&thread_id.to_string()), None)
            .await;
        if let Err(e) = &read {
            tracing::warn!(%thread_id, error = %e, "会話の共有の相手を読めないので、相手がいるものとして扱う");
        }
        audience_or_shared(read, ctx.subject().as_str())
    }
}

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
            .require_thread(
                ctx,
                thread_id,
                Relation::Owner,
                AuditAction::ThreadShare,
                trace_id,
            )
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
            .record_share_audit(
                ctx,
                thread_id,
                AuditAction::ThreadShare,
                target,
                role,
                trace_id,
            )
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
        let Some(folder_id) = self.workspace_folder_id(thread_id, ctx.tenant_id()).await? else {
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
            .require_thread(
                ctx,
                thread_id,
                Relation::Owner,
                AuditAction::ThreadUnshare,
                trace_id,
            )
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
            .record_share_audit(
                ctx,
                thread_id,
                AuditAction::ThreadUnshare,
                target,
                role,
                trace_id,
            )
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
        let Some(folder_id) = self.workspace_folder_id(thread_id, ctx.tenant_id()).await? else {
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
                AuditAction::ThreadSharesList,
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

    pub(super) async fn record_share_audit(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        action: AuditAction,
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

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "user:t1|alice";

    fn tuple(user: &str, relation: &str) -> ReadTupleKey {
        ReadTupleKey {
            user: user.into(),
            relation: relation.into(),
            object: "thread:t1|00000000-0000-0000-0000-000000000001".into(),
        }
    }

    /// 話した人だけのタプルなら相手はいない（relation を問わない）。
    #[test]
    fn only_the_speaker_is_speaker_only() {
        assert_eq!(
            audience_of(&[tuple(ME, "owner")], ME),
            ThreadAudience::SpeakerOnly
        );
        assert_eq!(
            audience_of(&[tuple(ME, "owner"), tuple(ME, "editor")], ME),
            ThreadAudience::SpeakerOnly
        );
        assert!(!ThreadAudience::SpeakerOnly.is_shared());
    }

    /// 空・ほかの人・ロール・知らない relation・解釈できない主体は、どれも相手がいる側に倒す。
    #[test]
    fn anything_else_is_shared() {
        let cases: Vec<Vec<ReadTupleKey>> = vec![
            vec![],
            vec![tuple(ME, "owner"), tuple("user:t1|dave", "viewer")],
            // 話した人が持ち主でないとき（editor の bob が話す）の owner も相手。
            vec![tuple("user:t1|bob", "owner"), tuple(ME, "editor")],
            // 話した人だけのロールでも数える。
            vec![
                tuple(ME, "owner"),
                tuple("role:t1|solo#member", "commenter"),
            ],
            vec![tuple(ME, "owner"), tuple(ME, "can_share")],
            vec![tuple(ME, "owner"), tuple("user:*", "viewer")],
            vec![tuple(ME, "owner"), tuple("alice", "viewer")],
            // 別のテナントの同じ id は別の人。
            vec![tuple("user:t2|alice", "owner")],
        ];
        for tuples in cases {
            assert_eq!(
                audience_of(&tuples, ME),
                ThreadAudience::Shared,
                "{tuples:?}"
            );
        }
        assert!(ThreadAudience::Shared.is_shared());
    }

    /// 読めなければ相手がいる側に倒す（fail-closed）。読めれば中身で決める。
    #[test]
    fn unreadable_is_shared() {
        assert_eq!(
            audience_or_shared(Err(AuthzError::InvalidModel("down".into())), ME),
            ThreadAudience::Shared
        );
        assert_eq!(
            audience_or_shared(Ok(vec![tuple(ME, "owner")]), ME),
            ThreadAudience::SpeakerOnly
        );
        assert_eq!(audience_or_shared(Ok(vec![]), ME), ThreadAudience::Shared);
    }
}

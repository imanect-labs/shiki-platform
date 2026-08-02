//! 共有スレッド閲覧時の**メッセージ再評価**（`ChatStore` の一部・#37/#386）。
//!
//! 会話は共有できるが、その中の引用・ツール結果は**閲覧者自身の権限**で見直す必要がある。
//! 「他人の引用をそのまま見せない」「認可の再評価ができない本文は出さない」の 2 つを、
//! 線形取得（[`super::threads`] の `get_messages`）の後段でまとめて掛ける。

#[allow(clippy::wildcard_imports)]
use super::*;

use authz::{AuthContext, Consistency, Relation};
use uuid::Uuid;

use crate::model::{ContentBlock, Message};

use super::threads::map_db;

impl ChatStore {
    /// 各メッセージの citation ブロックを閲覧者の viewer 権限で再評価し、読めない引用を落とす。
    pub(super) async fn filter_citations_for_viewer(
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

    /// ツール結果の本文を、実行主体本人以外には落とす（成否は残す）。
    ///
    /// tool_result は node に紐づかないため citation のような個別再認可ができない。
    /// 一方で本文には読取権限に依存する内容（社内文書のスニペット・ファイル本文・SQL 結果・
    /// コマンド出力）が入り得る。**認可の再評価ができない本文は出さない**方に倒す。
    /// 本人は自分の実行結果を全て見られるので、通常の会話では何も変わらない。
    pub(super) async fn redact_tool_results_for_viewer(
        &self,
        ctx: &AuthContext,
        messages: &mut [Message],
    ) -> Result<(), ChatError> {
        let ids: Vec<Uuid> = messages
            .iter()
            .filter(|m| {
                m.content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
            })
            .map(|m| m.id)
            .collect();
        if ids.is_empty() {
            return Ok(());
        }
        // そのメッセージを生成した run の actor（generation_run.message_id は assistant 側）。
        let rows: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT message_id, actor FROM generation_run \
             WHERE message_id = ANY($1) AND tenant_id = $2",
        )
        .bind(&ids)
        .bind(&ctx.tenant_id)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        let actors: std::collections::HashMap<Uuid, String> = rows.into_iter().collect();
        for m in messages.iter_mut() {
            // actor が引けないメッセージ（run 行が消えた等）は保守的に落とす。
            if actors.get(&m.id).is_some_and(|a| *a == ctx.principal.id) {
                continue;
            }
            for b in &mut m.content {
                if let ContentBlock::ToolResult { content, .. } = b {
                    content.clear();
                }
            }
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
}

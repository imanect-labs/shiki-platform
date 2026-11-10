//! 会話の分岐（`ChatStore` の一部・#1166）。
//!
//! メッセージは `parent_id` で木をなす（user 発話の親＝送ったときの枝の葉、assistant の親＝
//! 応答した user 発話）。過去の発話を編集すると同じ親を持つ兄弟ができ、それが枝になる。
//!
//! - 表示は**葉から根への経路**だけ（従来の線形取得と同じ形）。葉は `thread.active_message_id`、
//!   無ければ（指定なし・指す先が消えた）最も新しいメッセージ。
//! - 枝の切り替えは、選んだメッセージから**最も新しい子**を辿った葉を表示中にする。
//! - 生成ワーカーの履歴は「生成対象の祖先」だけ（他の枝の発話を混ぜない）。
//! - 「新しいチャットに分岐」は、その回答までの経路を**呼び出し者が見えるとおり**
//!   （引用の再評価・ツール結果の伏せ字の後）に新しいスレッドへ複製する。

#[allow(clippy::wildcard_imports)]
use super::*;

use std::collections::HashMap;

use authz::{AuthContext, Relation};
use chrono::{DateTime, Utc};
use serde_json::json;
use storage::audit::{AuditAction, AuditEntry, Decision};
use uuid::Uuid;

use super::threads::{map_db, map_storage};
use crate::model::{Message, Role, Thread};

/// 木の 1 ノード（本文は持たない）。
#[derive(Debug, Clone)]
pub(super) struct Node {
    pub id: Uuid,
    pub parent_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// スレッドのメッセージの木（id・親・作成時刻だけ）。
#[derive(Debug, Default)]
pub(super) struct Tree {
    nodes: HashMap<Uuid, Node>,
    /// 親 → 子（古い順）。根は `None` の下。
    children: HashMap<Option<Uuid>, Vec<Uuid>>,
    /// 最も新しいメッセージ（従来の線形取得の末尾と同じ並び）。
    latest: Option<Uuid>,
}

impl Tree {
    pub(super) fn new(rows: Vec<Node>) -> Self {
        let mut tree = Tree::default();
        // 並びは作成時刻 → 親を持たない方 → id。発話と応答の枠は同じトランザクションで作られ
        // 時刻が同値で、根以外はどちらも親を持つので、その組の前後は id（ランダム）で決まる。
        // そのため `latest` が発話を指すことがあるが、表示中の葉は `descend_latest` が子を辿って
        // 決めるので、葉としては応答に行き着く。兄弟の並び（版の番号）は時刻で決まる
        // （同じ親の下の発話は別々のトランザクションで作られる）。
        let key = |n: &Node| (n.created_at, n.parent_id.is_some(), n.id);
        let mut rows = rows;
        rows.sort_by_key(key);
        tree.latest = rows.last().map(|n| n.id);
        for n in &rows {
            tree.children.entry(n.parent_id).or_default().push(n.id);
        }
        // 親が木に無い（消えた）ノードは根として扱う（表示から落とさない）。
        let ids: std::collections::HashSet<Uuid> = rows.iter().map(|n| n.id).collect();
        let orphans: Vec<(Option<Uuid>, Uuid)> = rows
            .iter()
            .filter(|n| n.parent_id.is_some_and(|p| !ids.contains(&p)))
            .map(|n| (n.parent_id, n.id))
            .collect();
        for (parent, id) in orphans {
            if let Some(list) = tree.children.get_mut(&parent) {
                list.retain(|c| *c != id);
            }
            tree.children.entry(None).or_default().push(id);
        }
        if let Some(roots) = tree.children.get_mut(&None) {
            let pos: HashMap<Uuid, usize> =
                rows.iter().enumerate().map(|(i, n)| (n.id, i)).collect();
            roots.sort_by_key(|id| pos.get(id).copied());
        }
        tree.nodes = rows.into_iter().map(|n| (n.id, n)).collect();
        tree
    }

    pub(super) fn contains(&self, id: Uuid) -> bool {
        self.nodes.contains_key(&id)
    }

    /// 親（木に無い親は根扱いで `None`）。
    fn parent(&self, id: Uuid) -> Option<Uuid> {
        self.nodes
            .get(&id)
            .and_then(|n| n.parent_id)
            .filter(|p| self.nodes.contains_key(p))
    }

    /// 同じ親を持つメッセージ（古い順・自分を含む）。
    pub(super) fn siblings(&self, id: Uuid) -> &[Uuid] {
        if !self.contains(id) {
            return &[];
        }
        self.children
            .get(&self.parent(id))
            .map_or(&[], Vec::as_slice)
    }

    /// `from` から最も新しい子を辿った葉。
    pub(super) fn descend_latest(&self, from: Uuid) -> Uuid {
        let mut cur = from;
        // 循環は作らないが、壊れたデータでも止まるよう件数で打ち切る。
        for _ in 0..self.nodes.len() {
            match self.children.get(&Some(cur)).and_then(|c| c.last()) {
                Some(next) => cur = *next,
                None => break,
            }
        }
        cur
    }

    /// 表示中の葉（指定が木に無ければ最も新しいメッセージ）。メッセージが無ければ `None`。
    pub(super) fn active_leaf(&self, active: Option<Uuid>) -> Option<Uuid> {
        let start = active.filter(|id| self.contains(*id)).or(self.latest)?;
        Some(self.descend_latest(start))
    }

    /// 根から `leaf` までの経路（根が先頭）。`leaf` が木に無ければ空。
    pub(super) fn path_to(&self, leaf: Uuid) -> Vec<Uuid> {
        if !self.contains(leaf) {
            return Vec::new();
        }
        let mut path = vec![leaf];
        let mut cur = leaf;
        while let Some(p) = self.parent(cur) {
            if path.len() > self.nodes.len() {
                break; // 壊れたデータ（循環）への備え
            }
            path.push(p);
            cur = p;
        }
        path.reverse();
        path
    }
}

/// スレッドの木と表示中の葉の指定を 1 往復で引く（認可は呼び出し側が済ませる）。
///
/// スレッドが無ければ空の木と `None`。メッセージが無いスレッドも 1 行（メッセージ側が NULL）返る。
pub(super) async fn load_tree(
    conn: &mut sqlx::PgConnection,
    thread_id: Uuid,
    tenant_id: &str,
) -> Result<(Tree, Option<Uuid>), ChatError> {
    type Row = (
        Option<Uuid>,
        Option<Uuid>,
        Option<DateTime<Utc>>,
        Option<Uuid>,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT m.id, m.parent_id, m.created_at, t.active_message_id FROM thread t \
         LEFT JOIN message m ON m.thread_id = t.id AND m.tenant_id = t.tenant_id \
         WHERE t.id = $1 AND t.tenant_id = $2",
    )
    .bind(thread_id)
    .bind(tenant_id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db)?;
    let active = rows.first().and_then(|r| r.3);
    let nodes = rows
        .into_iter()
        .filter_map(|(id, parent_id, created_at, _)| {
            Some(Node {
                id: id?,
                parent_id,
                created_at: created_at?,
            })
        })
        .collect();
    Ok((Tree::new(nodes), active))
}

/// 未完了（順番待ち・生成中・承認待ち）の run があるか。
///
/// ある間は表示中の枝を動かさない（枝の切り替え・発話の編集を受けない）。生成先と画面の枝が
/// ずれると、生成中の回答や順番待ちが見えていない枝の下に描かれる。
pub(super) async fn has_unfinished_run(
    conn: &mut sqlx::PgConnection,
    thread_id: Uuid,
    tenant_id: &str,
) -> Result<bool, ChatError> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM generation_run \
         WHERE thread_id = $1 AND tenant_id = $2 \
           AND status IN ('queued', 'running', 'waiting_approval'))",
    )
    .bind(thread_id)
    .bind(tenant_id)
    .fetch_one(conn)
    .await
    .map_err(map_db)
}

/// 生成中は枝を動かせないことを伝えるエラー（400・画面へそのまま出す）。
pub(super) fn busy_error() -> ChatError {
    ChatError::Invalid("回答の生成中は、発話の編集や版の切り替えはできません".into())
}

/// 表示中の枝の葉を書く。
pub(super) async fn set_active_leaf(
    conn: &mut sqlx::PgConnection,
    thread_id: Uuid,
    tenant_id: &str,
    leaf: Uuid,
) -> Result<(), ChatError> {
    sqlx::query("UPDATE thread SET active_message_id = $3 WHERE id = $1 AND tenant_id = $2")
        .bind(thread_id)
        .bind(tenant_id)
        .bind(leaf)
        .execute(conn)
        .await
        .map_err(map_db)?;
    Ok(())
}

/// 経路上のメッセージに、兄弟（枝の切り替え先）を載せる。兄弟が自分だけなら空のまま。
pub(super) fn attach_siblings(tree: &Tree, messages: &mut [Message]) {
    for m in messages {
        let siblings = tree.siblings(m.id);
        if siblings.len() > 1 {
            m.sibling_ids = siblings.to_vec();
        }
    }
}

/// 分岐した会話の題名（元の題名に印を付ける・長すぎれば切り詰める）。
pub(super) fn fork_title(source: &str) -> String {
    const SUFFIX: &str = "（分岐）";
    let max = super::thread_manage::MAX_THREAD_TITLE_CHARS - SUFFIX.chars().count();
    let base: String = source.trim().chars().take(max).collect();
    format!("{base}{SUFFIX}")
}

impl ChatStore {
    /// 表示する枝を切り替える（editor・選んだメッセージから最も新しい子を辿った葉を表示中にする）。
    ///
    /// 表示中の枝はスレッドの状態（共有先にも同じ枝が見える）なので、投稿できる人だけが変えられる。
    pub async fn select_branch(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        message_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(), ChatError> {
        let action = AuditAction::ThreadSelectBranch;
        self.require_thread(ctx, thread_id, Relation::Editor, action, trace_id)
            .await?;
        let mut tx = self.db.begin().await.map_err(map_db)?;
        // 投稿と直列化する（投稿は同じ行を先に更新してから親を決める）。
        let locked: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM thread WHERE id = $1 AND tenant_id = $2 AND deleted_at IS NULL \
             FOR UPDATE",
        )
        .bind(thread_id)
        .bind(ctx.tenant_id())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db)?;
        if locked.is_none() {
            return Err(ChatError::NotFound);
        }
        if has_unfinished_run(&mut tx, thread_id, ctx.tenant_id()).await? {
            return Err(busy_error());
        }
        let (tree, _) = load_tree(&mut tx, thread_id, ctx.tenant_id()).await?;
        if !tree.contains(message_id) {
            return Err(ChatError::NotFound);
        }
        let leaf = tree.descend_latest(message_id);
        set_active_leaf(&mut tx, thread_id, ctx.tenant_id(), leaf).await?;
        tx.commit().await.map_err(map_db)?;
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action,
                    object_type: "thread",
                    object_id: &thread_id.to_string(),
                    decision: Decision::Allow,
                    trace_id,
                    metadata: json!({ "message_id": message_id.to_string() }),
                },
            )
            .await
            .map_err(map_storage)
    }

    /// 編集元の発話に応答した run の生成材料（自律か・skill ピン・ツール指定）を引く。
    ///
    /// 発話の編集は「その発話の送り直し」なので、スラッシュコマンドで送った発話を直しても
    /// 手順書（run 単位の skill）と自律の指定を落とさない。認可（editor）は
    /// [`Self::message_run_context`] が先に取る。応答がまだ無ければ `None`。
    pub async fn edited_turn_context(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        user_message_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Option<super::RunContext>, ChatError> {
        self.require_thread(
            ctx,
            thread_id,
            Relation::Editor,
            AuditAction::ThreadRunGet,
            trace_id,
        )
        .await?;
        let reply: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM message \
             WHERE thread_id = $1 AND tenant_id = $2 AND parent_id = $3 AND role = 'assistant' \
             ORDER BY created_at, id LIMIT 1",
        )
        .bind(thread_id)
        .bind(ctx.tenant_id())
        .bind(user_message_id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        match reply {
            Some(reply) => {
                self.message_run_context(ctx, thread_id, reply, trace_id)
                    .await
            }
            None => Ok(None),
        }
    }

    /// その回答までの会話を新しいスレッドへ複製する（「新しいチャットに分岐」）。
    ///
    /// 元のスレッドの **editor** を要求する（閲覧だけの共有先が内容を自分の会話として持ち出し、
    /// 元の owner が共有していない相手へ再共有できないようにする）。複製するのは**呼び出し者が
    /// 見えるとおりの内容**（読めない引用は番号だけ・他人のツール結果は伏せ字）で、新しい
    /// スレッドの owner は呼び出し者。分岐できるのは確定した（本文のある）回答からだけ。
    /// 途中の生成待ちの空の応答は複製しない。図のボタンの束縛・実行の記録・評価は複製しない
    /// （新しいメッセージでは押せない＝元の会話の操作を持ち越さない）。
    pub async fn fork_thread(
        &self,
        ctx: &AuthContext,
        source_thread_id: Uuid,
        message_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Thread, ChatError> {
        let action = AuditAction::ThreadFork;
        self.require_thread(ctx, source_thread_id, Relation::Editor, action, trace_id)
            .await?;
        let (source, path) = tokio::try_join!(
            self.get_thread(ctx, source_thread_id, trace_id),
            self.get_message_path(ctx, source_thread_id, message_id, trace_id),
        )?;
        let forkable = path
            .last()
            .is_some_and(|m| m.role == Role::Assistant && !m.content.is_empty());
        if !forkable {
            return Err(ChatError::NotFound);
        }
        let thread = self
            .create_thread(
                ctx,
                &fork_title(&source.title),
                source.agent_mode,
                None,
                trace_id,
            )
            .await?;
        if let Err(e) = self.copy_path(ctx, thread.id, &path).await {
            self.discard_forked(ctx, thread.id).await;
            return Err(e);
        }
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action,
                    object_type: "thread",
                    object_id: &thread.id.to_string(),
                    decision: Decision::Allow,
                    trace_id,
                    metadata: json!({
                        "source_thread_id": source_thread_id.to_string(),
                        "message_id": message_id.to_string(),
                        "messages": path.len(),
                    }),
                },
            )
            .await
            .map_err(map_storage)?;
        Ok(thread)
    }

    /// 複製に失敗した会話を消す（行と owner タプル）。失敗は記録だけして元のエラーを返す
    /// （行が無ければ残ったタプルからは何も見えない＝以後の操作は NotFound）。
    async fn discard_forked(&self, ctx: &AuthContext, thread_id: Uuid) {
        if let Err(e) = sqlx::query("DELETE FROM thread WHERE id = $1 AND tenant_id = $2")
            .bind(thread_id)
            .bind(ctx.tenant_id())
            .execute(&self.db)
            .await
        {
            tracing::warn!(%thread_id, error = %e, "分岐に失敗した会話を消せなかった");
        }
        let obj = ctx.ns().thread(&thread_id.to_string());
        if let Err(e) = self.authz.delete_object_tuples(&obj).await {
            tracing::warn!(%thread_id, error = %e, "分岐に失敗した会話のタプルを消せなかった");
        }
    }

    /// 経路を新しいスレッドへ 1 文で書く（親は直前のもの・作成時刻は元のまま）。
    async fn copy_path(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        path: &[Message],
    ) -> Result<(), ChatError> {
        let kept: Vec<&Message> = path
            .iter()
            .filter(|m| !(m.role == Role::Assistant && m.content.is_empty()))
            .collect();
        let ids: Vec<Uuid> = kept.iter().map(|_| Uuid::new_v4()).collect();
        let parents: Vec<Option<Uuid>> = std::iter::once(None)
            .chain(ids.iter().copied().map(Some))
            .take(ids.len())
            .collect();
        let roles: Vec<&str> = kept.iter().map(|m| m.role.as_str()).collect();
        let contents: Vec<serde_json::Value> = kept
            .iter()
            .map(|m| serde_json::to_value(&m.content))
            .collect::<Result<_, _>>()
            .map_err(|e| ChatError::Internal(format!("content: {e}")))?;
        let agent_modes: Vec<bool> = kept.iter().map(|m| m.agent_mode).collect();
        let created: Vec<DateTime<Utc>> = kept.iter().map(|m| m.created_at).collect();
        let mut tx = self.db.begin().await.map_err(map_db)?;
        sqlx::query(
            "INSERT INTO message (id, thread_id, org, tenant_id, role, content, agent_mode, parent_id, created_at) \
             SELECT u.id, $1, $2, $3, u.role, u.content, u.agent_mode, u.parent_id, u.created_at \
             FROM UNNEST($4::uuid[], $5::text[], $6::jsonb[], $7::bool[], $8::uuid[], $9::timestamptz[]) \
                  AS u(id, role, content, agent_mode, parent_id, created_at)",
        )
        .bind(thread_id)
        .bind(ctx.org())
        .bind(ctx.tenant_id())
        .bind(&ids)
        .bind(&roles)
        .bind(&contents)
        .bind(&agent_modes)
        .bind(&parents)
        .bind(&created)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        // 題名は複製元のものを使う（最初の生成で要約し直さない）。
        sqlx::query(
            "UPDATE thread SET active_message_id = $3, title_pending = false \
             WHERE id = $1 AND tenant_id = $2",
        )
        .bind(thread_id)
        .bind(ctx.tenant_id())
        .bind(ids.last())
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        tx.commit().await.map_err(map_db)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(s: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + s, 0).unwrap()
    }

    fn node(id: u128, parent: Option<u128>, t: i64) -> Node {
        Node {
            id: Uuid::from_u128(id),
            parent_id: parent.map(Uuid::from_u128),
            created_at: at(t),
        }
    }

    fn u(id: u128) -> Uuid {
        Uuid::from_u128(id)
    }

    /// u1 → a2 → u3 → a4、u3 を編集した u5 → a6（a2 の下に u3・u5 の 2 枝）。
    fn edited() -> Tree {
        Tree::new(vec![
            node(1, None, 0),
            node(2, Some(1), 0),
            node(3, Some(2), 1),
            node(4, Some(3), 1),
            node(5, Some(2), 2),
            node(6, Some(5), 2),
        ])
    }

    #[test]
    fn active_leaf_defaults_to_latest_and_follows_selection() {
        let t = edited();
        assert_eq!(t.active_leaf(None), Some(u(6)));
        assert_eq!(t.active_leaf(Some(u(4))), Some(u(4)));
        // 指す先が木に無ければ最も新しいもの。
        assert_eq!(t.active_leaf(Some(u(99))), Some(u(6)));
        // 途中を指していれば最も新しい子を辿る。
        assert_eq!(t.active_leaf(Some(u(2))), Some(u(6)));
        assert_eq!(Tree::new(Vec::new()).active_leaf(None), None);
    }

    #[test]
    fn path_and_siblings() {
        let t = edited();
        assert_eq!(t.path_to(u(4)), vec![u(1), u(2), u(3), u(4)]);
        assert_eq!(t.path_to(u(6)), vec![u(1), u(2), u(5), u(6)]);
        assert!(t.path_to(u(99)).is_empty());
        assert_eq!(t.siblings(u(3)), &[u(3), u(5)]);
        assert_eq!(t.siblings(u(1)), &[u(1)]);
        assert!(t.siblings(u(99)).is_empty());
        assert_eq!(t.descend_latest(u(3)), u(4));
    }

    #[test]
    fn same_time_reply_sorts_after_its_message() {
        // 発話と応答は同時刻。応答（親あり）が最新になる。
        let t = Tree::new(vec![node(9, Some(1), 0), node(1, None, 0)]);
        assert_eq!(t.active_leaf(None), Some(u(9)));
    }

    #[test]
    fn orphan_is_treated_as_root() {
        // 親が消えたノードは根として残る（表示から落とさない）。
        let t = Tree::new(vec![node(1, None, 0), node(3, Some(42), 1)]);
        assert_eq!(t.siblings(u(3)), &[u(1), u(3)]);
        assert_eq!(t.path_to(u(3)), vec![u(3)]);
    }

    #[test]
    fn fork_title_marks_and_clamps() {
        assert_eq!(fork_title("予算の相談"), "予算の相談（分岐）");
        let long = "あ".repeat(500);
        let t = fork_title(&long);
        assert_eq!(
            t.chars().count(),
            super::super::thread_manage::MAX_THREAD_TITLE_CHARS
        );
        assert!(t.ends_with("（分岐）"));
    }
}

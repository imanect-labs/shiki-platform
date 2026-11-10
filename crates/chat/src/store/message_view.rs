//! 共有スレッド閲覧時の**メッセージ再評価**（`ChatStore` の一部・#37/#386）。
//!
//! 会話は共有できるが、その中の引用・ツール結果は**閲覧者自身の権限**で見直す必要がある。
//! 「他人の引用をそのまま見せない」「認可の再評価ができない本文は出さない」の 2 つを、
//! 線形取得（[`super::threads`] の `get_messages`）の後段でまとめて掛ける。
//! 生成中の SSE 再送（[`ChatStore::viewer_event_stream`]）にも同じ 2 つを掛ける（#513）。

#[allow(clippy::wildcard_imports)]
use super::*;
use storage::audit::AuditAction;

use std::collections::{HashMap, HashSet, VecDeque};

use authz::{AuthContext, CheckTuple, Consistency, Relation};
use futures::stream::{BoxStream, StreamExt};
use uuid::Uuid;

use crate::model::{Citation, ContentBlock, Message, StreamEvent, StreamEventKind};

use super::threads::map_db;

/// 1 回にまとめて見直すイベントの上限（再訪で数千件届いても、最初の表示を待たせすぎない）。
const MAX_REDACT_BATCH: usize = 1000;

impl ChatStore {
    /// 各メッセージの citation ブロックを閲覧者の viewer 権限で再評価し、読めない引用を落とす。
    ///
    /// Web の出典（[`Citation::is_web`]）は公開 web のページなので見直さない（#891）。
    ///
    /// 番号（`cite_id`）を持つ引用は、落とす代わりに番号だけの「閲覧できない出典」に置き換える
    /// （[`Citation::withheld`]）。本文の `[n]` を欠番として描けるようにするため。ファイル・本文・
    /// 位置情報は一切残さない。番号を持たない旧データは従来どおり落とす。
    pub(super) async fn filter_citations_for_viewer(
        &self,
        ctx: &AuthContext,
        messages: &mut [Message],
    ) -> Result<(), ChatError> {
        // 引用対象ファイルの重複を除いて一括判定（同一ファイルの複数引用を一度に・1 往復にまとめる）。
        // Web の出典は公開情報（ファイルの権限で見直す対象ではない・#891）。
        let node_ids: Vec<String> = messages
            .iter()
            .flat_map(|m| m.content.iter())
            .filter_map(|b| match b {
                ContentBlock::Citation(c) if !c.is_web() => Some(c.node_id.clone()),
                _ => None,
            })
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let decisions = self.can_view_files(ctx, &node_ids).await;
        if decisions.values().all(|v| *v) {
            return Ok(()); // 全て閲覧可（所有者/十分な権限）なら何もしない
        }
        for m in messages.iter_mut() {
            let content = std::mem::take(&mut m.content);
            m.content = content
                .into_iter()
                .filter_map(|b| match b {
                    ContentBlock::Citation(c)
                        if !c.is_web() && !*decisions.get(&c.node_id).unwrap_or(&false) =>
                    {
                        (c.cite_id > 0)
                            .then(|| ContentBlock::Citation(Citation::withheld(c.cite_id)))
                    }
                    other => Some(other),
                })
                .collect();
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
        .bind(ctx.tenant_id())
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        // `generation_run.message_id` は UNIQUE ではないので、1 メッセージに複数の run が
        // 紐づき得る。actor が食い違う行が混ざったら**行順に依存して勝者が決まる**ので、
        // 一意に定まらない場合は「本人と確定できない」として落とす（fail-closed）。
        let mut actors: std::collections::HashMap<Uuid, Option<String>> =
            std::collections::HashMap::new();
        for (message_id, actor) in rows {
            match actors.entry(message_id) {
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(Some(actor));
                }
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    if e.get().as_deref() != Some(actor.as_str()) {
                        e.insert(None);
                    }
                }
            }
        }
        for m in messages.iter_mut() {
            // actor が引けないメッセージ（run 行が消えた・複数 actor で確定しない）は落とす。
            if actors
                .get(&m.id)
                .and_then(Option::as_deref)
                .is_some_and(|a| a == ctx.principal().id)
            {
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

    /// run のイベントを、**閲覧者の権限で見直してから**流すストリームを返す（#513）。
    ///
    /// 保存済みメッセージ（`get_messages`）と同じ 2 つを、生成中の再送にも掛ける。掛けないと、
    /// 共有スレッドを開いた閲覧者が、自分では読めない文書の抜粋を生成中にだけ見られてしまう。
    /// - 引用: 閲覧者が読めないファイルのものは番号だけの出典（[`Citation::withheld`]）に置き換える
    ///   （Web の出典は公開情報なのでそのまま）。
    ///   許可は引用のたびに確かめ直し、拒否だけをストリームの間覚えておく（剥奪を直後の引用から反映する）。
    /// - ツール結果: 閲覧者が run の実行主体でなければ本文を落とす（成否は残す）。
    ///
    /// スレッドの viewer 認可と「run がそのスレッドのものか」もここで確かめる（呼び出し側の
    /// 事前確認に頼らない。run_id だけで読めると同じテナントの他スレッドの run を覗ける）。
    pub async fn viewer_event_stream(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        run_id: Uuid,
        from_seq: i64,
        trace_id: Option<&str>,
    ) -> Result<BoxStream<'static, StreamEvent>, ChatError> {
        self.require_thread(
            ctx,
            thread_id,
            Relation::Viewer,
            AuditAction::ThreadStream,
            trace_id,
        )
        .await?;
        let actor: Option<String> = sqlx::query_scalar(
            "SELECT actor FROM generation_run \
             WHERE run_id = $1 AND thread_id = $2 AND tenant_id = $3",
        )
        .bind(run_id)
        .bind(thread_id)
        .bind(ctx.tenant_id())
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        let Some(actor) = actor else {
            return Err(ChatError::NotFound);
        };
        let is_actor = actor == ctx.principal().id;
        let state = (
            self.event_stream(ctx.tenant_id(), run_id, from_seq),
            self.clone(),
            ctx.clone(),
            HashSet::<String>::new(),
            VecDeque::<Option<StreamEvent>>::new(),
        );
        // 順序を保って見直す（引用の判定は非同期なので then ではなく unfold で状態を持ち回す）。
        //
        // **届いている分はまとめて見直す。** 再訪・再接続では run の最初からのイベントが一度に届く。
        // 引用を 1 件ずつ OpenFGA に問うと、その往復のたびに後続（ツール行・本文）が止まり、画面が
        // 最後の操作までリプレイのように少しずつ進む（#1152）。溜まっている分を一度に取り出し、含まれる
        // 引用の可否を 1 回の BatchCheck で判定してから順に流す。ライブで 1 件ずつ届く間は従来どおり
        // 都度確かめる（剥奪を直後の引用から反映する性質は変わらない）。
        let stream = futures::stream::unfold(
            state,
            move |(mut rx, store, ctx, mut denied, mut pending)| async move {
                if pending.is_empty() {
                    let mut batch = vec![rx.next().await?];
                    while batch.len() < MAX_REDACT_BATCH {
                        match rx.try_recv() {
                            Ok(ev) => batch.push(ev),
                            Err(_) => break, // 空（ライブの合間）か、送り手が閉じた
                        }
                    }
                    pending = store
                        .redact_events_for_viewer(&ctx, is_actor, &mut denied, batch)
                        .await
                        .into();
                }
                let ev = pending.pop_front()?;
                Some((ev, (rx, store, ctx, denied, pending)))
            },
        )
        .filter_map(futures::future::ready);
        Ok(stream.boxed())
    }

    /// 届いたイベントをまとめて閲覧者の権限で見直す（落とすものは `None`・順序は保つ）。
    ///
    /// **許可は使い回さない。** まとめて届いた分の引用は 1 回の BatchCheck（HigherConsistency）で
    /// 確かめ、次に届く分ではまた確かめ直す（長い run を開いたままの間に閲覧権限が外れたら、次に届く
    /// 引用から反映させる。保存済みの経路と同じく剥奪を優先する）。使い回すのは拒否だけ
    /// （拒否を覚えていても漏れる方向には倒れない）。
    async fn redact_events_for_viewer(
        &self,
        ctx: &AuthContext,
        is_actor: bool,
        denied: &mut HashSet<String>,
        events: Vec<StreamEvent>,
    ) -> Vec<Option<StreamEvent>> {
        // Web の出典は公開情報（保存済みの経路と同じくそのまま流す・#891）。拒否済みは問い直さない。
        let node_ids: Vec<String> = events
            .iter()
            .filter_map(|e| match &e.event {
                StreamEventKind::Citation(c) if !c.is_web() && !denied.contains(&c.node_id) => {
                    Some(c.node_id.clone())
                }
                _ => None,
            })
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let decisions = self.can_view_files(ctx, &node_ids).await;
        events
            .into_iter()
            .map(|mut ev| {
                match &mut ev.event {
                    StreamEventKind::Citation(c) if c.is_web() => {}
                    StreamEventKind::Citation(c) => {
                        let allowed = !denied.contains(&c.node_id)
                            && decisions.get(&c.node_id).copied().unwrap_or(false);
                        if !allowed {
                            denied.insert(c.node_id.clone());
                            // 番号を持たない旧形式は保存済みの経路と同じく落とす。
                            if c.cite_id == 0 {
                                return None;
                            }
                            *c = Citation::withheld(c.cite_id);
                        }
                    }
                    StreamEventKind::ToolResult { content, .. } if !is_actor => content.clear(),
                    _ => {}
                }
                Some(ev)
            })
            .collect()
    }

    /// 閲覧者が各ファイルを閲覧できるか（citation 再評価用）。BatchCheck で 1 往復にまとめる
    /// （OpenFGA 実装は 50 問ごとに 1 往復）。判定に失敗したら全件 false（fail-closed）。
    ///
    /// 引用は**文書の本文**（抜粋）を含むので、共有解除の反映が遅れると読めないはずの中身が
    /// 見えたままになる。thread の認可（`require_thread`）と同じ理由で HigherConsistency を使う
    /// （剥奪の即時反映を、キャッシュ由来の古い許可より優先する）。
    async fn can_view_files(
        &self,
        ctx: &AuthContext,
        node_ids: &[String],
    ) -> HashMap<String, bool> {
        if node_ids.is_empty() {
            return HashMap::new();
        }
        let subject = ctx.subject();
        let items: Vec<CheckTuple> = node_ids
            .iter()
            .map(|id| CheckTuple {
                subject: subject.clone(),
                relation: Relation::Viewer,
                object: ctx.ns().file(id),
            })
            .collect();
        let allowed = self
            .authz
            .batch_check(&items, Consistency::HigherConsistency)
            .await
            .unwrap_or_else(|_| vec![false; items.len()]);
        node_ids
            .iter()
            .cloned()
            .zip(allowed.into_iter().chain(std::iter::repeat(false)))
            .collect()
    }
}

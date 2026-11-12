//! 共有スレッド閲覧時の**メッセージ再評価**（`ChatStore` の一部・#37/#386）。
//!
//! 会話は共有できるが、その中の引用・ツール結果は**閲覧者自身の権限**で見直す必要がある。
//! 「他人の引用をそのまま見せない」「認可の再評価ができない本文は出さない」の 2 つを、
//! 線形取得（[`super::threads`] の `get_messages`）の後段でまとめて掛ける。
//! 生成中の SSE 再送（[`ChatStore::viewer_event_stream`]）にも同じ 2 つを掛ける（#513）。
//! 生成中の配信では、run の実行主体でない閲覧者の会話の閲覧権限も問い直し、共有を外された相手への
//! 配信を止める（PIT-84・D-214）。

#[allow(clippy::wildcard_imports)]
use super::*;
use storage::audit::AuditAction;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use authz::{AuthContext, CheckTuple, Consistency, Relation};
use futures::channel::mpsc;
use futures::stream::{BoxStream, StreamExt};
use uuid::Uuid;

use crate::model::{Citation, ContentBlock, Message, StreamEvent, StreamEventKind};

use super::threads::map_db;

/// 1 回にまとめて見直すイベントの上限（再訪で数千件届いても、最初の表示を待たせすぎない）。
const MAX_REDACT_BATCH: usize = 1000;
/// BatchCheck 1 往復の問数（OpenFGA の上限 `MAX_CHECKS_PER_BATCH` と揃える）。
const CHECK_CHUNK: usize = 50;
/// BatchCheck の往復を並行させる本数。
const CHECK_CONCURRENCY: usize = 8;
/// run の実行主体でない閲覧者の配信で、会話の閲覧権限を問い直す間隔（PIT-84・D-214）。
///
/// 開いた配信 1 本あたり、HigherConsistency の check と会話の生存の SELECT が毎秒 1 回まで増える。
/// 共有を外してから、この間隔の中に届いた分は流れうる（既知の限界）。
const VIEWER_RECHECK: Duration = Duration::from_secs(1);
/// 問い直しで断られた閲覧者に、配信の最後に 1 件だけ流す文（中身を含まない固定の文）。
///
/// 画面は `error` を受けると購読を閉じる（`web/src/lib/chat-api.ts` の `dispatch`）。読み込み直すと、
/// 会話の取得（`get_thread`）が断るので、もう開けない。
const VIEWER_REVOKED_MESSAGE: &str =
    "表示を続けられなくなりました。ページを読み込み直してください。";

/// [`ChatStore::viewer_event_stream`] の、イベントをまたいで持ち回す状態。
struct ViewerStreamState {
    rx: mpsc::UnboundedReceiver<StreamEvent>,
    store: ChatStore,
    ctx: AuthContext,
    /// 「読めない」と答えが出た引用のファイル（拒否だけを覚える）。
    denied: HashSet<String>,
    /// 見直し済みで、まだ流していない分（落とすものは `None`）。
    pending: VecDeque<Option<StreamEvent>>,
    /// 最後に会話の閲覧権限を確かめた時刻（開くときの認可の直後から数える）。
    checked_at: Instant,
    /// 打ち切りの文を積んだ（流し終えたら閉じる）。
    ended: bool,
    /// 問い直しの監査に残す trace_id（開いたときのもの）。
    trace: Option<String>,
}

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
        if decisions.values().all(|v| *v == Some(true)) {
            return Ok(()); // 全て閲覧可（所有者/十分な権限）なら何もしない
        }
        for m in messages.iter_mut() {
            let content = std::mem::take(&mut m.content);
            m.content = content
                .into_iter()
                .filter_map(|b| match b {
                    ContentBlock::Citation(c)
                        if !c.is_web()
                            && decisions.get(&c.node_id).copied().flatten() != Some(true) =>
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
    ///
    /// **実行主体でない閲覧者には、開いた後も会話の閲覧権限を問い直す**（PIT-84・D-214）。
    /// 開くときの認可だけだと、共有を外しても（`unshare_thread` はストリームを閉じない）run が終わるまで
    /// 本文・思考・ツールの行が流れ続ける。共有の取り消しを、引用だけでなくこれらにも効かせるため、
    /// 届いた分を流す前に、前の確認から [`VIEWER_RECHECK`] 以上たっていれば `require_thread`（Viewer・
    /// HigherConsistency・拒否は監査に残る）を問う。断られたか確かめられなければ、その分は流さず、
    /// 固定の文の `Error` を 1 件だけ流して閉じる（fail-closed）。間隔の中に届いた分は流れうる。
    /// 実行主体の配信は問い直さない（流れるのは本人の生成だけで、読み手を増やさないため）。
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
        let checked_at = Instant::now();
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
        let state = ViewerStreamState {
            rx: self.event_stream(ctx.tenant_id(), run_id, from_seq),
            store: self.clone(),
            ctx: ctx.clone(),
            denied: HashSet::new(),
            pending: VecDeque::new(),
            checked_at,
            ended: false,
            trace: trace_id.map(str::to_string),
        };
        // 順序を保って見直す（引用の判定は非同期なので then ではなく unfold で状態を持ち回す）。
        //
        // **届いている分はまとめて見直す。** 再訪・再接続では run の最初からのイベントが一度に届く。
        // 引用を 1 件ずつ OpenFGA に問うと、その往復のたびに後続（ツール行・本文）が止まり、画面が
        // 最後の操作までリプレイのように少しずつ進む（#1152）。溜まっている分を一度に取り出し、含まれる
        // 引用の可否を 1 回の BatchCheck で判定してから順に流す。ライブで 1 件ずつ届く間は従来どおり
        // 都度確かめる（剥奪を直後の引用から反映する性質は変わらない）。
        let stream = futures::stream::unfold(state, move |mut st| async move {
            if st.pending.is_empty() {
                // 打ち切りの文を流し終えた。次の分を待たずに閉じる（受け手を落として読み出しも止める）。
                if st.ended {
                    return None;
                }
                let mut batch = vec![st.rx.next().await?];
                while batch.len() < MAX_REDACT_BATCH {
                    match st.rx.try_recv() {
                        Ok(ev) => batch.push(ev),
                        Err(_) => break, // 空（ライブの合間）か、送り手が閉じた
                    }
                }
                if !is_actor && st.checked_at.elapsed() >= VIEWER_RECHECK {
                    match st
                        .store
                        .require_thread(
                            &st.ctx,
                            thread_id,
                            Relation::Viewer,
                            AuditAction::ThreadStream,
                            st.trace.as_deref(),
                        )
                        .await
                    {
                        Ok(_) => st.checked_at = Instant::now(),
                        Err(e) => {
                            tracing::info!(
                                error = %e,
                                thread_id = %thread_id,
                                run_id = %run_id,
                                "会話を見られなくなった閲覧者の配信を打ち切る"
                            );
                            // 届いた分（batch）は流さず、見直しにも回さない。seq は 0 にする
                            // （画面は 0 を「処理し終えた seq」に数えない・run の seq と重ならない）。
                            st.ended = true;
                            let cut = StreamEvent {
                                seq: 0,
                                event: StreamEventKind::Error {
                                    message: VIEWER_REVOKED_MESSAGE.to_string(),
                                },
                            };
                            return Some((Some(cut), st));
                        }
                    }
                }
                st.pending = st
                    .store
                    .redact_events_for_viewer(&st.ctx, is_actor, &mut st.denied, batch)
                    .await
                    .into();
            }
            let ev = st.pending.pop_front()?;
            Some((ev, st))
        })
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
                        let decision = if denied.contains(&c.node_id) {
                            Some(false)
                        } else {
                            decisions.get(&c.node_id).copied().flatten()
                        };
                        if decision != Some(true) {
                            // 「読めない」と答えが出たものだけ覚える。判定に失敗した分はこの回だけ伏せる。
                            if decision == Some(false) {
                                denied.insert(c.node_id.clone());
                            }
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

    /// 閲覧者が各ファイルを閲覧できるか（citation 再評価用）。BatchCheck にまとめ、
    /// [`CHECK_CHUNK`] 問ずつの往復を [`CHECK_CONCURRENCY`] 本まで並行させる（長い run の送り直しで
    /// 文書が数百あっても、直列に往復を重ねて最初の表示を待たせない）。
    ///
    /// 値は判定できたものだけ `Some(可否)`。判定に失敗した往復の分は `None`（呼び出し側は伏せる＝
    /// fail-closed。ただし「読めない」とは覚えない。一時的な失敗でその後の引用まで伏せ続けないため）。
    ///
    /// 引用は**文書の本文**（抜粋）を含むので、共有解除の反映が遅れると読めないはずの中身が
    /// 見えたままになる。thread の認可（`require_thread`）と同じ理由で HigherConsistency を使う
    /// （剥奪の即時反映を、キャッシュ由来の古い許可より優先する）。
    async fn can_view_files(
        &self,
        ctx: &AuthContext,
        node_ids: &[String],
    ) -> HashMap<String, Option<bool>> {
        let subject = ctx.subject();
        let chunks: Vec<&[String]> = node_ids.chunks(CHECK_CHUNK).collect();
        // 往復ごとの問いは手元に持ち切ってから流す（借用を future に持ち込まない）。
        let calls: Vec<_> = chunks
            .iter()
            .map(|ids| {
                let items: Vec<CheckTuple> = ids
                    .iter()
                    .map(|id| CheckTuple {
                        subject: subject.clone(),
                        relation: Relation::Viewer,
                        object: ctx.ns().file(id),
                    })
                    .collect();
                let authz = Arc::clone(&self.authz);
                async move {
                    authz
                        .batch_check(&items, Consistency::HigherConsistency)
                        .await
                        .ok()
                        .filter(|v| v.len() == items.len())
                }
            })
            .collect();
        let answers: Vec<Option<Vec<bool>>> = futures::stream::iter(calls)
            .buffered(CHECK_CONCURRENCY)
            .collect()
            .await;
        chunks
            .iter()
            .zip(answers)
            .flat_map(|(ids, answer)| {
                ids.iter()
                    .enumerate()
                    .map(move |(i, id)| (id.clone(), answer.as_ref().map(|v| v[i])))
            })
            .collect()
    }
}

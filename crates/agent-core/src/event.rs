//! エージェントループが外へ流すイベント（[`AgentEvent`]）と受け口（[`EventSink`]）。
//!
//! chat ワーカーが [`EventSink`] を実装し、各イベントを `generation_event` へ append
//! （真実のソース）＋ Redis pub/sub 配信する。ループはツール実行/トークンをこのイベントで
//! 逐次外部化し、chat 側で SSE の [`StreamEventKind`](../../chat) へ写す。

use crate::budget::BudgetKind;
use crate::plan::{Plan, SubtaskStatus};
use crate::tool::{ArtifactRef, Citation};

/// 失敗回復（Task 5.5）でループが取った行動。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryAction {
    /// エラー観測をモデルへ戻して自己修正を促した（継続）。
    Retry,
    /// 同一失敗のループを検出して安全停止した。
    StopLooping,
}

impl RecoveryAction {
    /// 監査・UI 表示用の安定文字列。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            RecoveryAction::Retry => "retry",
            RecoveryAction::StopLooping => "stop_looping",
        }
    }
}

/// エージェントループが外へ流すイベント（プロバイダ非依存・chat 非依存）。
///
/// Phase 3 の 6 種（Text/Thinking/ToolCall/ToolResult/Citation/Artifact）に、Phase 5 の
/// 自律セッション可視化（5.9）・監査（5.10）・承認（5.6）・予算（5.7）用の構造化イベントを足す。
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    /// 本文テキストの差分。
    Text(String),
    /// 思考テキストの差分。
    Thinking(String),
    /// ツール呼び出し（id/name/入力確定）。
    ///
    /// `step` は**そのループステップの通し番号**（0 始まり）。1 ステップで LLM が出した呼び出しは
    /// すべて同じ値を持つため、UI 側は「同一ステップ＝並行実行」をイベントだけで判別できる
    /// （`tool_call` の到達順から推測しない）。結果はステップ内の全ツール完了後に呼び出し順で
    /// まとめて流れる（`agent_tools` の観測順の決定性）。
    ToolCall {
        id: String,
        name: String,
        input: serde_json::Value,
        step: u32,
    },
    /// ツール結果。
    ToolResult {
        tool_call_id: String,
        ok: bool,
        content: String,
    },
    /// 引用（doc_search）。
    Citation(Citation),
    /// ツールが保存した成果物（code_interpreter）。chat 側で FileRef へ写す。
    Artifact {
        tool_call_id: String,
        artifact: ArtifactRef,
    },
    /// 検証済み generative UI スペック（emit_ui・Phase 6 Task 6.4）。
    /// **検証層（gui::SpecValidator）を通過した JSON のみ**がここへ乗る（生スペックは流さない）。
    GenerativeUi { spec: serde_json::Value },
    /// 保存済みワークフローへの参照（emit_workflow・Task 10.13）。
    /// **保存パイプライン（V1〜V7）を通過し artifact 化された参照のみ**がここへ乗る。
    WorkflowRef { workflow: serde_json::Value },
    /// 保存済みノートへの参照（save_note・Task 11P.5）。
    /// **StorageService へ作成済みのノードのみ**がここへ乗る（chat 側で note_ref へ写る）。
    NoteRef { note: serde_json::Value },
    /// 未保存の下書きノート（save_note の下書き確定型・issue #282）。
    /// `{name, markdown}`。**まだ作成していない**下書き本文（chat 側で note_draft へ写り、
    /// フロントが下書きノート画面で詰めてから「ドライブに保存」で確定する）。
    NoteDraft { draft: serde_json::Value },
    /// 未保存の下書きスライド（save_slide の下書き確定型・Task 11.3）。
    /// `{name, content}`（content=正規化スライド JSON 文字列）。**まだ作成していない**下書き
    /// （chat 側で slide_draft へ写り、フロントが下書きスライド画面で詰めてから確定する）。
    SlideDraft { draft: serde_json::Value },
    /// 未保存の下書き CSV（save_csv の下書き確定型・Task 11.11）。
    /// `{name, csv}`（csv=CSV 本文）。**まだ作成していない**下書き
    /// （chat 側で csv_draft へ写り、フロントが下書き CSV 画面で詰めてから確定する）。
    CsvDraft { draft: serde_json::Value },
    /// AI が作成/編集した文書への参照（#381）。`{id, name, kind, version}`。
    /// **StorageService 上に実在するノードのみ**がここへ乗る（chat 側で document_ref へ写り、
    /// 「〜を編集しました（v3）→ 開く」カードになる）。作成系と編集系が共有する。
    DocumentRef { document: serde_json::Value },
    /// skill ツールの発動記録（#344 Task 10.11）。`skill = {skill_id, skill_version, name}`。
    /// **発話ユーザー権限で解決に成功した**発動のみが乗る（`generation_event` に append され
    /// replay 可能＝「何をいつ適用したか」の完全な列。message.content へは projection しない —
    /// instructions は ToolResult block として既に履歴に残る）。
    SkillInvoked { skill: serde_json::Value },
    /// サブエージェント委譲の記録（#391）。
    /// `subagent = {objective, boundary, steps, tool_calls, tokens}`。
    ///
    /// **`SkillInvoked` と同型**: `generation_event` へ append して replay/監査に残すが、
    /// `message.content` へは projection しない（親のコンテキストも履歴も汚さない）。子の生の
    /// イベント（取得本文・思考）は親へ一切流れない。
    SubagentRun { subagent: serde_json::Value },
    /// 計画が改訂された（全サブタスク列・revision 付き・Task 5.2）。
    PlanUpdated(Plan),
    /// 単一サブタスクの状態遷移（軽量更新・Task 5.2）。
    SubtaskUpdated { id: String, status: SubtaskStatus },
    /// 予算上限への接近警告（種別・現在値・上限・Task 5.7）。
    BudgetWarning {
        kind: BudgetKind,
        used: u64,
        limit: u64,
    },
    /// 破壊系/egress/高コスト操作の承認要求（実行前にブロック・Task 5.6）。
    /// API 結線は W3。ここでは種別を定義し、ループが承認境界で発火できるようにする。
    ApprovalRequested {
        tool_call_id: String,
        name: String,
        input: serde_json::Value,
        reason: String,
    },
    /// 承認/却下の結果（Task 5.6）。
    ApprovalResolved {
        tool_call_id: String,
        approved: bool,
    },
    /// 失敗回復の判断（自己修正リトライ／ループ検出停止・Task 5.5）。
    FailureRecovery {
        detail: String,
        action: RecoveryAction,
    },
}

/// エージェントループのエラー。
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// LLM ゲートウェイ側の障害。
    #[error("llm error: {0}")]
    Llm(String),
    /// プロバイダの利用枠超過（429）。**文言はそのままユーザーへ出せる**ものが入る
    /// （`llm-gateway` が整形済み）ので、Display に接頭辞を付けない。
    #[error("{0}")]
    RateLimited(String),
    /// イベント永続化（sink）側の障害。
    #[error("sink error: {0}")]
    Sink(String),
    /// キャンセル要求で停止した（ユーザー明示停止）。
    #[error("cancelled")]
    Cancelled,
}

/// ループ外へイベントを流す受け口。chat ワーカーが実装する。
///
/// `emit` は append-only 永続化（＋pub/sub）を行うため async。`is_cancelled` はステップ境界と
/// ストリーム読取ループでの協調キャンセル検知に使う（ユーザー明示停止のみ・ページ離脱≠キャンセル）。
#[async_trait::async_trait]
pub trait EventSink: Send {
    async fn emit(&mut self, event: AgentEvent) -> Result<(), AgentError>;

    /// キャンセル要求が来ているか（協調キャンセル）。
    fn is_cancelled(&self) -> bool;

    /// ステップ境界のチェックポイントを durable run へ永続化する（resume 配線・#351）。
    ///
    /// ループはステップを完了するたびに呼ぶ。永続化しないシンク（テスト・短ホライズンの
    /// Chat プロファイル）は既定の no-op のまま。fencing 不一致（リース喪失）を検知した実装は
    /// `Err(AgentError::Sink)` を返してループを止めること（ゾンビ書込防止）。
    async fn save_checkpoint(
        &mut self,
        checkpoint: &crate::checkpoint::Checkpoint,
    ) -> Result<(), AgentError> {
        let _ = checkpoint;
        Ok(())
    }
}

/// 停止フラグを見直す間隔。差分が届かない間もこの間隔で [`EventSink::is_cancelled`] を確かめる。
const CANCEL_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// [`next_unless_cancelled`] の結果。
#[derive(Debug)]
pub enum NextDelta<T> {
    /// 次の要素が届いた。
    Item(T),
    /// ストリームが尽きた。
    End,
    /// 停止要求が来た。呼び出し側はストリームを drop して上流の読み取りを打ち切ること。
    Cancelled,
}

/// ストリームの次の要素を待ちつつ、その間も停止要求を見張る。
///
/// `stream.next().await` だけだと、プロバイダが黙っている間（推論モデルの思考中など）は次の差分か
/// 無通信のタイムアウトまで停止に気付けず、上流の生成と接続を握り続ける（#602）。
/// 待ちを [`CANCEL_POLL`] ごとに区切って停止フラグを見直す。`next()` は取り消しても要素を
/// 落とさないので、区切りで待ちを作り直しても取りこぼさない。
///
/// sink は読むだけだが `&mut` で受ける（`EventSink` は `Sync` を要求しないので、`&` を await を
/// またいで持つとループの future が `Send` でなくなる）。
pub async fn next_unless_cancelled<S>(
    stream: &mut S,
    sink: &mut dyn EventSink,
) -> NextDelta<S::Item>
where
    S: futures::Stream + Unpin + ?Sized,
{
    use futures::StreamExt;
    loop {
        if sink.is_cancelled() {
            return NextDelta::Cancelled;
        }
        match tokio::time::timeout(CANCEL_POLL, stream.next()).await {
            Ok(Some(item)) => return NextDelta::Item(item),
            Ok(None) => return NextDelta::End,
            Err(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    struct FlagSink(Arc<AtomicBool>);

    #[async_trait::async_trait]
    impl EventSink for FlagSink {
        async fn emit(&mut self, _event: AgentEvent) -> Result<(), AgentError> {
            Ok(())
        }
        fn is_cancelled(&self) -> bool {
            self.0.load(Ordering::Relaxed)
        }
    }

    #[tokio::test]
    async fn silent_stream_is_released_soon_after_cancel() {
        let flag = Arc::new(AtomicBool::new(false));
        let mut sink = FlagSink(flag.clone());
        // 何も返さないまま黙り続けるストリーム（思考中のプロバイダ）。
        let mut stream = futures::stream::pending::<u32>();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            flag.store(true, Ordering::Relaxed);
        });
        let started = std::time::Instant::now();
        let got = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            next_unless_cancelled(&mut stream, &mut sink),
        )
        .await
        .expect("停止後も待ち続けた");
        assert!(matches!(got, NextDelta::Cancelled));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[tokio::test]
    async fn passes_items_through_then_ends() {
        let mut sink = FlagSink(Arc::new(AtomicBool::new(false)));
        let mut stream = futures::stream::iter([1, 2]);
        assert!(matches!(
            next_unless_cancelled(&mut stream, &mut sink).await,
            NextDelta::Item(1)
        ));
        assert!(matches!(
            next_unless_cancelled(&mut stream, &mut sink).await,
            NextDelta::Item(2)
        ));
        assert!(matches!(
            next_unless_cancelled(&mut stream, &mut sink).await,
            NextDelta::End
        ));
    }

    #[tokio::test]
    async fn slow_item_is_not_lost_across_poll_boundaries() {
        let mut sink = FlagSink(Arc::new(AtomicBool::new(false)));
        // 区切り（250ms）を何度かまたいでから届く要素も取りこぼさない。
        let mut stream = Box::pin(futures::stream::once(async {
            tokio::time::sleep(std::time::Duration::from_millis(700)).await;
            7
        }));
        assert!(matches!(
            next_unless_cancelled(&mut stream, &mut sink).await,
            NextDelta::Item(7)
        ));
    }

    #[tokio::test]
    async fn already_cancelled_returns_without_polling() {
        let mut sink = FlagSink(Arc::new(AtomicBool::new(true)));
        let mut stream = futures::stream::iter([1]);
        assert!(matches!(
            next_unless_cancelled(&mut stream, &mut sink).await,
            NextDelta::Cancelled
        ));
    }
}

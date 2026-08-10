//! サブエージェントのイベント収集シンク（#391）。
//!
//! [`super::subagent`] から切り出した（1 ファイル行数ゲート）。**隔離の要**なので
//! 単独で読めるようにしておく: 何を親へ渡し、何を捨てるかがここに全部ある。

use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::event::{AgentError, AgentEvent, EventSink};
use crate::tool::Citation;

/// 子のイベントを**親へ流さず**集める内部シンク（#391）。
///
/// 親の `generation_event` に子の生イベントを混ぜると SSE と projection が壊れる。ここで
/// ①最終本文（findings）②`Citation`③ツール名の列（監査用）だけを取り、他は捨てる。
pub(super) struct CollectingSink {
    /// 子のツール実行を親の UI へ中継する送り口（LLM コンテキストへは入れない）。
    pub(super) tool_events: Option<tokio::sync::mpsc::UnboundedSender<AgentEvent>>,
    pub(super) text: String,
    pub(super) citations: Vec<Citation>,
    pub(super) tool_calls: Vec<String>,
    /// ステップ境界で観測した消費（`save_checkpoint` 経由）。
    ///
    /// `run_agent` が `Err`（LLM 障害等）で抜けると `AgentOutcome` に到達せず、**途中まで
    /// 消費したトークンが親の予算に計上されない**（＝予算の抜け穴・レビュー指摘 Critical）。
    /// ループはステップを完了するたびにチェックポイントを渡してくるので、その `spent` を控えて
    /// エラー経路でも計上する。取りこぼすのは失敗したステップ自身の分だけ。
    pub(super) spent: crate::budget::Spent,
    pub(super) cancel: Arc<std::sync::atomic::AtomicBool>,
}

impl CollectingSink {
    pub(super) fn new(
        cancel: Arc<std::sync::atomic::AtomicBool>,
        tool_events: Option<tokio::sync::mpsc::UnboundedSender<AgentEvent>>,
    ) -> Self {
        CollectingSink {
            tool_events,
            text: String::new(),
            citations: Vec::new(),
            tool_calls: Vec::new(),
            spent: crate::budget::Spent::default(),
            cancel,
        }
    }
}

#[async_trait::async_trait]
impl EventSink for CollectingSink {
    async fn emit(&mut self, event: AgentEvent) -> Result<(), AgentError> {
        // ツールの実行だけは**親の UI へ**中継する（何を調べているかが見えないと、委譲した
        // 瞬間に画面が止まって見える）。中継先は generation_event＝イベント経路であり、
        // 親の LLM コンテキストにも履歴にも入らない。送り先が閉じていても無視する。
        if let Some(tx) = &self.tool_events {
            if matches!(
                event,
                AgentEvent::ToolCall { .. } | AgentEvent::ToolResult { .. }
            ) {
                let _ = tx.send(event.clone());
            }
        }
        match event {
            AgentEvent::Text(t) => self.text.push_str(&t),
            AgentEvent::Citation(c) => self.citations.push(c),
            AgentEvent::ToolCall { name, .. } => {
                self.tool_calls.push(name);
                // ツール呼び出しの前に出た本文は「これから調べます」の前置き。findings は
                // **ツールを呼ばずに終わった最後のステップ**の本文なので、ここで捨てる。
                self.text.clear();
            }
            // Thinking / ToolResult / 予算警告などは親の**文脈**へは出さない（生の観測を漏らさない）。
            _ => {}
        }
        Ok(())
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// ステップ境界の消費を控える（エラー経路でも親へ計上するため）。永続化はしない。
    async fn save_checkpoint(
        &mut self,
        checkpoint: &crate::checkpoint::Checkpoint,
    ) -> Result<(), AgentError> {
        self.spent = checkpoint.spent;
        Ok(())
    }
}

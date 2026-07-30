//! 実行前フェーズの門（#400）。**ツール提示で**確認フェーズを保証する。
//!
//! deep research の「質問 → 計画 → 実行」は当初 instructions だけで止めていたが、実 LLM
//! （DeepSeek V4 Pro）＋実 web で 3 回回すと **同じ指示で止まる run と素通りする run が出た**
//! （2 回目は計画カードを出して停止、3 回目は 22 ツール呼び出しで完走）。承認機構が確率的に
//! しか効かないのは機構ではない。
//!
//! このリポジトリの整理（#344）どおり、**決定性はツール実装＋認可＋承認ゲートが担う**。
//! ここでは「その run で何を提示するか」を状態から決め、承認前は調査系ツールを**渡さない**。
//! モデルは物理的に調査できないので、カードを出す以外に進みようがない。
//!
//! 段階（`skill` の `command.variants[].phase = plan_first` を宣言した variant のみ）:
//!
//! | スレッドの状態 | 段階 | 提示するツール |
//! |---|---|---|
//! | 質問カードがまだ無い | 明確化 | `emit_ui` のみ |
//! | 質問カードがある・計画カードが無い | 計画 | `emit_ui` ＋ `plan` ＋ 作業ファイル |
//! | 計画カードがある | 実行 | 全ツール（従来どおり） |
//!
//! 段階の**順序**（質問が先）はツールだけでは強制できない（どちらも `emit_ui`）。ただし
//! 明確化の段階では `emit_ui` しか無いため、調査へ逃げる経路が存在しない。

use agent_core::{Tool, ToolName};
use std::sync::Arc;

/// 実行前フェーズの段階。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GateStage {
    /// 明確化（質問カードを出す）。`emit_ui` のみ提示する。
    Clarify,
    /// 計画（計画カードを出して承認を待つ）。調査系はまだ提示しない。
    Plan,
    /// 実行（承認済み）。従来どおり全ツール。
    Execute,
}

/// 計画フェーズまで**提示しない**ツール（調査＝外部/社内へ取りに行く操作と委譲）。
///
/// 作業ファイル（`fs_write`/`fs_read`…）は brief/outline を書くために計画段階でも要る。
const RESEARCH_TOOLS: [ToolName; 4] = [
    ToolName::WebSearch,
    ToolName::WebFetch,
    ToolName::DocSearch,
    ToolName::Subagent,
];

impl GateStage {
    /// この段階で提示してよいツールか。
    fn allows(self, name: &str) -> bool {
        let Some(tool) = ToolName::parse(name) else {
            // `plan` メタツールは `ToolName` 外（ループが横取りする）。ここへは来ない。
            return true;
        };
        match self {
            // 明確化では質問カードを出すこと以外にできることを持たせない。
            GateStage::Clarify => tool == ToolName::EmitUi,
            // 計画では作業ファイルと UI のみ（調査は承認後）。
            GateStage::Plan => !RESEARCH_TOOLS.contains(&tool),
            GateStage::Execute => true,
        }
    }

    /// 段階に応じて提示ツールを絞る（`Execute` は素通し）。
    pub(super) fn filter(self, tools: &mut Vec<Arc<dyn Tool>>) {
        if self == GateStage::Execute {
            return;
        }
        tools.retain(|t| self.allows(t.name()));
    }

    /// system プロンプトへ足す、この段階でやることの明示。
    ///
    /// ツールを絞っただけだと、モデルは「検索できない」ことに戸惑って言い訳を書くことがある。
    /// 何をすれば次へ進むのかを 1 行で伝える。
    pub(super) fn system_note(self) -> Option<&'static str> {
        match self {
            GateStage::Clarify => Some(
                "\n\n# いまのフェーズ: 明確化\n\
                 この発話では**調査ツールを提示していません**（承認前だからです）。\
                 `emit_ui` の `question_card` で最大 3 問だけ聞き、ターンを終えてください。\
                 回答が返ったら次のターンで計画を出します。",
            ),
            GateStage::Plan => Some(
                "\n\n# いまのフェーズ: 計画\n\
                 この発話でも**調査ツールは提示していません**。brief と outline を書き、\
                 `emit_ui` の `plan_card` を出してターンを終えてください。ユーザーが\
                 「この計画で開始」を押すと、次のターンで調査ツールが使えるようになります。\n\
                 \n\
                 計画の `steps` は **5〜7 個すべてを問いの形**にすること（`title` が「〜か」\
                 「〜はどこから来るか」「〜はどの条件で成り立つか」で終わる）。\
                 **「〜の検索」「〜の調査」「〜の収集」「〜の整理」「レポート作成」は禁止**\
                 — これは作業工程であって問いではなく、依頼が変わっても同じ文面になるため\
                 ユーザーは何も直せない。`description` には**何を根拠に決着させるか**\
                 （見る指標・当たる情報源・比較の軸）を書く。\
                 `intro` は定型文を書かず、1 文目「〈依頼〉に〈切り口〉で答えます」／\
                 2 文目「〈これ〉は対象外です」の型にすること。",
            ),
            GateStage::Execute => None,
        }
    }
}

/// スレッドに出ている genui カードの有無から段階を決める（#400）。
///
/// `has_question` / `has_plan` は「そのカードを**出した**か」。押されたかは見ない:
/// カードは出した時点でターンが終わり、次の run はユーザーの反応（回答/承認/修正）でしか
/// 起きないため、「出た＝ユーザーが見て次へ進めた」と同値になる。
pub(super) fn stage_from_cards(has_question: bool, has_plan: bool) -> GateStage {
    match (has_question, has_plan) {
        (_, true) => GateStage::Execute,
        (true, false) => GateStage::Plan,
        (false, false) => GateStage::Clarify,
    }
}

impl super::ChatWorker {
    /// この run の実行前フェーズ段階を決める（#400）。
    ///
    /// `plan_first` を宣言した variant で起動された run だけが門の対象。判定材料:
    /// ①ピンされた skill の `command.variants[].phase` ②その variant の `args` が発話の
    /// コマンド部分と一致するか ③スレッドに出ているカード。**ハードコードしない**
    /// （どの skill が確認フェーズを要るかは skill 自身が宣言する）。
    pub(super) async fn plan_gate_stage(
        &self,
        ctx: &authz::AuthContext,
        run: &crate::store::ClaimedRun,
        skills: &[crate::skill::AppliedSkill],
    ) -> Result<GateStage, crate::ChatError> {
        let text = self.run_message_text(run).await;
        if !skills.iter().any(|s| declares_plan_first(s, &text)) {
            return Ok(GateStage::Execute);
        }
        let (has_question, has_plan) = self
            .store
            .emitted_cards(run.thread_id, &ctx.tenant_id)
            .await?;
        Ok(stage_from_cards(has_question, has_plan))
    }

    /// この run を起こしたユーザー発話の本文（コマンド判定に使う）。
    async fn run_message_text(&self, run: &crate::store::ClaimedRun) -> String {
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT string_agg(b->>'text', '') FROM message m, \
                    jsonb_array_elements(m.content) b \
             WHERE m.id = (SELECT message_id FROM generation_run WHERE run_id = $1) \
               AND b->>'type' = 'text'",
        )
        .bind(run.run_id)
        .fetch_optional(&self.db)
        .await
        .ok()
        .flatten()
        .flatten()
        .unwrap_or_default()
    }
}

/// この skill が、発話で使われた variant に対して `plan_first` を宣言しているか。
///
/// 発話は `/<command> [<args>] <依頼>` のリテラル（#387）。`args` の長い順に見て最長一致を
/// 採る（`""` と `"auto"` が両方あるとき `auto` を先に当てる）。
fn declares_plan_first(skill: &crate::skill::AppliedSkill, text: &str) -> bool {
    let Some(command) = &skill.body.command else {
        return false;
    };
    let rest = match text
        .trim_start()
        .strip_prefix(&format!("/{}", command.name))
    {
        Some(rest) => rest.trim_start(),
        None => return false,
    };
    let mut variants: Vec<_> = command.variants.iter().collect();
    variants.sort_by_key(|v| std::cmp::Reverse(v.args.len()));
    variants
        .into_iter()
        .find(|v| {
            v.args.is_empty()
                || rest
                    .strip_prefix(&v.args)
                    .is_some_and(|r| r.is_empty() || r.starts_with(char::is_whitespace))
        })
        .is_some_and(|v| v.phase == Some(gui::CommandPhase::PlanFirst))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_advances_with_cards() {
        assert_eq!(stage_from_cards(false, false), GateStage::Clarify);
        assert_eq!(stage_from_cards(true, false), GateStage::Plan);
        assert_eq!(stage_from_cards(true, true), GateStage::Execute);
        // 質問を出さずに計画だけ出した run（モデルの判断）でも、承認後は実行へ進む。
        assert_eq!(stage_from_cards(false, true), GateStage::Execute);
    }

    #[test]
    fn clarify_offers_only_emit_ui() {
        let s = GateStage::Clarify;
        assert!(s.allows("emit_ui"));
        for denied in [
            "web_search",
            "web_fetch",
            "doc_search",
            "subagent",
            "fs_write",
        ] {
            assert!(!s.allows(denied), "{denied} は明確化で提示しない");
        }
    }

    #[test]
    fn plan_offers_workspace_but_no_research() {
        let s = GateStage::Plan;
        for allowed in ["emit_ui", "fs_write", "fs_append", "fs_read", "save_note"] {
            assert!(s.allows(allowed), "{allowed} は計画段階でも使う");
        }
        for denied in ["web_search", "web_fetch", "doc_search", "subagent"] {
            assert!(!s.allows(denied), "{denied} は承認後にだけ渡す");
        }
    }

    #[test]
    fn execute_allows_everything() {
        for name in ["web_search", "subagent", "shell", "fs_delete"] {
            assert!(GateStage::Execute.allows(name));
        }
    }
}

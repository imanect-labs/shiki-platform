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
//! 段階（run のピンに `phase = plan_first` が焼かれているときのみ・[`crate::SkillPin`]）:
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
            // 計画では作業ファイル・UI に加えて `subagent` を通す（#402）。委譲そのものは
            // 調査だが、**計画ロール**の子はツールを 1 つも持たず調べない（手順書を知らない
            // 文脈で「何を確かめるべきか」だけを立てる）。研究ロールで呼んでも子に調査ツールは
            // 渡るため、ここは「計画を子に書かせる」導線として開ける。
            GateStage::Plan => tool == ToolName::Subagent || !RESEARCH_TOOLS.contains(&tool),
            GateStage::Execute => true,
        }
    }

    /// 段階に応じて提示ツールを絞り、`emit_ui` の出せる形も限定する（`Execute` は素通し）。
    ///
    /// ツールを落とすだけでは足りない: 明確化で `emit_ui` しか渡していないのに、その
    /// `emit_ui` で計画カードを出されて質問フェーズが飛ばされた（#402）。この段階で
    /// 出せるカードそのものを指定する。
    pub(super) fn filter(
        self,
        tools: &mut Vec<Arc<dyn Tool>>,
        ui_validator: Option<&Arc<gui::SpecValidator>>,
    ) {
        if self == GateStage::Execute {
            return;
        }
        tools.retain(|t| self.allows(t.name()));
        let Some(allowed) = self.allowed_cards() else {
            return;
        };
        let Some(validator) = ui_validator else {
            return;
        };
        // 制限つきの emit_ui へ差し替える（提示ツールの最終形に対して効かせる）。
        tools.retain(|t| t.name() != ToolName::EmitUi.as_str());
        tools.push(Arc::new(
            gui::EmitUiTool::new(validator.clone()).with_allowed_root(allowed),
        ));
    }

    /// この段階で `emit_ui` のルートに出せるカード（`None` は制限なし）。
    fn allowed_cards(self) -> Option<Vec<gui::ComponentKind>> {
        match self {
            GateStage::Clarify => Some(vec![gui::ComponentKind::QuestionCard]),
            GateStage::Plan => Some(vec![gui::ComponentKind::PlanCard]),
            GateStage::Execute => None,
        }
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
                 計画の中身は**自分で書かず** `subagent` に `role: \"plan\"` で書かせること。\
                 `objective` にはユーザーの依頼（と質問カードの回答）だけを渡す。\
                 手順書を知っているあなたが書くと「brief を書く」「証拠台帳を作る」のような\
                 **自分の作業**が計画に混ざる。返ってきた JSON の `title` / `intro` / `steps` は\
                 **書き換えずそのまま** `plan_card` に載せる。",
            ),
            GateStage::Execute => None,
        }
    }
}

/// このピンが焼いた起動 variant を、解決済み skill body から引き直してフェーズ宣言を返す。
///
/// 引けない組み合わせ（コマンド起動でないピン・body から消えた variant・別バージョン）は
/// すべて `None`＝門を掛けない。skill は途中で版が変わり得るので**閉じる方向へ倒す**
/// （宣言が読めないのに調査ツールを取り上げると、ユーザーは何も進められなくなる）。
fn declared_phase(
    pin: &crate::SkillPin,
    skills: &[crate::skill::AppliedSkill],
) -> Option<gui::CommandPhase> {
    let args = pin.command_args.as_deref()?;
    skills
        .iter()
        .find(|s| s.id == pin.skill_id)?
        .body
        .command
        .as_ref()?
        .variant_by_args(args)?
        .phase
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
    /// 判定材料は ①run のピンに焼かれた**起動 variant**（[`crate::SkillPin::command_args`]）が
    /// 解決済み body で宣言している `phase` ②スレッドに出ているカード。**ハードコードしない**
    /// （どの skill が確認フェーズを要るかは skill 自身が宣言する）。
    ///
    /// 焼くのは variant の identity だけで、**意味はここで body から引き直す**。導出値
    /// （`phase`）を焼くと variant に宣言を足すたび同じ経路で落ちる。
    ///
    /// フェーズを**毎ターン発話本文から引き直さない**のが要点。カード回答・計画承認は
    /// `chat.submit` 由来でコマンドリテラルを持たないので、本文照合では 2 ターン目以降が
    /// 必ず素通りになる（#402 の実害。実測では 1 run で `emit_ui` が 6 回走り、質問と計画が
    /// 同じターンに出た）。
    pub(super) async fn plan_gate_stage(
        &self,
        ctx: &authz::AuthContext,
        run: &crate::store::ClaimedRun,
        skills: &[crate::skill::AppliedSkill],
    ) -> Result<GateStage, crate::ChatError> {
        if !run
            .skill_pins
            .0
            .iter()
            .any(|pin| declared_phase(pin, skills) == Some(gui::CommandPhase::PlanFirst))
        {
            return Ok(GateStage::Execute);
        }
        let (has_question, has_plan) = self
            .store
            .emitted_cards(run.thread_id, &ctx.tenant_id)
            .await?;
        Ok(stage_from_cards(has_question, has_plan))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `plan_first` を宣言した既定 variant を持つ skill 1 件。
    fn applied(id: uuid::Uuid) -> crate::skill::AppliedSkill {
        crate::skill::AppliedSkill {
            id,
            version: 1,
            name: "deep-research".into(),
            body: serde_json::from_value(serde_json::json!({
                "description": "d",
                "instructions": "i",
                "command": {
                    "name": "deep-research",
                    "variants": [
                        { "args": "", "summary": "s", "phase": "plan_first" },
                        { "args": "auto", "summary": "s" }
                    ]
                }
            }))
            .unwrap(),
        }
    }

    fn pin(id: uuid::Uuid, command_args: Option<&str>) -> crate::SkillPin {
        crate::SkillPin {
            skill_id: id,
            skill_version: 1,
            command_args: command_args.map(str::to_string),
        }
    }

    /// 焼かれた variant の identity から、解決済み body の宣言を引き直す（#402）。
    #[test]
    fn phase_comes_from_the_body_via_the_pinned_variant() {
        let id = uuid::Uuid::new_v4();
        let skills = [applied(id)];
        assert_eq!(
            declared_phase(&pin(id, Some("")), &skills),
            Some(gui::CommandPhase::PlanFirst),
            "既定 variant は確認フェーズを宣言している"
        );
        assert_eq!(
            declared_phase(&pin(id, Some("auto")), &skills),
            None,
            "auto は宣言なし＝門を掛けない"
        );
        // コマンド起動でないピン（thread の常設ピン）・別 skill・消えた variant は素通し。
        assert_eq!(declared_phase(&pin(id, None), &skills), None);
        assert_eq!(
            declared_phase(&pin(uuid::Uuid::new_v4(), Some("")), &skills),
            None
        );
        assert_eq!(declared_phase(&pin(id, Some("legacy")), &skills), None);
    }

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
        assert!(
            s.allows("subagent"),
            "計画を子に書かせる導線は開ける（#402）"
        );
        for denied in ["web_search", "web_fetch", "doc_search"] {
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

//! 実行前フェーズの門（#400）。**ツール提示で**確認フェーズを保証する。
//!
//! deep research の「質問 → 計画 → 実行」は当初 instructions だけで止めていたが、実 LLM
//! （DeepSeek V4 Pro）＋実 web で 3 回回すと **同じ指示で止まる run と素通りする run が出た**
//! （2 回目は計画カードを出して停止、3 回目は 22 ツール呼び出しで完走）。承認機構が確率的に
//! しか効かないのは機構ではない。
//!
//! このリポジトリの整理（#344）どおり、**決定性はツール実装＋認可＋承認ゲートが担う**。
//! ここでは「その run で何を提示するか」を状態から決め、承認前は調査系ツールを**渡さない**。
//! モデルは物理的に調査できないので、質問や計画を書く以外に進みようがない。
//!
//! 質問と計画は回答の本文の ```ui ブロック（Yui の `ask`・`plan`・#604）で出す。ボタンの回答・
//! 承認は会話への発話として返り（`ui_action_invocation` で 1 回だけ）、次の run を起こす。
//!
//! 段階（run のピンに `phase = plan_first` が焼かれているときのみ・[`crate::SkillPin`]）:
//!
//! | スレッドの状態 | 段階 | 提示するツール |
//! |---|---|---|
//! | `ask` をまだ出していない | 明確化 | なし（本文に `ask` を書くだけ） |
//! | `ask` を出した・その後の回答に `plan` が無い | 計画 | 調査系を除く（作業ファイル・`subagent`） |
//! | `ask` より後の回答に `plan` を出した | 実行 | 全ツール（従来どおり） |
//!
//! 段階の**順序**（質問が先）はツールでは強制できない（どちらも本文に書くだけ）。そこで、
//! `plan` は **`ask` を出した回答より後の回答**にあるものだけを数える。明確化の段階で `ask` と
//! `plan` を同じ回答に書いても（#402 の実害の形）、計画は出ていないものとして扱い、次の
//! 段階は計画になる。

use agent_core::{Tool, ToolName};
use std::sync::Arc;

/// 実行前フェーズの段階（宣言の順に進む）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GateStage {
    /// 明確化（本文に ```ui の `ask` を書く）。ツールは 1 つも提示しない。
    Clarify,
    /// 計画（本文に ```ui の `plan` を書いて承認を待つ）。調査系はまだ提示しない。
    Plan,
    /// 実行（承認済み）。従来どおり全ツール。
    Execute,
}

/// 計画フェーズまで**提示しない**ツール（調査＝外部/社内へ取りに行く操作と委譲）。
///
/// 作業ファイル（`fs_write`/`fs_read`…）は brief/outline を書くために計画段階でも要る。
const RESEARCH_TOOLS: [ToolName; 5] = [
    ToolName::WebSearch,
    ToolName::WebFetch,
    ToolName::DocSearch,
    ToolName::DocRead,
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
            // 明確化では質問を書くこと以外にできることを持たせない（質問は本文に書く）。
            GateStage::Clarify => false,
            // 計画では作業ファイルに加えて `subagent` を通す（#402）。委譲そのものは
            // 調査だが、**計画ロール**の子はツールを 1 つも持たず調べない（手順書を知らない
            // 文脈で「何を確かめるべきか」だけを立てる）。研究ロールで呼んでも子に調査ツールは
            // 渡るため、ここは「計画を子に書かせる」導線として開ける。
            GateStage::Plan => tool == ToolName::Subagent || !RESEARCH_TOOLS.contains(&tool),
            GateStage::Execute => true,
        }
    }

    /// 段階に応じて提示ツールを絞る（`Execute` は素通し）。
    ///
    /// 戻り値は**門が承認まで取り上げた**ツール名（積んであったのに外したもの）。発話ごとの
    /// ツール指定（#734）の注記で「使えない」ではなく「承認後に使える」と伝えるのに使う。
    pub(super) fn filter(self, tools: &mut Vec<Arc<dyn Tool>>) -> Vec<String> {
        if self == GateStage::Execute {
            return Vec::new();
        }
        let mut held = Vec::new();
        tools.retain(|t| {
            let keep = self.allows(t.name());
            if !keep {
                held.push(t.name().to_string());
            }
            keep
        });
        held
    }

    /// system プロンプトへ足す、この段階でやることの明示。
    ///
    /// ツールを絞っただけだと、モデルは「検索できない」ことに戸惑って言い訳を書くことがある。
    /// 何をすれば次へ進むのかを 1 行で伝える。
    pub(super) fn system_note(self) -> Option<&'static str> {
        match self {
            GateStage::Clarify => Some(
                "\n\n# いまのフェーズ: 明確化\n\
                 この発話では**ツールを提示していません**（承認前だからです）。\
                 回答の本文に ```ui の `ask` で最大 3 問だけ聞き、ターンを終えてください。\
                 回答が返ったら次のターンで計画を出します。書き方:\n\
                 ```ui\nask 確認させてください\n? 質問 1\n- 選択肢 A: 説明\n- 選択肢 B\n\
                 ? 質問 2 multi\n- 選択肢 C\n- 選択肢 D\n- その他\n```\n\
                 （`multi` は複数選択、`- その他` は自由記述を受ける。）",
            ),
            GateStage::Plan => Some(
                "\n\n# いまのフェーズ: 計画\n\
                 この発話でも**調査ツールは提示していません**。brief と outline を書き、\
                 回答の本文に ```ui の `plan` を書いてターンを終えてください。ユーザーが\
                 「この計画で進める」を押すと、次のターンで調査ツールが使えるようになります。書き方:\n\
                 ```ui\nplan 調査の計画\n1. 手順 1: 説明\n2. 手順 2\n```\n\
                 \n\
                 計画の中身は**自分で書かず** `subagent` に `role: \"plan\"` で書かせること。\
                 `objective` にはユーザーの依頼（と質問への回答）だけを渡す。\
                 手順書を知っているあなたが書くと「brief を書く」「証拠台帳を作る」のような\
                 **自分の作業**が計画に混ざる。返ってきた JSON の `title` / `intro` / `steps` は\
                 **書き換えずそのまま** `plan` に載せる（`title` を見出し、`intro` を `> ` で始まる前置きの行、\
                 `steps` を番号付きの行に）。",
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

/// 回答の本文（text ブロックを会話の順に）から段階を決める（#400・#604）。
///
/// 数えるのはトップレベルの、閉じていて、エラーの無い ```ui ブロックだけ（ボタンの経路と同じ
/// `yui::extract_action_blocks`）。`plan` は **`ask` を最初に出した回答より後の回答**にあるものだけを
/// 数える（明確化の段階で両方を同じ回答に書かれても、質問を飛ばして実行へ進ませない）。
///
/// 押されたかは見ない: 質問・計画を出した時点でターンが終わり、次の run はユーザーの反応
/// （回答・承認・修正の依頼・自由な発話）でしか起きないため、「出た＝ユーザーが見て次へ進めた」と同値になる。
pub(super) fn stage_from_answers<'a>(
    texts: impl IntoIterator<Item = (uuid::Uuid, &'a str)>,
) -> GateStage {
    let mut asked_in: Option<uuid::Uuid> = None;
    for (message_id, text) in texts {
        for block in yui::extract_action_blocks(text) {
            match block.figure {
                yui::Figure::Ask { .. } if asked_in.is_none() => asked_in = Some(message_id),
                yui::Figure::Plan { .. } if asked_in.is_some_and(|m| m != message_id) => {
                    return GateStage::Execute;
                }
                _ => {}
            }
        }
    }
    if asked_in.is_some() {
        GateStage::Plan
    } else {
        GateStage::Clarify
    }
}

impl super::ChatWorker {
    /// この run の実行前フェーズ段階を決める（#400）。
    ///
    /// 判定材料は ①run のピンに焼かれた**起動 variant**（[`crate::SkillPin::command_args`]）が
    /// 解決済み body で宣言している `phase` ②回答の本文に出した ```ui の `ask`・`plan`。**ハードコードしない**
    /// （どの skill が確認フェーズを要るかは skill 自身が宣言する）。
    ///
    /// 焼くのは variant の identity だけで、**意味はここで body から引き直す**。導出値
    /// （`phase`）を焼くと variant に宣言を足すたび同じ経路で落ちる。
    ///
    /// フェーズを**毎ターン発話本文から引き直さない**のが要点。質問への回答・計画の承認は
    /// `chat.submit` 由来でコマンドリテラルを持たないので、本文照合では 2 ターン目以降が
    /// 必ず素通りになる（#402 の実害。実測では 1 run で質問と計画のカードが 6 回出て、
    /// 質問と計画が同じターンに出た）。
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
        let texts = self
            .store
            .fenced_answer_texts(run.thread_id, &ctx.tenant_id)
            .await?;
        Ok(stage_from_answers(
            texts.iter().map(|(id, text)| (*id, text.as_str())),
        ))
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

    const ASK: &str = "質問します。\n\n```ui\nask 確認\n? 対象は\n- 国内\n- 海外\n```";
    const PLAN: &str = "```ui\nplan 調査の計画\n1. 市場規模を調べる\n2. 比べる\n```";

    #[test]
    fn stage_advances_with_ask_then_plan() {
        let (a, b) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        assert_eq!(stage_from_answers([]), GateStage::Clarify);
        assert_eq!(
            stage_from_answers([(a, "```ui\nbar 売上\nA | 1\n```")]),
            GateStage::Clarify
        );
        assert_eq!(stage_from_answers([(a, ASK)]), GateStage::Plan);
        assert_eq!(
            stage_from_answers([(a, ASK), (b, PLAN)]),
            GateStage::Execute
        );
        // 質問の後に何度か回答を挟んでも、計画が出れば実行へ進む。
        let c = uuid::Uuid::new_v4();
        assert_eq!(
            stage_from_answers([(a, ASK), (c, "了解しました。"), (b, PLAN)]),
            GateStage::Execute
        );
    }

    /// 質問と計画を同じ回答に書かれても、質問を飛ばして実行へは進ませない（#402 の形）。
    #[test]
    fn plan_in_the_asking_answer_does_not_count() {
        let a = uuid::Uuid::new_v4();
        let both = format!("{ASK}\n\n{PLAN}");
        assert_eq!(stage_from_answers([(a, both.as_str())]), GateStage::Plan);
        // 同じ回答の別の text ブロック（ツールを挟んで切れた）でも同じ。
        assert_eq!(stage_from_answers([(a, ASK), (a, PLAN)]), GateStage::Plan);
        // 質問より前の計画も数えない。
        let b = uuid::Uuid::new_v4();
        assert_eq!(stage_from_answers([(b, PLAN), (a, ASK)]), GateStage::Plan);
    }

    /// 引用・箇条書きの中・閉じていない・エラーのあるブロックは数えない（ボタンの経路と同じ規則）。
    #[test]
    fn only_top_level_valid_blocks_count() {
        let a = uuid::Uuid::new_v4();
        let quoted = "> ```ui\n> ask 確認\n> ? 対象は\n> - 国内\n> ```";
        assert_eq!(stage_from_answers([(a, quoted)]), GateStage::Clarify);
        let unclosed = "```ui\nask 確認\n? 対象は\n- 国内\n";
        assert_eq!(stage_from_answers([(a, unclosed)]), GateStage::Clarify);
        let empty = "```ui\nask 確認\n```";
        assert_eq!(stage_from_answers([(a, empty)]), GateStage::Clarify);
    }

    #[test]
    fn clarify_offers_no_tools() {
        let s = GateStage::Clarify;
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
        for allowed in ["fs_write", "fs_append", "fs_read", "save_note"] {
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

    /// 名前だけを持つ提示ツール（門の絞り込みの検証用）。
    struct Named(&'static str);

    #[async_trait::async_trait]
    impl Tool for Named {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            ""
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn call(
            &self,
            _ctx: &authz::AuthContext,
            _input: serde_json::Value,
            _trace_id: Option<&str>,
        ) -> Result<agent_core::ToolOutcome, agent_core::ToolError> {
            unreachable!("門の検証では呼ばない")
        }
    }

    fn offered(names: &[&'static str]) -> Vec<Arc<dyn Tool>> {
        names
            .iter()
            .map(|n| Arc::new(Named(n)) as Arc<dyn Tool>)
            .collect()
    }

    /// 門は承認まで取り上げたツール名を返す（発話ごとのツール指定の注記に使う・#734）。
    #[test]
    fn filter_reports_tools_held_until_approval() {
        let base = ["web_search", "doc_search", "fs_write", "subagent"];
        let mut tools = offered(&base);
        let held = GateStage::Plan.filter(&mut tools);
        assert_eq!(held, vec!["web_search", "doc_search"]);
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert_eq!(names, vec!["fs_write", "subagent"]);

        let mut tools = offered(&base);
        let held = GateStage::Clarify.filter(&mut tools);
        assert_eq!(held, base.to_vec());
        assert!(tools.is_empty(), "明確化ではツールを 1 つも渡さない");

        let mut tools = offered(&base);
        assert!(GateStage::Execute.filter(&mut tools).is_empty());
        assert_eq!(tools.len(), base.len());
    }

    #[test]
    fn execute_allows_everything() {
        for name in ["web_search", "subagent", "shell", "fs_delete"] {
            assert!(GateStage::Execute.allows(name));
        }
    }
}

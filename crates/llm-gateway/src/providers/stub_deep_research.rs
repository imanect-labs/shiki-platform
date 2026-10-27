//! deep research（#387）の決定的駆動。
//!
//! `/deep-research` / `/deep-research auto` を**本番と同じ発話**で入口にし、
//! 質問（```ui ask）→ 計画（```ui plan）→ 調査 → レポート → 検証委譲 → 指摘の反映 → 出典カード → 下書きの各フェーズを
//! 実 LLM 無しで再現する（e2e とローカルのデモ用）。
//!
//! 質問への回答・計画の承認は会話への発話として**別 run** になるため、状態は履歴から復元する:
//! ターン数＝user メッセージ数、run 内の進行＝tool メッセージ数。

use super::stub_deep_research_specs::{DEEP_RESEARCH_PLAN, DEEP_RESEARCH_QUESTION};
use super::stub_stream::{text_stream, tool_calls_stream};
use crate::model::{Block, GenerateRequest, Role};
use crate::provider::DeltaStream;

/// `/deep-research` 起動を検出して、フェーズを決定的に再現するストリームを返す。
///
/// 実際のコマンド文字列（`/deep-research …` / `/deep-research auto …`）をそのまま入口にする
/// ため、e2e は**本番と同じ発話**で全経路を通せる。フェーズの進み方:
///
/// | 経路 | ターン 1 | ターン 2 | ターン 3 |
/// |---|---|---|---|
/// | 既定 | 質問（ask） | 計画（plan） | 調査 → レポート → 検証委譲 → 反映 → 出典カード → 下書き |
/// | `auto` | 調査 → レポート → 検証委譲 → 反映 → 出典カード → 下書き | – | – |
///
/// ターン数は履歴の user メッセージ数、run 内の進行は tool メッセージ数で数える
/// （回答・承認は会話への発話で**別 run** になるため、run を跨いでも状態が復元できる）。
pub(super) fn deep_research_call(req: &GenerateRequest, prompt_tokens: u64) -> Option<DeltaStream> {
    let command = req.messages.iter().find_map(|m| {
        if m.role != Role::User {
            return None;
        }
        m.content.iter().find_map(|b| match b {
            Block::Text { text } if text.trim_start().starts_with(DEEP_RESEARCH_COMMAND) => {
                Some(text.trim_start().to_string())
            }
            _ => None,
        })
    })?;
    let auto = command
        .strip_prefix(DEEP_RESEARCH_COMMAND)
        .is_some_and(|rest| rest.trim_start().starts_with("auto"));
    let turns = req.messages.iter().filter(|m| m.role == Role::User).count();
    let steps = req.messages.iter().filter(|m| m.role == Role::Tool).count();

    // 既定経路の確認フェーズ（`auto` は飛ばす）。本文に ```ui の ask・plan を書いてターンを終える
    // （門はこの段階でツールを渡さないので、本番もこの形になる・#604）。
    if !auto && turns <= 2 {
        let reply = if turns <= 1 {
            DEEP_RESEARCH_QUESTION
        } else {
            DEEP_RESEARCH_PLAN
        };
        return Some(text_stream(reply, prompt_tokens));
    }
    // 調査の段階はツールで進む。ツールの無い要求（要約など）はここでは扱わない。
    if req.tools.is_empty() {
        return None;
    }

    // 調査フェーズ。1 ステップに複数ツールを載せ、冪等 read の有界並列（#349）と
    // 逐次書込が同じステップに混在する実際の形を再現する。
    // 進捗カード（`plan` メタツール）は手順書どおり**節目のステップで一緒に**進める。
    let mut calls: Vec<(String, serde_json::Value)> = match steps {
        0 => progress(req, ["doing", "todo", "todo", "todo"]),
        2 => progress(req, ["done", "doing", "todo", "todo"]),
        3 => progress(req, ["done", "done", "doing", "todo"]),
        5 => progress(req, ["done", "done", "done", "done"]),
        _ => Vec::new(),
    };
    calls.extend(match steps {
        // ① 検索 ＋ brief の作成。
        0 => tools_of(
            req,
            &[
                (
                    "web_search",
                    serde_json::json!({ "query": "SaaS 市場規模 2026" }),
                ),
                (
                    "fs_write",
                    serde_json::json!({
                        "name": "brief.md",
                        "content": "# research brief\n\n- 問い: 2026 年の国内 SaaS 市場規模\n- 分類: depth-first\n"
                    }),
                ),
            ],
        ),
        // ② **別ホスト**の本文取得 3 件（並列）＋ 証拠台帳への追記（逐次）。
        1 => {
            let mut calls = tools_of(
                req,
                &[(
                    "web_fetch",
                    serde_json::json!({ "url": "https://example.com/stub-1" }),
                )],
            );
            if let Some(t) = req.tools.iter().find(|t| t.name == "web_fetch") {
                for url in ["https://example.org/stub-2", "https://example.net/stub-3"] {
                    calls.push((t.name.clone(), serde_json::json!({ "url": url })));
                }
            }
            calls.extend(tools_of(
                req,
                &[(
                    "fs_append",
                    serde_json::json!({
                        "name": "notes.md",
                        "content": "E1 | https://example.com/stub-1 | 2026-07-30 | 一次 | 市場は 1.2 兆円 | 「1兆2000億円」\nE2 | https://example.org/stub-2 | 2026-03-31 | 二次 | 別集計では 9800 億円 | 「9800億円」\n"
                    }),
                )],
            ));
            calls
        }
        // ③ レポートを作業領域へも残す（ドライブ還流はユーザー確定後）。
        2 => tools_of(
            req,
            &[(
                "fs_write",
                serde_json::json!({ "name": "report.md", "content": DEEP_RESEARCH_REPORT }),
            )],
        ),
        // ④ 裏取りを**独立した検証者**へ委譲する（#407。書いた本人に検証させない）。
        3 => tools_of(
            req,
            &[(
                "subagent",
                serde_json::json!({
                    "role": "verify",
                    "objective": "report.md の実質的な主張が notes.md の証拠に紐づいているか検証し、\
                                  未確認・誤引用・過剰な一般化を指摘してください"
                }),
            )],
        ),
        // ⑤ 検証の**指摘を反映**する（fs_edit）。ここを飛ばすと「独立検証済み」を装いながら
        // 指摘を無視するレポートが提出されてしまう（検証を回した意味が消える）。
        4 => tools_of(
            req,
            &[(
                "fs_edit",
                serde_json::json!({
                    "name": "report.md",
                    "old_text": DEEP_RESEARCH_UNVERIFIED,
                    "new_text": DEEP_RESEARCH_VERIFIED
                }),
            )],
        ),
        // ⑥ レポート全文を下書きノート化（本文の下に保存ボタンが出る）。
        5 => tools_of(
            req,
            &[(
                "save_note",
                serde_json::json!({
                    "name": "2026年 国内SaaS市場の調査",
                    "markdown": deep_research_final()
                }),
            )],
        ),
        _ => Vec::new(),
    });
    if calls.is_empty() {
        // 最後の発話も**反映後**の本文にする。検証で直した箇所を会話で言い直したら、
        // ユーザーが受け取るものとして直っていないのと同じ。出典は本文の末尾にリンクで並べる
        // （#604・旧 emit_ui の出典カードの置き換え）。
        return Some(text_stream(
            &format!("{}{DEEP_RESEARCH_SOURCES}", deep_research_final()),
            prompt_tokens,
        ));
    }
    Some(tool_calls_stream(calls, prompt_tokens))
}

/// 提示ツールに存在するものだけを (name, input) で拾う（未配線構成でも落ちない）。
fn tools_of(
    req: &GenerateRequest,
    wanted: &[(&str, serde_json::Value)],
) -> Vec<(String, serde_json::Value)> {
    wanted
        .iter()
        .filter_map(|(name, input)| {
            req.tools
                .iter()
                .find(|t| t.name == *name)
                .map(|t| (t.name.clone(), input.clone()))
        })
        .collect()
}

/// 進捗カードの項目（計画の問い＋執筆・検証・提出）。状態は項目と同じ順に渡す。
const DEEP_RESEARCH_PROGRESS: [&str; 4] = [
    "2026 年の実数はいくらか",
    "レポートを書く",
    "裏取り（検証）",
    "ノートにまとめて提出",
];

/// 進捗カードの更新（`plan` メタツール・全項目を同じ title で全置換）。提示に無ければ呼ばない。
fn progress(req: &GenerateRequest, statuses: [&str; 4]) -> Vec<(String, serde_json::Value)> {
    let subtasks: Vec<serde_json::Value> = DEEP_RESEARCH_PROGRESS
        .iter()
        .zip(statuses)
        .map(|(title, status)| serde_json::json!({ "title": title, "status": status }))
        .collect();
    tools_of(
        req,
        &[("plan", serde_json::json!({ "subtasks": subtasks }))],
    )
}

/// `/deep-research` の起動トークン（本番のコマンド文字列と同一）。
const DEEP_RESEARCH_COMMAND: &str = "/deep-research";

/// 決定的なレポート本文（出典つき・両論併記・未確認の明示を含む最小形）。
const DEEP_RESEARCH_REPORT: &str = "## 結論と確度\n\n国内 SaaS 市場は 2026 年時点で 1.2 兆円規模とみられる（独立 2 系統が一致）。\n成長率は出典 1 系統のみで、確度は低い。\n\n## 市場規模\n\n2026 年の国内市場規模は 1 兆 2000 億円と公表されている（https://example.com/stub-1）。\n一方、3 月時点の別集計では 9800 億円とされ、集計範囲の違いが残る（https://example.org/stub-2）。\n\n```ui\nbar 2026 年の国内 SaaS 市場規模（公表値の比較）\nunit: 億円\n集計 | 市場規模\n7 月公表 | 12000\n3 月時点の別集計 | 9800\nnote: 出典: 国内 SaaS 市場調査（2026 年版）・別集計（2026 年 3 月時点）。集計範囲が異なる\n```\n\n公表値の差は 2200 億円で、集計範囲の違いで説明できるかは確認できていない。\n\n## 見つからなかったこと\n\n地域別の内訳は公表資料では確認できなかった。\n";

/// 会話の最後に並べる出典（本文で引用したものだけ・一次を上に）。
const DEEP_RESEARCH_SOURCES: &str = "\n## 出典\n\n- [国内 SaaS 市場調査（2026 年版）](https://example.com/stub-1)（一次）\n- [別集計（2026 年 3 月時点）](https://example.org/stub-2)（二次）\n";

/// 検証者が突く**過剰な一般化**（証拠は E1 の 1 系統しか無いのに「独立 2 系統が一致」と断定）。
const DEEP_RESEARCH_UNVERIFIED: &str = "1.2 兆円規模とみられる（独立 2 系統が一致）";

/// 指摘を反映した表現（出典が 1 系統であることを明示する）。
const DEEP_RESEARCH_VERIFIED: &str = "1.2 兆円規模とみられる（出典 1 系統・別集計とは不一致）";

/// 提出される最終レポート（**検証の指摘を反映した後**の本文）。
///
/// スタブが検証前の本文をそのまま提出すると、「独立検証済み」を装いながら指摘を無視する
/// 回帰を e2e が見逃す。反映後を提出させ、e2e は断定が消えたことまで見る。
fn deep_research_final() -> String {
    DEEP_RESEARCH_REPORT.replace(DEEP_RESEARCH_UNVERIFIED, DEEP_RESEARCH_VERIFIED)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{GenerateRequest, Message, ToolDef};

    fn tools(names: &[&str]) -> Vec<ToolDef> {
        names
            .iter()
            .map(|n| ToolDef::new(*n, "", serde_json::json!({})))
            .collect()
    }

    fn req(messages: Vec<Message>, tool_names: &[&str]) -> GenerateRequest {
        GenerateRequest {
            model: None,
            system: None,
            messages,
            tools: tools(tool_names),
            effort: None,
            max_tokens: None,
            temperature: None,
            session_id: None,
        }
    }

    fn user(text: &str) -> Message {
        Message::text(Role::User, text.to_string())
    }

    fn assistant_tool_use(name: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![Block::ToolUse {
                id: "t1".into(),
                name: name.into(),
                input: serde_json::json!({}),
            }],
        }
    }

    fn tool_result() -> Message {
        Message {
            role: Role::Tool,
            content: vec![Block::tool_result("t1", "ok", false)],
        }
    }

    /// ストリームから (ツール名, 入力) の列を取り出す（本文だけなら空）。
    async fn calls_of(stream: DeltaStream) -> Vec<(String, serde_json::Value)> {
        use futures::StreamExt;
        let mut names: Vec<(String, serde_json::Value)> = Vec::new();
        let mut pending: Vec<String> = Vec::new();
        let mut s = stream;
        while let Some(Ok(delta)) = s.next().await {
            match delta {
                crate::model::StreamDelta::ToolUseStart { name, .. } => pending.push(name),
                crate::model::StreamDelta::ToolUseStop { input, .. } => {
                    names.push((pending.remove(0), input));
                }
                _ => {}
            }
        }
        names
    }

    const ALL: &[&str] = &[
        "web_search",
        "web_fetch",
        "doc_search",
        "fs_write",
        "fs_append",
        "fs_edit",
        "save_note",
        "subagent",
    ];

    /// 非 deep-research の発話には一切反応しない（他のトリガを奪わない）。
    #[test]
    fn ignores_other_prompts() {
        assert!(deep_research_call(&req(vec![user("こんにちは")], ALL), 0).is_none());
        assert!(deep_research_call(&req(vec![user("websearch: 市場規模")], ALL), 0).is_none());
    }

    /// 調査の段階（`auto`・3 ターン目以降）はツールで進むので、ツールの無い要求は扱わない。
    #[test]
    fn research_phase_needs_tools() {
        assert!(deep_research_call(&req(vec![user("/deep-research auto SaaS")], &[]), 0).is_none());
        assert!(deep_research_call(&req(vec![user("/deep-research auto SaaS")], ALL), 0).is_some());
    }

    /// ストリームの本文を連結する。
    async fn text_of(stream: DeltaStream) -> String {
        use futures::StreamExt;
        let mut out = String::new();
        let mut s = stream;
        while let Some(Ok(delta)) = s.next().await {
            if let crate::model::StreamDelta::TextDelta { text } = delta {
                out.push_str(&text);
            }
        }
        out
    }

    /// 既定経路: 1 ターン目に質問（ask）、回答後（2 ターン目）に計画（plan）を本文に書いて止まる。
    /// どちらもツールは呼ばない（門が明確化・計画の段階でツールを渡さないのと同じ形）。
    #[tokio::test]
    async fn default_path_asks_then_plans() {
        let turn1 = deep_research_call(&req(vec![user("/deep-research SaaS 市場")], &[]), 0)
            .expect("質問フェーズ");
        let text = text_of(turn1).await;
        assert!(
            text.contains("```ui\nask 調査の前に確認させてください"),
            "{text}"
        );
        assert!(text.contains("button この条件で進める"));

        // 回答が投稿された次の run（user 2 件）は計画。
        let turn2 = deep_research_call(
            &req(
                vec![
                    user("/deep-research SaaS 市場"),
                    user("国内のみ / 意思決定用"),
                ],
                ALL,
            ),
            0,
        )
        .expect("計画フェーズ");
        let text = text_of(turn2).await;
        assert!(
            text.contains("```ui\nplan 2026 年の国内 SaaS 市場規模と成長率の検証"),
            "{text}"
        );
        assert!(text.contains("button この計画で開始"));
    }

    /// `auto` は確認を飛ばして 1 ターン目から調査に入り、
    /// 検索→並列取得＋追記→レポート→出典カード→下書きの順に進む。
    #[tokio::test]
    async fn auto_path_runs_research_phases_in_order() {
        let mut messages = vec![user("/deep-research auto SaaS 市場")];

        // ① 検索＋brief。
        let step0 = deep_research_call(&req(messages.clone(), ALL), 0).expect("step0");
        let calls = calls_of(step0).await;
        let names: Vec<&str> = calls.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["web_search", "fs_write"]);
        assert_eq!(calls[1].1["name"], "brief.md");

        // ② 別ホスト 3 件の取得（並列になる条件）＋ notes.md への追記。
        messages.push(assistant_tool_use("web_search"));
        messages.push(tool_result());
        let step1 = deep_research_call(&req(messages.clone(), ALL), 0).expect("step1");
        let calls = calls_of(step1).await;
        let fetches: Vec<&str> = calls
            .iter()
            .filter(|(n, _)| n == "web_fetch")
            .map(|(_, i)| i["url"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(fetches.len(), 3, "3 件を同一ステップで取る");
        let hosts: std::collections::BTreeSet<&str> = fetches
            .iter()
            .map(|u| u.split('/').nth(2).unwrap_or_default())
            .collect();
        assert_eq!(
            hosts.len(),
            3,
            "ホストが分かれていないと直列化されて並列を検証できない"
        );
        assert!(
            calls
                .iter()
                .any(|(n, i)| n == "fs_append" && i["name"] == "notes.md"),
            "証拠台帳は fs_append（全文置換ではない）"
        );

        // ③〜⑥ レポート保存 → 検証委譲 → **指摘の反映** → 下書きノート。
        for (tool, expect) in [
            ("fs_write", "report.md"),
            ("subagent", "verify"),
            ("fs_edit", "出典 1 系統"),
            ("save_note", "2026年 国内SaaS市場の調査"),
        ] {
            messages.push(assistant_tool_use("prev"));
            messages.push(tool_result());
            let s = deep_research_call(&req(messages.clone(), ALL), 0).expect("継続");
            let calls = calls_of(s).await;
            assert_eq!(calls.len(), 1, "各フェーズは 1 ツール");
            assert_eq!(calls[0].0, tool, "順序が固定される");
            let rendered = calls[0].1.to_string();
            assert!(rendered.contains(expect), "{tool}: {rendered}");
        }

        // 最後は本文（レポート＋出典のリンク）で終わる。
        messages.push(assistant_tool_use("save_note"));
        messages.push(tool_result());
        let last = deep_research_call(&req(messages.clone(), ALL), 0).expect("終了ターン");
        assert!(
            calls_of(last).await.is_empty(),
            "レポート本文で自然終了する"
        );
        let last = deep_research_call(&req(messages, ALL), 0).expect("終了ターン");
        let text = text_of(last).await;
        assert!(text.contains("## 出典"), "{text}");
        assert!(text.contains("](https://example.com/stub-1)"), "{text}");
    }

    /// 進捗カード（`plan`）は最初に 1 回出したきりにせず、節目のステップで一緒に進めて
    /// 提出時にすべて done になる（カードが 0 のまま動かない回帰を防ぐ）。
    #[tokio::test]
    async fn progress_card_advances_to_done() {
        let mut with_plan = ALL.to_vec();
        with_plan.push("plan");
        let mut messages = vec![user("/deep-research auto SaaS 市場")];
        let mut done_counts = Vec::new();
        for _ in 0..6 {
            let s = deep_research_call(&req(messages.clone(), &with_plan), 0).expect("継続");
            let calls = calls_of(s).await;
            if let Some((_, input)) = calls.iter().find(|(n, _)| n == "plan") {
                let subtasks = input["subtasks"].as_array().expect("subtasks");
                assert_eq!(
                    subtasks.len(),
                    DEEP_RESEARCH_PROGRESS.len(),
                    "毎回全項目を渡す"
                );
                for (st, title) in subtasks.iter().zip(DEEP_RESEARCH_PROGRESS) {
                    assert_eq!(st["title"], title, "title を変えると別項目になる");
                    assert!(st["status"].is_string(), "status を全項目に書く");
                }
                done_counts.push(subtasks.iter().filter(|s| s["status"] == "done").count());
            }
            assert!(
                calls.iter().any(|(n, _)| n != "plan"),
                "plan だけのステップを作らない"
            );
            messages.push(assistant_tool_use("prev"));
            messages.push(tool_result());
        }
        assert_eq!(
            done_counts,
            vec![0, 1, 2, 4],
            "節目ごとに進み、提出で完了する"
        );
    }

    /// 提出するレポートには図（```ui）が入り、図の出典を note に書く（ノートで図として描かれる）。
    #[test]
    fn report_includes_figure_with_source_note() {
        let report = deep_research_final();
        assert!(report.contains("```ui\nbar "), "{report}");
        assert!(report.contains("note: 出典: "), "{report}");
    }

    /// 図の数値（1.2 兆円・9800 億円）はどちらも証拠台帳（notes.md）に載っている。
    #[tokio::test]
    async fn figure_values_are_in_evidence_ledger() {
        let mut messages = vec![user("/deep-research auto SaaS 市場")];
        messages.push(assistant_tool_use("web_search"));
        messages.push(tool_result());
        let calls = calls_of(deep_research_call(&req(messages, ALL), 0).expect("step1")).await;
        let ledger = calls
            .iter()
            .find(|(n, i)| n == "fs_append" && i["name"] == "notes.md")
            .and_then(|(_, i)| i["content"].as_str().map(str::to_string))
            .expect("notes.md への追記");
        for value in ["1兆2000億円", "9800億円"] {
            assert!(ledger.contains(value), "{value} が台帳に無い: {ledger}");
        }
    }

    /// 未配線のツールは呼ばない（web 検索が無い構成でも run が壊れない）。
    #[tokio::test]
    async fn skips_unwired_tools() {
        let s = deep_research_call(
            &req(vec![user("/deep-research auto SaaS 市場")], &["fs_write"]),
            0,
        )
        .expect("step0");
        let calls = calls_of(s).await;
        assert_eq!(calls.len(), 1, "web_search は提示に無いので呼ばない");
        assert_eq!(calls[0].0, "fs_write");
    }
}

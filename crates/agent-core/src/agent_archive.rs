//! 元に戻せる圧縮（[`crate::archive`]）をループにつなぐ: 結果の退避・区間の保存・読み戻しの枠。
//!
//! 何を退避するかはコードが決める。1 ステップの結果の上限（[`crate::result_budget`]）で
//! 外した・削った結果と、要約に置き換えた区間だけ。モデルには参照と読み方を見せ、読み戻しは
//! メタツール（`recall_read` / `recall_search`）で回数と量を絞って許す。

use authz::AuthContext;
use llm_gateway::{Block, Message};

use crate::archive::{
    render_index, stub, ArchiveKind, ArchivedItem, ContextArchive, RECALL_PER_RUN, RECALL_PER_STEP,
    RECALL_READ, RECALL_SEARCH,
};
use crate::checkpoint::Checkpoint;
use crate::context::text_tokens;

/// 要約に付ける索引の最大件数（古いものは「ほか N 件」にまとめる）。
const INDEX_MAX: usize = 12;
/// 退避した結果の参照に最低限あてる量（トークン・読み方の説明が収まる）。
const STUB_MIN_TOKENS: usize = 160;

/// 読み戻しのメタツールか。
pub(crate) fn is_recall(name: &str) -> bool {
    name == RECALL_READ || name == RECALL_SEARCH
}

/// 1 ステップで許す読み戻し（回数と 1 回の量）。
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct RecallAllowance {
    /// このステップで実行できる回数。
    pub(crate) left: usize,
    /// 1 回の結果の量（手元の見積りのトークン）。
    pub(crate) max_tokens: usize,
}

impl RecallAllowance {
    /// `step_budget` = 1 ステップの結果の上限（見積りの単位・窓を知らなければ `None`）。
    /// 1 回は上限の半分まで（2 回で 1 ステップの上限に収まる）。
    pub(crate) fn for_step(state: &Checkpoint, step_budget: Option<usize>) -> Self {
        let run_left = usize::try_from(RECALL_PER_RUN.saturating_sub(state.recalls)).unwrap_or(0);
        RecallAllowance {
            left: RECALL_PER_STEP.min(run_left),
            max_tokens: step_budget.map_or(2_000, |b| (b / 2).max(300)),
        }
    }
}

/// 上限に達したときの返答（`run_exhausted` = run 全体の上限に達した）。
pub(crate) fn recall_limit_message(run_exhausted: bool) -> String {
    if run_exhausted {
        format!(
            "読み戻しは run 全体で {RECALL_PER_RUN} 回までで、上限に達しました。これ以上は読み戻せません。\
             確認を重ねず、今ある情報で結論を出して作業を終えてください（提出・保存がまだなら今すぐ）。"
        )
    } else {
        format!(
            "読み戻しは 1 ステップ {RECALL_PER_STEP} 回までです。この呼び出しは実行していません。\
             本当に要る部分だけを、次のステップで読んでください。"
        )
    }
}

fn next_ref(state: &mut Checkpoint, kind: ArchiveKind) -> String {
    state.archive_seq = state.archive_seq.saturating_add(1);
    let prefix = match kind {
        ArchiveKind::ToolResult => 'r',
        ArchiveKind::History => 'h',
    };
    format!("{prefix}{}", state.archive_seq)
}

/// 呼び出しの 1 行表記（`read_report(id=2025-04)`・長い引数は切る）。
fn call_label(name: &str, input: &serde_json::Value) -> String {
    let args = match input {
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(k, v)| match v {
                serde_json::Value::String(s) => format!("{k}={s}"),
                other => format!("{k}={other}"),
            })
            .collect::<Vec<_>>()
            .join(", "),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    };
    let args: String = if args.chars().count() > 120 {
        let mut a: String = args.chars().take(120).collect();
        a.push('…');
        a
    } else {
        args
    };
    format!("{name}({})", args.replace('\n', " "))
}

/// 結果の上限をかける前の本文（呼び出し ID → 本文・エラーか）。
pub(crate) fn snapshot(blocks: &[Block]) -> Vec<(String, String)> {
    blocks
        .iter()
        .filter_map(|b| match b {
            Block::ToolResult {
                tool_use_id,
                content,
                ..
            } => Some((tool_use_id.clone(), content.clone())),
            _ => None,
        })
        .collect()
}

/// 1 ステップの結果の上限で外した・削った結果を保存し、参照（先頭・末尾つき）に差し替える。
/// 戻り値は退避した件数。保存に失敗した結果は上限をかけた本文のまま残す。
///
/// `call_info` = (呼び出し ID, ツール名, 引数, 読み取り系か)。読み戻しの結果は退避しない
/// （読み戻しの読み戻しを作らない）。
pub(crate) async fn spill_step_results(
    archive: &dyn ContextArchive,
    ctx: &AuthContext,
    state: &mut Checkpoint,
    blocks: &mut [Block],
    before: &[(String, String)],
    call_info: &[(String, String, serde_json::Value, bool)],
    budget: usize,
) -> usize {
    let original = |id: &str| before.iter().find(|(i, _)| i == id).map(|(_, c)| c);
    let info = |id: &str| call_info.iter().find(|c| c.0 == id);
    let mut changed = Vec::new();
    let mut kept = 0usize;
    for (i, b) in blocks.iter().enumerate() {
        let Block::ToolResult {
            tool_use_id,
            content,
            ..
        } = b
        else {
            continue;
        };
        let spill = original(tool_use_id).is_some_and(|o| o != content)
            && info(tool_use_id).is_some_and(|c| !is_recall(&c.1));
        if spill {
            changed.push(i);
        } else {
            kept += text_tokens(content);
        }
    }
    if changed.is_empty() {
        return 0;
    }
    let per = (budget.saturating_sub(kept) / changed.len()).max(STUB_MIN_TOKENS);
    let mut spilled = 0;
    for i in changed {
        let Block::ToolResult {
            tool_use_id,
            content,
            is_error,
            ..
        } = &mut blocks[i]
        else {
            continue;
        };
        let (Some(full), Some(call)) = (original(tool_use_id), info(tool_use_id)) else {
            continue;
        };
        let item = ArchivedItem {
            reference: next_ref(state, ArchiveKind::ToolResult),
            kind: ArchiveKind::ToolResult,
            label: call_label(&call.1, &call.2),
            status: if *is_error { "error" } else { "ok" }.to_string(),
            step: state.spent.steps,
            content: full.clone(),
        };
        let rendered = stub(&item, per);
        match archive.put(ctx, item).await {
            Ok(()) => {
                *content = rendered;
                spilled += 1;
            }
            Err(e) => {
                tracing::warn!(error = %e, "ツール結果の保存に失敗（上限をかけた本文で続行）");
            }
        }
    }
    spilled
}

/// 要約に置き換える区間を保存し、要約ブロックに保存済みの記録の索引を足す。
///
/// `middle` = 要約に回した区間（置き換え前の履歴）。`rebuilt` = 圧縮後の履歴（要約ブロックを
/// 含む）。保存に失敗しても圧縮は続ける（索引に載らないだけ）。
pub(crate) async fn archive_history(
    archive: &dyn ContextArchive,
    ctx: &AuthContext,
    state: &mut Checkpoint,
    middle: &[Message],
    rebuilt: &mut [Message],
) {
    let mut tools: Vec<(String, usize)> = Vec::new();
    for b in middle.iter().flat_map(|m| m.content.iter()) {
        if let Block::ToolUse { name, .. } = b {
            match tools.iter_mut().find(|(n, _)| n == name) {
                Some((_, c)) => *c += 1,
                None => tools.push((name.clone(), 1)),
            }
        }
    }
    let used = if tools.is_empty() {
        "ツール呼び出しなし".to_string()
    } else {
        tools
            .iter()
            .map(|(n, c)| format!("{n}×{c}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let item = ArchivedItem {
        reference: next_ref(state, ArchiveKind::History),
        kind: ArchiveKind::History,
        label: format!(
            "要約に置き換えたやり取り（ステップ {} まで・{used}）",
            state.spent.steps
        ),
        status: "ok".to_string(),
        step: state.spent.steps,
        content: crate::compaction::render_transcript(middle, usize::MAX),
    };
    if let Err(e) = archive.put(ctx, item).await {
        tracing::warn!(error = %e, "要約した区間の保存に失敗（索引なしで続行）");
    }
    let items = match archive.list(ctx).await {
        Ok(items) => items,
        Err(e) => {
            tracing::warn!(error = %e, "保存済みの記録の一覧に失敗（索引なしで続行）");
            return;
        }
    };
    let Some(index) = render_index(&items, INDEX_MAX) else {
        return;
    };
    let summary = rebuilt
        .iter_mut()
        .flat_map(|m| m.content.iter_mut())
        .find_map(|b| match b {
            Block::Text { text } if text.starts_with(crate::compaction::SUMMARY_MARK) => Some(text),
            _ => None,
        });
    if let Some(text) = summary {
        text.push('\n');
        text.push_str(&index);
        text.push_str(
            "要約に無い値が要るときは、ツールを呼び直す前に、まず上の記録を \
             recall_search で探してください（呼び直すと文脈をまた使います）。\n",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::InMemoryArchive;

    fn ctx() -> AuthContext {
        AuthContext::new(
            authz::Principal {
                kind: authz::PrincipalKind::User,
                id: "u".into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some("t-1".into()),
            },
            "o".into(),
            authz::TenantId::parse("t-1").unwrap(),
        )
    }

    fn info(id: &str, name: &str, ro: bool) -> (String, String, serde_json::Value, bool) {
        (
            id.into(),
            name.into(),
            serde_json::json!({"id": format!("doc-{id}")}),
            ro,
        )
    }

    #[tokio::test]
    async fn changed_results_are_archived_and_replaced_with_refs() {
        let archive = InMemoryArchive::new();
        let mut state = Checkpoint::start(vec![]);
        let big: String = (1..=200).map(|i| format!("行{i}\n")).collect();
        let mut blocks = vec![
            Block::tool_result("a", big.clone(), false),
            Block::tool_result("b", big.clone(), false),
            Block::tool_result("c", "読み戻しの結果", false),
        ];
        let before = snapshot(&blocks);
        // 上限で b を外した・c（読み戻し）も書き換わったとする。
        if let Block::ToolResult { content, .. } = &mut blocks[1] {
            *content = "［未表示］".into();
        }
        if let Block::ToolResult { content, .. } = &mut blocks[2] {
            *content = "短縮".into();
        }
        let calls = [
            info("a", "read", true),
            info("b", "read", true),
            info("c", RECALL_READ, false),
        ];
        let n = spill_step_results(
            &archive,
            &ctx(),
            &mut state,
            &mut blocks,
            &before,
            &calls,
            2_000,
        )
        .await;
        assert_eq!(n, 1);
        let Block::ToolResult { content, .. } = &blocks[1] else {
            panic!()
        };
        assert!(content.contains("保存済み r1: read(id=doc-b)") && content.contains("行200"));
        // 変わっていない結果・読み戻しの結果はそのまま。
        assert!(matches!(&blocks[0], Block::ToolResult { content, .. } if *content == big));
        assert!(matches!(&blocks[2], Block::ToolResult { content, .. } if content == "短縮"));
        let saved = archive.get(&ctx(), "r1").await.unwrap().unwrap();
        assert_eq!(saved.content, big);
        assert_eq!(state.archive_seq, 1);
    }

    #[tokio::test]
    async fn history_is_archived_and_indexed_in_the_summary() {
        let archive = InMemoryArchive::new();
        let mut state = Checkpoint::start(vec![]);
        let middle = vec![
            Message {
                role: llm_gateway::Role::Assistant,
                content: vec![Block::ToolUse {
                    id: "x".into(),
                    name: "read_report".into(),
                    input: serde_json::json!({}),
                }],
            },
            Message {
                role: llm_gateway::Role::Tool,
                content: vec![Block::tool_result("x", "札幌 1801", false)],
            },
        ];
        let mut rebuilt = vec![Message::text(
            llm_gateway::Role::User,
            format!("{}\n要約", crate::compaction::SUMMARY_MARK),
        )];
        archive_history(&archive, &ctx(), &mut state, &middle, &mut rebuilt).await;
        let Block::Text { text } = &rebuilt[0].content[0] else {
            panic!()
        };
        assert!(text.contains("- h1: 要約に置き換えたやり取り") && text.contains("read_report×1"));
        let saved = archive.get(&ctx(), "h1").await.unwrap().unwrap();
        assert!(saved.content.contains("札幌 1801"));
    }

    #[test]
    fn allowance_respects_step_and_run_limits() {
        let mut state = Checkpoint::start(vec![]);
        assert_eq!(
            RecallAllowance::for_step(&state, Some(4_000)).left,
            RECALL_PER_STEP
        );
        assert_eq!(
            RecallAllowance::for_step(&state, Some(4_000)).max_tokens,
            2_000
        );
        state.recalls = RECALL_PER_RUN - 1;
        assert_eq!(RecallAllowance::for_step(&state, None).left, 1);
        state.recalls = RECALL_PER_RUN + 3;
        assert_eq!(RecallAllowance::for_step(&state, None).left, 0);
        assert!(recall_limit_message(true).contains("結論を出して"));
        assert!(recall_limit_message(false).contains("1 ステップ 2 回まで"));
    }
}

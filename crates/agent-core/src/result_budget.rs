//! 1 ステップのツール結果の総量に上限をかける（窓を知っているモデルのみ・
//! docs/agent/small-model-harness.md）。
//!
//! 小さい窓のモデルが大きな文書を並列に何本も読むと、1 ステップの結果だけで窓を使い切る。
//! 剪定は「古い結果」を畳む仕組みなので、**まだ一度も読まれていない直近の結果**には使えない
//! （畳むと読み直し→また畳まれる、の空回りになった）。そこで結果を受け取った時点で、
//!
//! - 上限に収まる分は届いた順にそのまま返す（単独でも上限を超える 1 件だけは中ほどを削って返す）。
//! - 収まらない**読み取り系**（冪等）の結果は本文を返さず、「未表示。呼び直せば読める。
//!   一度に呼ぶ数を減らす」と正直に伝える（読み取りは呼び直しても副作用が無い）。
//! - **変更を伴うツール**の結果は既に実行済みなので落とさない（大きすぎるものだけ中ほどを削る）。

use std::collections::HashSet;

use llm_gateway::Block;

use crate::context::text_tokens;

/// 未表示にした結果の本文（目印を兼ねる）。
pub(crate) const DEFERRED_MARK: &str =
    "［未表示: 同じステップの他の結果で、文脈に載せられる量の上限に達したため、";

fn deferred_note() -> String {
    format!(
        "{DEFERRED_MARK}この結果は表示していません。読み取りの呼び出しなので、同じ引数で呼び直せば\
         読めます。表示された結果から必要な値を書き留めてから、一度に呼ぶ数を減らして呼び直して\
         ください。］"
    )
}

/// 中ほどを削って `max_tokens` 前後に縮める（頭と尻尾を残す・UTF-8 境界を守る）。
fn shorten(content: &str, max_tokens: usize, read_only: bool) -> String {
    // text_tokens は ASCII 4 バイト/1、非 ASCII 1 文字/1。バイト数へは保守的に 1 トークン≒3 バイト。
    let keep = max_tokens.saturating_mul(3) / 2;
    let mut a = keep.min(content.len());
    while a > 0 && !content.is_char_boundary(a) {
        a -= 1;
    }
    let mut b = content.len().saturating_sub(keep).max(a);
    while b < content.len() && !content.is_char_boundary(b) {
        b += 1;
    }
    let hint = if read_only {
        "範囲や条件を絞って読み直すと、省いた部分も読めます"
    } else {
        "この操作は実行済みです。呼び直さずに結果を確かめてください"
    };
    let head = crate::tools::document_envelope::close_dangling(&content[..a]);
    format!(
        "{head}\n［長すぎるため中ほど {} バイトを省略。{hint}。］\n{}",
        b - a,
        &content[b..]
    )
}

/// `blocks`（このステップのツール結果）を `budget_tokens` に収める。`read_only` は読み取り系
/// ツールの呼び出し ID。戻り値は未表示にした件数。
pub(crate) fn cap_step_results(
    blocks: &mut [Block],
    read_only: &HashSet<String>,
    budget_tokens: usize,
) -> usize {
    let total: usize = blocks.iter().map(result_tokens).sum();
    if total <= budget_tokens {
        return 0;
    }
    let mut used = 0usize;
    let mut deferred = 0usize;
    for b in blocks.iter_mut() {
        let Block::ToolResult {
            tool_use_id,
            content,
            ..
        } = b
        else {
            continue;
        };
        let t = text_tokens(content);
        if used + t <= budget_tokens {
            used += t;
            continue;
        }
        let room = budget_tokens.saturating_sub(used);
        if !read_only.contains(tool_use_id.as_str()) {
            // 実行済みの変更は落とさない。収まらない分だけ削る。
            *content = shorten(content, room.max(budget_tokens / 10).min(t), false);
            used += text_tokens(content);
        } else if used == 0 {
            // 単独でも上限を超える 1 件だけは、削ってでも見せる（呼び直しても同じ大きさのため）。
            // 収まる大きさなら削らない: 中ほどを削った文書は、要の部分（表など）が欠けたまま
            // 読み違えられる（実測: 16k 窓で表の欠けた報告を「確認済み」と書き、読み直しを重ねた）。
            // 省略の注記ぶんを空けておく。
            *content = shorten(content, budget_tokens.saturating_sub(200), true);
            used += text_tokens(content);
        } else {
            // 単独なら収まる結果は、削らずに「未表示・呼び直せる」にする（次は丸ごと読める）。
            *content = deferred_note();
            deferred += 1;
        }
    }
    deferred
}

fn result_tokens(b: &Block) -> usize {
    match b {
        Block::ToolResult { content, .. } => text_tokens(content),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn results(n: usize, size: usize) -> Vec<Block> {
        (0..n)
            .map(|i| Block::tool_result(format!("r{i}"), "報".repeat(size), false))
            .collect()
    }

    fn contents(bs: &[Block]) -> Vec<String> {
        bs.iter()
            .map(|b| match b {
                Block::ToolResult { content, .. } => content.clone(),
                _ => String::new(),
            })
            .collect()
    }

    #[test]
    fn under_budget_is_untouched() {
        let mut bs = results(3, 1000);
        let before = bs.clone();
        let ro: HashSet<String> = (0..3).map(|i| format!("r{i}")).collect();
        assert_eq!(cap_step_results(&mut bs, &ro, 10_000), 0);
        assert_eq!(bs, before);
    }

    #[test]
    fn read_results_beyond_budget_are_deferred_in_order() {
        let mut bs = results(12, 2500);
        let ro: HashSet<String> = (0..12).map(|i| format!("r{i}")).collect();
        let deferred = cap_step_results(&mut bs, &ro, 8_400);
        let c = contents(&bs);
        // 先頭の 3 件は全文、残りは未表示（呼び直せる旨）。
        assert!(c[..3].iter().all(|s| s.len() == 2500 * 3));
        assert!(c[3..].iter().all(|s| s.starts_with(DEFERRED_MARK)));
        assert_eq!(deferred, 9);
        let total: usize = c.iter().map(|s| text_tokens(s)).sum();
        assert!(total <= 8_400 + 9 * 200, "{total}");
    }

    #[test]
    fn a_single_huge_read_is_shortened_not_dropped() {
        let mut bs = results(1, 50_000);
        let ro: HashSet<String> = ["r0".to_string()].into();
        assert_eq!(cap_step_results(&mut bs, &ro, 9_000), 0);
        let c = &contents(&bs)[0];
        assert!(c.contains("中ほど") && c.contains("範囲や条件を絞って"));
        assert!(text_tokens(c) <= 9_000, "{}", text_tokens(c));
    }

    #[test]
    fn a_read_that_fits_alone_is_deferred_not_cut() {
        // 2 件目は単独なら収まる（4k ≦ 6k）が、1 件目の後には収まらない → 削らず未表示。
        let mut bs = results(2, 4000);
        let ro: HashSet<String> = ["r0", "r1"].iter().map(|s| (*s).to_string()).collect();
        assert_eq!(cap_step_results(&mut bs, &ro, 6_000), 1);
        let c = contents(&bs);
        assert_eq!(c[0].len(), 4000 * 3);
        assert!(c[1].starts_with(DEFERRED_MARK));
    }

    #[test]
    fn mutating_results_are_never_deferred() {
        let mut bs = results(4, 4000);
        // r2 だけ変更系（保存など）。
        let ro: HashSet<String> = ["r0", "r1", "r3"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        cap_step_results(&mut bs, &ro, 6_000);
        let c = contents(&bs);
        assert!(!c[2].starts_with(DEFERRED_MARK));
        assert!(c[2].contains("実行済み") || c[2].len() == 4000 * 3);
        assert!(c[3].starts_with(DEFERRED_MARK));
    }
}

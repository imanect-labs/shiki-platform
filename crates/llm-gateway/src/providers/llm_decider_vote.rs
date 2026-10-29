//! llm_decider の確からしさの出し方（`llm_decider.rs` から分割）。どちらも較正されていない（PIT-80）。
//!
//! - **自己申告**（1 回）: 申告 c（0〜1）を選んだ値の確率にする。ただし一様（1/n）より低くはしない。
//!   残りは他の値に均等に配る。引き上げた質問（はい/いいえで 50% 未満を申告したなど。取り違えや迷いの合図）は
//!   [`raised_below_uniform`] で数え、決定器がログに残す（値の扱いは変えない・D-78 (3)・BUG-124）。
//! - **多数決**（k 回）: 得票率を確率にし、確からしさは勝った値の得票率。同数なら自己申告の合計が大きい
//!   方、それも同じなら値域の順で先のもの（boolean は false が先）。
//!
//! score の `value` は確率で重み付けした平均の位置、`level_index` は確率が最大の段階。

use std::collections::BTreeMap;

use super::llm_decider_prompt::{arity, Reply};
use crate::decision::{Answer, DecideRequest, QuestionKind};

/// 自己申告から確率を組む（選んだ値は max(c, 1/n)・残りを均等に）。
fn spread(n: usize, chosen: usize, c: f64) -> Vec<f64> {
    #[allow(clippy::cast_precision_loss)] // 値域は 100 件まで
    let nf = n as f64;
    let top = c.max(1.0 / nf);
    let rest = if n > 1 { (1.0 - top) / (nf - 1.0) } else { 0.0 };
    (0..n)
        .map(|i| if i == chosen { top } else { rest })
        .collect()
}

/// 自己申告（1 回）の確からしさが一様（1/n・はい/いいえなら 0.5）より低く、[`spread`] が引き上げた質問のキー
/// （質問の順）。多数決（2 回以上）は得票率を使うので数えない。
pub(super) fn raised_below_uniform<'a>(
    req: &'a DecideRequest,
    samples: &[Vec<Reply>],
) -> Vec<&'a str> {
    let [one] = samples else {
        return Vec::new();
    };
    req.questions
        .iter()
        .zip(one)
        .filter(|(q, r)| {
            #[allow(clippy::cast_precision_loss)] // 値域は 100 件まで
            let uniform = 1.0 / arity(&q.kind).max(1) as f64;
            r.confidence < uniform
        })
        .map(|(q, _)| q.key.as_str())
        .collect()
}

/// 得票から確率と勝者を決める。
fn tally(n: usize, picks: &[Reply]) -> (usize, Vec<f64>) {
    let mut votes = vec![0usize; n];
    let mut weight = vec![0.0f64; n];
    for r in picks {
        if let Some(v) = votes.get_mut(r.index) {
            *v += 1;
            weight[r.index] += r.confidence;
        }
    }
    let mut winner = 0;
    for i in 1..n {
        let better =
            votes[i] > votes[winner] || (votes[i] == votes[winner] && weight[i] > weight[winner]);
        if better {
            winner = i;
        }
    }
    #[allow(clippy::cast_precision_loss)] // 回数は 5 まで
    let k = picks.len().max(1) as f64;
    #[allow(clippy::cast_precision_loss)]
    let probs = votes.iter().map(|&v| v as f64 / k).collect();
    (winner, probs)
}

/// 質問ごとに答えをまとめる。`samples` は回ごとの答え（どれも質問の順に並ぶ）。
pub(super) fn aggregate(req: &DecideRequest, samples: &[Vec<Reply>]) -> BTreeMap<String, Answer> {
    let mut out = BTreeMap::new();
    for (qi, q) in req.questions.iter().enumerate() {
        let picks: Vec<Reply> = samples.iter().filter_map(|s| s.get(qi).copied()).collect();
        let n = arity(&q.kind);
        let (winner, probs) = match picks.as_slice() {
            [one] => (one.index, spread(n, one.index, one.confidence)),
            many => tally(n, many),
        };
        let confidence = probs.get(winner).copied().unwrap_or(0.0);
        let answer = match &q.kind {
            QuestionKind::Choice { options } => Answer::Choice {
                value: options[winner].key.clone(),
                confidence,
                probabilities: options
                    .iter()
                    .zip(&probs)
                    .map(|(o, p)| (o.key.clone(), *p))
                    .collect(),
            },
            QuestionKind::Score { .. } => Answer::Score {
                #[allow(clippy::cast_precision_loss)]
                value: probs.iter().enumerate().map(|(i, p)| i as f64 * p).sum(),
                level_index: winner,
                confidence,
                probabilities: probs,
            },
            QuestionKind::Boolean { .. } => Answer::Boolean {
                value: winner == 1,
                probability: probs[1],
                confidence,
            },
        };
        out.insert(q.key.clone(), answer);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::llm_decider_prompt::tests::request;
    use super::*;

    fn r(index: usize, confidence: f64) -> Reply {
        Reply { index, confidence }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn a_single_reply_uses_the_verbalized_confidence() {
        let got = aggregate(&request(), &[vec![r(1, 0.8), r(2, 0.9), r(1, 0.3)]]);
        let Answer::Choice {
            value,
            confidence,
            probabilities,
        } = &got["category"]
        else {
            panic!("{got:?}")
        };
        assert_eq!(value, "bug");
        assert!(close(*confidence, 0.8));
        assert!(close(probabilities["billing"], 0.1) && close(probabilities["other"], 0.1));
        let Answer::Score {
            value,
            level_index,
            confidence,
            probabilities,
        } = &got["priority"]
        else {
            panic!()
        };
        assert_eq!(*level_index, 2);
        assert!(close(*confidence, 0.9));
        assert!(close(*value, 0.05 + 2.0 * 0.9));
        assert_eq!(probabilities.len(), 3);
        // はい/いいえで申告が 0.5 未満でも、答えと確率が食い違わない（0.5 で止める）。
        let Answer::Boolean {
            value,
            probability,
            confidence,
        } = got["angry"]
        else {
            panic!()
        };
        assert!(value);
        assert!(close(probability, 0.5) && close(confidence, 0.5));
    }

    #[test]
    fn a_low_verbalized_confidence_never_drops_below_uniform() {
        let got = aggregate(&request(), &[vec![r(0, 0.1), r(0, 0.0), r(0, 0.95)]]);
        assert!(close(got["category"].confidence(), 1.0 / 3.0));
        let Answer::Boolean {
            value, probability, ..
        } = got["angry"]
        else {
            panic!()
        };
        assert!(!value);
        assert!(close(probability, 0.05));
    }

    #[test]
    fn self_reports_below_uniform_are_counted_by_question() {
        // はい/いいえは 0.5 未満、選択肢（3 件）は 1/3 未満、段階（3 件）も 1/3 未満を数える。ちょうど一様は数えない。
        let req = request();
        let raised = raised_below_uniform(&req, &[vec![r(1, 0.2), r(2, 0.9), r(1, 0.3)]]);
        assert_eq!(raised, ["category", "angry"]);
        assert!(
            raised_below_uniform(&request(), &[vec![r(0, 1.0 / 3.0), r(0, 0.5), r(1, 0.5)]])
                .is_empty()
        );
        // 多数決は得票率を使うので数えない。
        let votes = vec![vec![r(1, 0.1), r(0, 0.1), r(1, 0.1)]; 3];
        assert!(raised_below_uniform(&request(), &votes).is_empty());
    }

    #[test]
    fn votes_become_probabilities_and_ties_go_to_the_more_confident() {
        let samples = vec![
            vec![r(1, 0.6), r(0, 0.5), r(1, 0.9)],
            vec![r(1, 0.7), r(1, 0.5), r(0, 0.9)],
            vec![r(0, 0.99), r(1, 0.5), r(1, 0.9)],
            vec![r(1, 0.6), r(2, 0.5), r(0, 0.9)],
            vec![r(0, 0.9), r(2, 0.9), r(1, 0.9)],
        ];
        let got = aggregate(&request(), &samples);
        let Answer::Choice {
            value,
            confidence,
            probabilities,
        } = &got["category"]
        else {
            panic!()
        };
        assert_eq!(value, "bug");
        assert!(close(*confidence, 0.6));
        assert!(close(probabilities["billing"], 0.4) && close(probabilities["other"], 0.0));
        // score は 中 2 票・高 2 票・低 1 票の同数 → 自己申告の合計が大きい「高」。
        let Answer::Score {
            value,
            level_index,
            confidence,
            ..
        } = got["priority"]
        else {
            panic!()
        };
        assert_eq!(level_index, 2);
        assert!(close(confidence, 0.4));
        assert!(close(value, 0.4 + 2.0 * 0.4));
        let Answer::Boolean {
            value,
            probability,
            confidence,
        } = got["angry"]
        else {
            panic!()
        };
        assert!(value);
        assert!(close(probability, 0.6) && close(confidence, 0.6));
    }

    #[test]
    fn a_full_tie_goes_to_the_earlier_value() {
        let samples = vec![
            vec![r(0, 0.5), r(0, 0.5), r(0, 0.5)],
            vec![r(1, 0.5), r(1, 0.5), r(1, 0.5)],
        ];
        let got = aggregate(&request(), &samples);
        assert!(matches!(&got["category"], Answer::Choice { value, .. } if value == "billing"));
        assert!(matches!(got["angry"], Answer::Boolean { value: false, .. }));
    }
}

//! 採点: 呼び出しの結果から、データごとの指標・θ の案・合格基準を組む。保存した結果の答えを今のデータで
//! 採点し直す段（`DECISION_EVAL_STAGE=rescore`）もここ（実 LLM を呼ばない）。

use crate::data::{self, Dataset};
use crate::metrics::{self, RepMetrics};
use crate::report::{self, DatasetResult, ResultFile};
use crate::run::CallResult;

/// 1 回の反復の指標。
fn rep_metrics(ds: &Dataset, calls: &[&CallResult]) -> RepMetrics {
    if ds.name == "expense_policy" {
        RepMetrics::Expense(metrics::expense(ds, calls))
    } else {
        RepMetrics::Inquiry(metrics::inquiry(ds, calls))
    }
}

/// データ 1 つの結果（反復ごとの指標・一致率・θ の案か合格基準）。
///
/// `theta` は凍結したしきい値（合格基準に照らす。`None` なら照らさない）、`suggest` は θ の案を出すか（dev の段）。
pub(crate) fn dataset_result(
    ds: &Dataset,
    calls: &[CallResult],
    reps: u32,
    theta: Option<f64>,
    suggest: bool,
) -> DatasetResult {
    let mine: Vec<&CallResult> = calls.iter().filter(|c| c.dataset == ds.name).collect();
    let per_rep: Vec<RepMetrics> = (1..=reps)
        .map(|r| {
            let of_rep: Vec<&CallResult> = mine.iter().copied().filter(|c| c.rep == r).collect();
            rep_metrics(ds, &of_rep)
        })
        .collect();
    let agreement =
        (reps > 1).then(|| metrics::agreement(&mine, metrics::primary_key(ds.name), reps));
    let rows = mine.iter().filter(|c| c.rep == 1).count();
    DatasetResult {
        name: ds.name.to_string(),
        questions_sha256: ds.questions_sha256.clone(),
        rows,
        suggested_theta: if suggest {
            per_rep.first().and_then(metrics::suggest_theta)
        } else {
            None
        },
        acceptance: theta.map(|t| metrics::acceptance(&per_rep, agreement, t)),
        reps: per_rep,
        agreement,
    }
}

/// 保存した結果（`results/<from>.json`）の答えを、今のデータと今の指標の計算で採点し直し、
/// `results/<from>-rescored.json` と summary.md に書く（実 LLM は呼ばない・裁定の後のラベルの直しを反映する）。
///
/// 質問が結果と違えば止める（答えは別の質問へのもの）。凍結した段なら、結果に残した凍結のしきい値で照らす。
/// 採点し直した結果はさらに採点し直さない（元の結果を指す）。
pub(crate) fn rescore(from: &str) -> ResultFile {
    rescore_file(report::read_result(from), from)
}

/// [`rescore`] の本体（読んだ結果を採点し直す・ファイルは読み書きしない）。
pub(crate) fn rescore_file(mut file: ResultFile, from: &str) -> ResultFile {
    assert!(
        file.rescored_from.is_none(),
        "{from} は採点し直した結果（元の結果を指す）"
    );
    let datasets: Vec<Dataset> = file
        .datasets
        .iter()
        .map(|d| {
            let ds = data::load_named(&d.name);
            assert_eq!(
                ds.questions_sha256, d.questions_sha256,
                "{}: 質問が {from} のときと違う（答えは別の質問へのもの）",
                d.name
            );
            ds
        })
        .collect();
    let mismatch =
        data::version_mismatch(&data::resolved_models(&file.results), file.frozen.as_ref());
    file.datasets = datasets
        .iter()
        .map(|ds| {
            let theta = file
                .frozen
                .as_ref()
                .filter(|_| mismatch.is_none())
                .and_then(|f| f.theta.get(ds.name).copied());
            dataset_result(ds, &file.results, file.reps, theta, file.frozen.is_none())
        })
        .collect();
    if let Some(m) = mismatch {
        file.stopped.get_or_insert(m);
    }
    file.datasets_sha256 = datasets
        .iter()
        .map(|ds| (ds.name.to_string(), ds.rows_sha256.clone()))
        .collect();
    file.rescored_from = Some(from.to_string());
    file.label = format!("{from}-rescored");
    file
}

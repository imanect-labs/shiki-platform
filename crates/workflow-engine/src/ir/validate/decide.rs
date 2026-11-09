//! 判定のノード（`ai.decide`・FEAT-1）の保存時検証 V3（モデルの照合）と、保存を止めない警告。
//!
//! - V3: `model` を組織が許可した判定用のモデル（[`Catalog::decision_models`]）へ照合する。文章を生成する
//!   モデル（保存する人のアカウントでは使えないものも）を書いたら `ir.model_modality_mismatch`、組織では使えるが
//!   保存する人のアカウントでは使えない判定用のモデルは `ir.model_unavailable_for_user`（D-169）、どれでもなければ
//!   `ir.unknown_model`（使える判定用のモデルを添える）。逆に llm.invoke・agent.invoke に判定用のモデルを書いたものは `v3_vocab::check_model`
//!   が `ir.model_modality_mismatch` にする。照合の材料が無い構成（`None`）は照合しない（実行時に
//!   gateway が fail-closed で拒否する）。
//! - 警告: しきい値が低い（[`DECIDE_LOW_CONFIDENCE`]）・値で振り分ける出口が多い（[`DECIDE_MANY_PORTS`]）・
//!   版の固定されていないモデル名（[`DECIDE_UNPINNED_MODEL`]）。

use super::{Catalog, ValidationError};
use crate::ir::params::{AiDecideParams, DecideQuestionType, DecideRouteBy};
use crate::ir::WorkflowIr;
use crate::vocab::NodeType;

/// 警告のコード: しきい値（`min_confidence`）が [`LOW_CONFIDENCE`] 未満。
pub const DECIDE_LOW_CONFIDENCE: &str = "ir.decide_low_confidence";
/// 警告のコード: 値で振り分ける選択肢が [`MANY_PORTS`] 件以上（出口が多すぎる）。
pub const DECIDE_MANY_PORTS: &str = "ir.decide_many_ports";
/// 警告のコード: モデルの名前が版を固定していない（`-latest`・`-preview`）。
pub const DECIDE_UNPINNED_MODEL: &str = "ir.decide_unpinned_model";

/// これ未満のしきい値は警告する（較正されていない確からしさでは、低いしきい値の自動の振り分けが誤りやすい）。
const LOW_CONFIDENCE: f64 = 0.6;
/// 値で振り分ける出口がこれ以上なら警告する。
const MANY_PORTS: usize = 10;
/// 判定のモデルの欄の位置。
const MODEL_PATH: &str = "/params/model";

/// 判定のノード。
fn decide_nodes(ir: &WorkflowIr) -> impl Iterator<Item = (&str, AiDecideParams)> {
    ir.nodes
        .iter()
        .filter(|n| n.node_type == NodeType::AiDecide.as_str())
        .filter_map(|n| {
            // 契約に合わないものは V1b が報告する。
            let p: AiDecideParams = serde_json::from_value(n.params.clone()).ok()?;
            Some((n.id.as_str(), p))
        })
}

/// 使える判定用のモデルの一覧（上限まで）。0 件のとき、保存する人の制限で 0 件になったなら、組織ではなくその人の
/// アカウントで使えないと言う（組織には使える判定用のモデルがある・D-169）。
fn candidates(catalog: &Catalog, models: &[String]) -> String {
    if models.is_empty() {
        return if catalog.user_restricted_decision_models.is_empty() {
            "組織で使える判定用のモデルがありません".to_string()
        } else {
            "あなたのアカウントで使える判定用のモデルがありません。管理者に相談してください"
                .to_string()
        };
    }
    let shown = models
        .iter()
        .take(super::v3_vocab::MAX_MODEL_CANDIDATES)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    match models
        .len()
        .checked_sub(super::v3_vocab::MAX_MODEL_CANDIDATES)
    {
        Some(rest) if rest > 0 => format!("使える判定用のモデル: {shown} ほか {rest} 件"),
        _ => format!("使える判定用のモデル: {shown}"),
    }
}

/// V3: 判定のモデルの照合。
pub(super) fn v3(ir: &WorkflowIr, catalog: &Catalog, errors: &mut Vec<ValidationError>) {
    let Some(allowed) = catalog.decision_models.as_ref() else {
        return;
    };
    for (id, p) in decide_nodes(ir) {
        let model = p.model.as_str();
        // 空（空白だけを含む）は保存を止めず、実行と自動実行のオンの前に止める（readiness と同じ読み方・D-83）。
        if model.trim().is_empty() || allowed.iter().any(|m| m == model) {
            continue;
        }
        let listed = |list: &[String]| list.iter().any(|m| m == model);
        // 文章を生成するモデルは、保存する人に使えるかに関わらず種類の違いを言う（組織で使える chat モデル全体で見る）。
        let e = if catalog.models.as_deref().is_some_and(listed)
            || listed(&catalog.user_restricted_models)
        {
            ValidationError::new(
                "ir.model_modality_mismatch",
                format!(
                    "{model} は文章を生成するモデルです。判定（ai.decide）には判定用のモデルを選んでください（{}）",
                    candidates(catalog, allowed)
                ),
            )
        } else if listed(&catalog.user_restricted_decision_models) {
            // 組織では使えるが、保存する人のアカウントでは使えない（D-169）。判定には組織の選択の禁止を掛けない。
            let fix = if allowed.is_empty() {
                "あなたのアカウントで使える判定用のモデルがないため、管理者に相談してください"
                    .to_string()
            } else {
                format!(
                    "一覧から選び直してください（{}）",
                    candidates(catalog, allowed)
                )
            };
            ValidationError::new(
                super::v3_vocab::MODEL_UNAVAILABLE_FOR_USER,
                format!("この判定用のモデルはあなたのアカウントでは使えません: {model}。{fix}"),
            )
        } else {
            ValidationError::new(
                "ir.unknown_model",
                format!(
                    "この判定用のモデルは使えません: {model}（{}）",
                    candidates(catalog, allowed)
                ),
            )
        };
        errors.push(e.at_node(id).at_path(MODEL_PATH));
    }
}

/// 保存を止めない警告。
pub(super) fn warnings(ir: &WorkflowIr) -> Vec<ValidationError> {
    let mut out = Vec::new();
    for (id, p) in decide_nodes(ir) {
        if p.model.contains("-latest") || p.model.contains("-preview") {
            out.push(
                ValidationError::new(
                    DECIDE_UNPINNED_MODEL,
                    format!(
                        "判定用のモデル {} は版が固定されていません。版が変わると確からしさの傾向も変わるので、\
                         しきい値を測り直してください",
                        p.model
                    ),
                )
                .at_node(id)
                .at_path(MODEL_PATH),
            );
        }
        let Some(route) = &p.route else {
            continue;
        };
        if let Some(min) = route
            .min_confidence
            .filter(|c| c.is_finite() && *c < LOW_CONFIDENCE)
        {
            out.push(
                ValidationError::new(
                    DECIDE_LOW_CONFIDENCE,
                    format!(
                        "しきい値 {min} は低めです。確からしさは較正されていないことがあり、自動の振り分けが誤りやすくなります",
                    ),
                )
                .at_node(id)
                .at_path("/params/route/min_confidence"),
            );
        }
        let options = p
            .question(&route.question)
            .filter(|q| q.kind == DecideQuestionType::Choice)
            .and_then(|q| q.options.as_ref())
            .map_or(0, Vec::len);
        if route.by() == DecideRouteBy::Value && options >= MANY_PORTS {
            out.push(
                ValidationError::new(
                    DECIDE_MANY_PORTS,
                    format!(
                        "値で振り分けると出口が {options} 個になります。確からしさで分けて（by: confidence）、\
                         後ろの振り分けで /answers/{}/value を使う方が読みやすくなります",
                        route.question
                    ),
                )
                .at_node(id)
                .at_path("/params/route"),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests;

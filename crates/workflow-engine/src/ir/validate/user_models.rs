//! 有効化する人のアカウントの制限でモデルを照らす（利用者ごとのプロバイダの制限・D-196）。
//!
//! 保存のときは、保存する人の制限でモデルを照らす（D-169・D-185）。schedule・event の実行はワークフローの主体で
//! 動き、制限がかからないので、制限の無い人が保存した版や、この照合より前に保存した版を、制限された人が有効化すると、
//! 有効化した人には使えないプロバイダで自動実行される。有効化のときにも、保存のときと同じ照合を、有効化する人の
//! 材料（[`Catalog`]）でかける。
//!
//! - 照らすのは schedule・event のきっかけを持つ IR だけ（対話の実行は起動した本人の文脈で動き、生成の入口で
//!   制限がかかる）。
//! - llm.invoke・agent.invoke は保存のときと同じ `v3_vocab::check_model`、ai.decide は `decide::v3` を流し、
//!   [`MODEL_UNAVAILABLE_FOR_USER`] だけを残す（組織で使えないモデル・選択の禁止・種類の違いは実行のときに断られる）。
//! - skill.invoke は、組織の既定のモデルが有効化する人に使えないとき、有効化する人が入れた shiki script のスキル
//!   （[`Catalog::script_skills`]）でなければ止める。有効化する人が入れていないスキルも止める（中身を確かめられない・
//!   fail-closed）。
//!
//! [`super::validate`] を流してコードで絞る形にはしない（上限を超えた IR や、入れていないスキルの誤りが先に返ると、
//! 制限の照合まで届かない）。コードと位置（`/params/model`・`/params/skill`・ノード id）は保存のときと同じで、文には
//! 有効化の文脈の 1 文（[`ENABLE_HINT`]）を添える。

use super::{refs, Catalog, ValidationError, MODEL_UNAVAILABLE_FOR_USER};
use crate::ir::{Node, WorkflowIr};
use crate::vocab::NodeType;

/// モデルの欄の位置（保存のときと同じ）。
const MODEL_PATH: &str = "/params/model";
/// スキルの欄の位置（保存のときと同じ）。
const SKILL_PATH: &str = "/params/skill";
/// 有効化の文脈で、どの誤りの文にも添える文。
const ENABLE_HINT: &str = "自動実行は、このモデルを使える人にオンにしてもらうこともできます";

/// 有効化する人のアカウントでは使えないモデルで自動実行される IR の、有効化を止める理由（ブロックの順・空なら止めない）。
///
/// `catalog` は有効化する人の材料（保存のときと同じ組み方・API 層が詰める）。制限の無い材料（
/// [`Catalog::user_restricted_models`]・[`Catalog::user_restricted_decision_models`] が空で、
/// [`Catalog::default_model_unavailable`] が `None`）と、schedule・event のきっかけを持たない IR では空。
#[must_use]
pub fn user_model_blockers(ir: &WorkflowIr, catalog: &Catalog) -> Vec<ValidationError> {
    if !super::readiness::runs_unattended(ir) {
        return Vec::new();
    }
    let mut decided = Vec::new();
    super::decide::v3(ir, catalog, &mut decided);
    let mut found = Vec::new();
    for node in &ir.nodes {
        match NodeType::parse(&node.node_type) {
            Some(NodeType::LlmInvoke | NodeType::AgentInvoke) => {
                found.extend(
                    super::v3_vocab::check_model(&node.params, catalog)
                        .map(|e| e.at_node(&node.id).at_path(MODEL_PATH)),
                );
            }
            Some(NodeType::AiDecide) => found.extend(
                decided
                    .iter()
                    .filter(|e| e.node_id.as_deref() == Some(node.id.as_str()))
                    .cloned(),
            ),
            Some(NodeType::SkillInvoke) => found.extend(skill_blocker(node, catalog)),
            _ => {}
        }
    }
    found
        .into_iter()
        .filter(|e| e.code == MODEL_UNAVAILABLE_FOR_USER)
        .map(for_enabling)
        .collect()
}

/// skill.invoke が、有効化する人に使えない組織の既定のモデルで動くか（D-185 の保存の照合を有効化する人で）。
///
/// 空・形の誤った参照は見ない（空は必須の欄として有効化の前に止まり、形の誤りは実行のときに断られる）。
fn skill_blocker(node: &Node, catalog: &Catalog) -> Option<ValidationError> {
    let model = catalog.default_model_unavailable.as_deref()?;
    let (name, version) = node
        .params
        .get("skill")
        .and_then(serde_json::Value::as_str)
        .and_then(refs::parse_skill_ref)?;
    if catalog.script_skills.contains(&format!("{name}@{version}")) {
        return None;
    }
    let installed = catalog
        .skills
        .get(name)
        .is_some_and(|versions| versions.contains(version));
    let e = if installed {
        refs::skill_default_unavailable(model, catalog)
    } else {
        refs::skill_default_unchecked(name, version, model)
    };
    Some(e.at_node(&node.id).at_path(SKILL_PATH))
}

/// 有効化の文脈の 1 文を添える。
fn for_enabling(mut e: ValidationError) -> ValidationError {
    if !e.message.ends_with('。') {
        e.message.push('。');
    }
    e.message.push_str(ENABLE_HINT);
    e
}

#[cfg(test)]
#[path = "user_models_tests.rs"]
mod tests;

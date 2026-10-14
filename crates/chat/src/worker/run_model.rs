//! run のモデル指定（チャットで選んだモデル・思考強度・#553）と skill のモデル既定の合成。
//!
//! 投入時に API が組織の設定で解決・検証した値が run に載っている。実行時に
//! [`llm_gateway::LlmGateway::resolve_at_run`] で**実行時点の設定**に照らし直す
//! （キュー待ちや GUI カードへの回答で続く run の間に管理者が設定を変えることがある）:
//! 1. ユーザーがモデルを**選んだ** run（`model_auto = false`）はそのモデルと思考強度で生成する。
//!    skill のピンより優先する（利用者の明示的な選択を黙って差し替えない）。もう使えなければ既定へ。
//! 2. Auto の run は skill がモデルを指定していて、それが今の設定で選べるモデルならそれを使う
//!    （Auto＝「任せる」なので skill 作者の指定が勝つ。Auto の思考強度は別モデル向けなので載せない）。
//!    無効化されたモデル・選択禁止の組織では skill のピンを使わない。
//! 3. それ以外は今の設定での Auto の振り先。
//! 4. run にモデルが無い（このカラム以前の run）は従来どおり（skill → 設定 → 既定）。
//!    無効化されたモデルは gateway が拒否する。

use authz::AuthContext;
use llm_gateway::{ChoiceError, Effort, LlmGateway, ResolvedChoice};

use crate::error::ChatError;
use crate::store::ClaimedRun;

/// 合成結果（`model` が `None` なら gateway の既定モデル）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EffectiveModel {
    pub model: Option<String>,
    pub effort: Option<Effort>,
}

/// run の指定・skill の既定・従来の既定（設定）からモデルと思考強度を決める。
pub(super) async fn effective_model(
    gateway: &LlmGateway,
    ctx: &AuthContext,
    run: &ClaimedRun,
    skill_model: Option<&str>,
    fallback: Option<&str>,
) -> Result<EffectiveModel, ChatError> {
    let Some(model) = run.model.as_deref() else {
        return Ok(EffectiveModel {
            model: skill_model.or(fallback).map(str::to_string),
            effort: None,
        });
    };
    let snapshot = ResolvedChoice {
        model: model.to_string(),
        effort: run.effort.as_deref().and_then(Effort::parse),
        auto: run.model_auto,
    };
    let resolved = gateway
        .resolve_at_run(ctx, &snapshot, skill_model)
        .await
        .map_err(|e| ChatError::Unavailable(format!("llm settings: {e}")))?
        .map_err(|e: ChoiceError| ChatError::Invalid(e.to_string()))?;
    Ok(EffectiveModel {
        model: Some(resolved.model),
        effort: resolved.effort,
    })
}

/// 自律・ツールループの実行オプションへ適用する（skill の既定を適用した**後**に呼ぶ）。
///
/// skill の既定（`apply_model_defaults`）は温度・上限も触るので、モデルと思考強度だけを
/// ここで決め直す。`opts.model` には skill の指定か設定の既定が入っている。
pub(super) async fn apply_to_opts(
    gateway: &LlmGateway,
    ctx: &AuthContext,
    run: &ClaimedRun,
    skills: &[crate::skill::AppliedSkill],
    opts: &mut agent_core::AgentOptions,
) -> Result<(), ChatError> {
    let skill_model = crate::skill::combined_model_defaults(skills).and_then(|d| d.model);
    let chosen = effective_model(
        gateway,
        ctx,
        run,
        skill_model.as_deref(),
        opts.model.as_deref(),
    )
    .await?;
    opts.model = chosen.model;
    opts.effort = chosen.effort;
    Ok(())
}

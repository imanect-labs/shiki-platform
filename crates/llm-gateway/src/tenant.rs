//! テナントの AI 設定（モデルカタログの絞り込み・既定・Auto）と、チャットのモデル指定の解決。
//!
//! モデルとプロバイダの実体（接続先・API キー・単価）はデプロイ設定のカタログが正で、テナント
//! 管理者は「その中から何を使わせるか」だけを決める（requirements §FR: 管理者が絞るモデル
//! カタログの範囲でユーザーが選ぶ・思考強度は 3 段階）。
//!
//! 解決（[`resolve_choice`]）は純粋関数にして、API（投入時の検証・400）とテストが同じ規則を使う。

use authz::AuthContext;
use sqlx::PgPool;

use crate::config::{AutoConfig, ModelCatalog, ModelEntry};
use crate::model::Effort;

/// 「Auto」を表すモデル指定。
pub const AUTO_MODEL: &str = "auto";

/// テナントの AI 設定（`tenant_llm_settings` ＋ `tenant_llm_model`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantLlmSettings {
    /// 新しいチャットの既定（[`AUTO_MODEL`] かモデル ID）。
    pub default_model: String,
    /// ユーザーがチャットでモデルを選べるか。
    pub allow_user_choice: bool,
    /// ユーザーがモデルを選んだときの思考強度の初期値。
    pub default_effort: Effort,
    /// Auto の振り先の上書き（`None` はデプロイ設定）。
    pub auto_model: Option<String>,
    pub auto_effort: Option<Effort>,
    /// 円換算レート（1 USD あたりの円 × 100）。`None` は円を出さない。
    pub jpy_per_usd_centi: Option<i32>,
    /// テナントが無効にしたモデル ID。
    pub disabled_models: Vec<String>,
}

impl Default for TenantLlmSettings {
    fn default() -> Self {
        TenantLlmSettings {
            default_model: AUTO_MODEL.to_string(),
            allow_user_choice: true,
            default_effort: Effort::Medium,
            auto_model: None,
            auto_effort: None,
            jpy_per_usd_centi: None,
            disabled_models: Vec::new(),
        }
    }
}

impl TenantLlmSettings {
    /// テナントで使えるモデルか（カタログにあり、無効化されていない）。
    pub fn is_enabled(&self, catalog: &ModelCatalog, model: &str) -> bool {
        catalog.get(model).is_some() && !self.disabled_models.iter().any(|d| d == model)
    }

    /// テナントで使えるモデル（カタログ順）。
    pub fn enabled_models<'a>(&self, catalog: &'a ModelCatalog) -> Vec<&'a ModelEntry> {
        catalog
            .models
            .iter()
            .filter(|m| !self.disabled_models.contains(&m.id))
            .collect()
    }
}

/// チャットのモデル指定（リクエスト）。`model` が `None` はテナントの既定に従う。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelChoice {
    pub model: Option<String>,
    pub effort: Option<Effort>,
}

/// 解決済みのモデル指定（run にスナップショットする）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedChoice {
    /// 論理モデル名（カタログ内）。
    pub model: String,
    /// 送る思考強度（モデルが対応しなければ `None`）。
    pub effort: Option<Effort>,
    /// Auto（ユーザーが選ばなかった）か。Auto のときだけ skill のピンがモデルを上書きできる。
    pub auto: bool,
}

/// 解決の失敗（API は 400 に写す）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChoiceError {
    /// カタログに無い／テナントで無効なモデル。
    #[error("このモデルは使えません: {0}")]
    ModelNotAllowed(String),
    /// テナントがモデルの選択を許していない。
    #[error("この組織ではモデルを選べません")]
    ChoiceDisabled,
    /// 使えるモデルが 1 つも無い（カタログが空・全部無効）。
    #[error("使えるモデルがありません")]
    NoModel,
}

/// Auto の振り先を決める。テナントの上書き → デプロイ設定 → 既定モデル → 有効な先頭の順で、
/// **使えるモデル**に当たったところで止める（Auto が「使えない」状態を作らない）。
pub fn auto_target(
    catalog: &ModelCatalog,
    auto: &AutoConfig,
    settings: &TenantLlmSettings,
) -> Result<(String, Effort), ChoiceError> {
    let effort = settings.auto_effort.unwrap_or(auto.effort);
    let candidates = [
        settings.auto_model.as_deref(),
        Some(auto.model.as_str()),
        Some(catalog.default_model.as_str()),
    ];
    if let Some(m) = candidates
        .into_iter()
        .flatten()
        .find(|m| settings.is_enabled(catalog, m))
    {
        return Ok((m.to_string(), effort));
    }
    settings
        .enabled_models(catalog)
        .first()
        .map(|m| (m.id.clone(), effort))
        .ok_or(ChoiceError::NoModel)
}

/// モデル指定を解決する（純粋関数）。
///
/// - `model` 省略はテナントの既定（`auto` かモデル ID）。
/// - `auto` は [`auto_target`]。思考強度はユーザー指定を使わず Auto の設定に従う。
/// - モデル ID はテナントで有効なものだけ。選択が禁止されていれば既定と同じモデル以外は拒否。
/// - 思考強度に対応しないモデルは `effort = None`（gateway も落とすが、run の記録を正直にする）。
pub fn resolve_choice(
    catalog: &ModelCatalog,
    auto: &AutoConfig,
    settings: &TenantLlmSettings,
    supports_effort: impl Fn(&ModelEntry) -> bool,
    choice: &ModelChoice,
) -> Result<ResolvedChoice, ChoiceError> {
    let requested = choice
        .model
        .as_deref()
        .filter(|m| !m.is_empty())
        .unwrap_or(settings.default_model.as_str());
    let (model, effort, is_auto) = if requested == AUTO_MODEL {
        let (m, e) = auto_target(catalog, auto, settings)?;
        (m, e, true)
    } else {
        if !settings.allow_user_choice && requested != settings.default_model {
            return Err(ChoiceError::ChoiceDisabled);
        }
        if !settings.is_enabled(catalog, requested) {
            return Err(ChoiceError::ModelNotAllowed(requested.to_string()));
        }
        (
            requested.to_string(),
            choice.effort.unwrap_or(settings.default_effort),
            false,
        )
    };
    let effort = catalog
        .get(&model)
        .filter(|e| supports_effort(e))
        .map(|_| effort);
    Ok(ResolvedChoice {
        model,
        effort,
        auto: is_auto,
    })
}

/// 実行時点の設定で run のモデルを決め直す（純粋関数・チャットのワーカーが使う）。
///
/// 投入時に解決した `snapshot` は、実行までの間（キュー待ち・GUI カードへの回答で続く run）に
/// 管理者が設定を変えると古くなる。ここで照らし直し、使えなくなっていれば既定へ戻す。
/// - ユーザーが選んだモデル: 今も有効で、選択が許されている（または既定と同じ）なら維持。
/// - Auto: skill のピンが今の設定で選べるモデルならそれ（思考強度は載せない）、
///   なければ今の設定での Auto の振り先。skill のピンもユーザーの選択と同じ扱いにする
///   （選択禁止・無効化を skill 経由ですり抜けさせない）。
pub fn resolve_at_run(
    catalog: &ModelCatalog,
    auto: &AutoConfig,
    settings: &TenantLlmSettings,
    supports_effort: impl Fn(&ModelEntry) -> bool,
    snapshot: &ResolvedChoice,
    skill_model: Option<&str>,
) -> Result<ResolvedChoice, ChoiceError> {
    let user_may_use = |m: &str| {
        settings.is_enabled(catalog, m)
            && (settings.allow_user_choice || m == settings.default_model)
    };
    if !snapshot.auto {
        if user_may_use(&snapshot.model) {
            return Ok(snapshot.clone());
        }
        return resolve_choice(
            catalog,
            auto,
            settings,
            supports_effort,
            &ModelChoice::default(),
        );
    }
    if let Some(m) = skill_model.filter(|m| user_may_use(m)) {
        return Ok(ResolvedChoice {
            model: m.to_string(),
            effort: None,
            auto: true,
        });
    }
    let choice = ModelChoice {
        model: Some(AUTO_MODEL.to_string()),
        effort: None,
    };
    resolve_choice(catalog, auto, settings, supports_effort, &choice)
}

/// 設定の更新内容（管理画面から）。`None` の項目は変えない。
#[derive(Debug, Clone, Default)]
pub struct SettingsPatch {
    pub default_model: Option<String>,
    pub allow_user_choice: Option<bool>,
    pub default_effort: Option<Effort>,
    /// `Some(None)` で上書きを外す（デプロイ設定へ戻す）。
    pub auto_model: Option<Option<String>>,
    pub auto_effort: Option<Option<Effort>>,
    pub jpy_per_usd_centi: Option<Option<i32>>,
    /// モデルの有効/無効（モデル ID → 有効か）。
    pub models: Vec<(String, bool)>,
}

/// 設定更新の検証エラー（API は 400 に写す）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SettingsError {
    #[error("カタログに無いモデルです: {0}")]
    UnknownModel(String),
    #[error("既定のモデルが無効になっています: {0}")]
    DefaultDisabled(String),
    #[error("少なくとも 1 つのモデルを有効にしてください")]
    NoModelEnabled,
    #[error("円換算レートは 1〜1000 円の範囲で指定してください")]
    InvalidRate,
}

/// 更新後の設定を作って検証する（純粋関数）。保存は [`TenantLlmStore::save`]。
pub fn apply_patch(
    catalog: &ModelCatalog,
    current: &TenantLlmSettings,
    patch: &SettingsPatch,
) -> Result<TenantLlmSettings, SettingsError> {
    let mut next = current.clone();
    for (id, enabled) in &patch.models {
        if catalog.get(id).is_none() {
            return Err(SettingsError::UnknownModel(id.clone()));
        }
        next.disabled_models.retain(|d| d != id);
        if !enabled {
            next.disabled_models.push(id.clone());
        }
    }
    next.disabled_models.sort();
    if let Some(m) = &patch.default_model {
        next.default_model.clone_from(m);
    }
    if let Some(v) = patch.allow_user_choice {
        next.allow_user_choice = v;
    }
    if let Some(e) = patch.default_effort {
        next.default_effort = e;
    }
    if let Some(m) = &patch.auto_model {
        next.auto_model.clone_from(m);
    }
    if let Some(e) = patch.auto_effort {
        next.auto_effort = e;
    }
    if let Some(r) = patch.jpy_per_usd_centi {
        next.jpy_per_usd_centi = r;
    }

    if next.enabled_models(catalog).is_empty() {
        return Err(SettingsError::NoModelEnabled);
    }
    if next.default_model != AUTO_MODEL {
        if catalog.get(&next.default_model).is_none() {
            return Err(SettingsError::UnknownModel(next.default_model));
        }
        if !next.is_enabled(catalog, &next.default_model) {
            return Err(SettingsError::DefaultDisabled(next.default_model));
        }
    }
    if let Some(m) = &next.auto_model {
        if catalog.get(m).is_none() {
            return Err(SettingsError::UnknownModel(m.clone()));
        }
    }
    if next
        .jpy_per_usd_centi
        .is_some_and(|r| !(100..=100_000).contains(&r))
    {
        return Err(SettingsError::InvalidRate);
    }
    Ok(next)
}

/// `tenant_llm_settings` / `tenant_llm_model` の読み書き（粒度は tenant_id ＋ org）。
#[derive(Clone)]
pub struct TenantLlmStore {
    db: PgPool,
}

type SettingsRow = (
    Option<String>,
    Option<bool>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i32>,
    Vec<String>,
);

/// 設定と無効モデルを 1 往復で読む（行が無ければ設定列は NULL）。
const LOAD_SQL: &str =
    "SELECT s.default_model, s.allow_user_choice, s.default_effort, s.auto_model, \
            s.auto_effort, s.jpy_per_usd_centi, \
            ARRAY(SELECT m.model_id FROM tenant_llm_model m \
                  WHERE m.tenant_id = $1 AND m.org = $2 AND NOT m.enabled ORDER BY m.model_id) \
     FROM (SELECT 1) AS one \
     LEFT JOIN tenant_llm_settings s ON s.tenant_id = $1 AND s.org = $2";

fn settings_from_row(row: SettingsRow) -> TenantLlmSettings {
    let (default_model, allow, default_effort, auto_model, auto_effort, rate, disabled_models) =
        row;
    let base = TenantLlmSettings::default();
    TenantLlmSettings {
        default_model: default_model.unwrap_or(base.default_model),
        allow_user_choice: allow.unwrap_or(base.allow_user_choice),
        default_effort: default_effort
            .as_deref()
            .and_then(Effort::parse)
            .unwrap_or(base.default_effort),
        auto_model,
        auto_effort: auto_effort.as_deref().and_then(Effort::parse),
        jpy_per_usd_centi: rate,
        disabled_models,
    }
}

impl TenantLlmStore {
    pub fn new(db: PgPool) -> Self {
        TenantLlmStore { db }
    }

    /// 組織の設定を読む（行が無ければ既定）。
    pub async fn load(&self, ctx: &AuthContext) -> Result<TenantLlmSettings, sqlx::Error> {
        let row: SettingsRow = sqlx::query_as(LOAD_SQL)
            .bind(&ctx.tenant_id)
            .bind(&ctx.org)
            .fetch_one(&self.db)
            .await?;
        Ok(settings_from_row(row))
    }

    /// 組織がこのモデルを無効にしているか（gateway が生成のたびに引く・1 往復）。
    pub async fn is_disabled(&self, ctx: &AuthContext, model: &str) -> Result<bool, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM tenant_llm_model \
             WHERE tenant_id = $1 AND org = $2 AND model_id = $3 AND NOT enabled)",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(model)
        .fetch_one(&self.db)
        .await
    }

    /// 読む → `change` で次の設定を作る → 保存する、を 1 トランザクションで行う。
    ///
    /// 組織ごとの advisory lock で直列化する。読みと保存の間に別の管理者の更新が挟まると、
    /// 片方の変更が消えたり、検証済みの不変条件（既定モデルが有効、など）が崩れたりするため。
    pub async fn update<E>(
        &self,
        ctx: &AuthContext,
        changed_models: &[(String, bool)],
        change: impl FnOnce(&TenantLlmSettings) -> Result<TenantLlmSettings, E>,
    ) -> Result<Result<TenantLlmSettings, E>, sqlx::Error> {
        let mut tx = self.db.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 553))")
            .bind(format!("tenant_llm_settings:{}|{}", ctx.tenant_id, ctx.org))
            .execute(&mut *tx)
            .await?;
        let row: SettingsRow = sqlx::query_as(LOAD_SQL)
            .bind(&ctx.tenant_id)
            .bind(&ctx.org)
            .fetch_one(&mut *tx)
            .await?;
        let next = match change(&settings_from_row(row)) {
            Ok(n) => n,
            Err(e) => return Ok(Err(e)),
        };
        sqlx::query(
            "INSERT INTO tenant_llm_settings \
             (tenant_id, org, default_model, allow_user_choice, default_effort, auto_model, \
              auto_effort, jpy_per_usd_centi, updated_by, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, now()) \
             ON CONFLICT (tenant_id, org) DO UPDATE SET \
               default_model = EXCLUDED.default_model, \
               allow_user_choice = EXCLUDED.allow_user_choice, \
               default_effort = EXCLUDED.default_effort, \
               auto_model = EXCLUDED.auto_model, \
               auto_effort = EXCLUDED.auto_effort, \
               jpy_per_usd_centi = EXCLUDED.jpy_per_usd_centi, \
               updated_by = EXCLUDED.updated_by, \
               updated_at = now()",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(&next.default_model)
        .bind(next.allow_user_choice)
        .bind(next.default_effort.as_str())
        .bind(next.auto_model.as_deref())
        .bind(next.auto_effort.map(Effort::as_str))
        .bind(next.jpy_per_usd_centi)
        .bind(&ctx.principal.id)
        .execute(&mut *tx)
        .await?;
        for (model, enabled) in changed_models {
            sqlx::query(
                "INSERT INTO tenant_llm_model \
                 (tenant_id, org, model_id, enabled, updated_by, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, now()) \
                 ON CONFLICT (tenant_id, org, model_id) DO UPDATE SET \
                   enabled = EXCLUDED.enabled, updated_by = EXCLUDED.updated_by, updated_at = now()",
            )
            .bind(&ctx.tenant_id)
            .bind(&ctx.org)
            .bind(model)
            .bind(*enabled)
            .bind(&ctx.principal.id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(Ok(next))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> ModelCatalog {
        let m = |id: &str| ModelEntry {
            id: id.into(),
            ..Default::default()
        };
        ModelCatalog {
            default_model: "sonnet".into(),
            models: vec![m("sonnet"), m("opus"), m("gpt-6-luna"), m("local")],
        }
    }

    fn effort_except_local(e: &ModelEntry) -> bool {
        e.id != "local"
    }

    fn resolve(
        settings: &TenantLlmSettings,
        model: Option<&str>,
        effort: Option<Effort>,
    ) -> Result<ResolvedChoice, ChoiceError> {
        resolve_choice(
            &catalog(),
            &AutoConfig::default(),
            settings,
            effort_except_local,
            &ModelChoice {
                model: model.map(str::to_string),
                effort,
            },
        )
    }

    #[test]
    fn default_is_auto_with_gpt6_luna_low() {
        let r = resolve(&TenantLlmSettings::default(), None, Some(Effort::High)).unwrap();
        assert_eq!(r.model, "gpt-6-luna");
        // Auto ではユーザーの思考強度を使わない。
        assert_eq!(r.effort, Some(Effort::Low));
        assert!(r.auto);
    }

    #[test]
    fn auto_falls_back_when_target_is_disabled_or_missing() {
        let s = TenantLlmSettings {
            disabled_models: vec!["gpt-6-luna".into()],
            ..Default::default()
        };
        assert_eq!(resolve(&s, Some(AUTO_MODEL), None).unwrap().model, "sonnet");
        let s = TenantLlmSettings {
            disabled_models: vec!["gpt-6-luna".into(), "sonnet".into()],
            ..Default::default()
        };
        assert_eq!(resolve(&s, Some(AUTO_MODEL), None).unwrap().model, "opus");
        let s = TenantLlmSettings {
            auto_model: Some("opus".into()),
            auto_effort: Some(Effort::High),
            ..Default::default()
        };
        let r = resolve(&s, None, None).unwrap();
        assert_eq!((r.model.as_str(), r.effort), ("opus", Some(Effort::High)));
    }

    #[test]
    fn explicit_model_uses_requested_or_default_effort() {
        let s = TenantLlmSettings::default();
        let r = resolve(&s, Some("opus"), Some(Effort::High)).unwrap();
        assert_eq!(
            (r.model.as_str(), r.effort, r.auto),
            ("opus", Some(Effort::High), false)
        );
        let r = resolve(&s, Some("opus"), None).unwrap();
        assert_eq!(r.effort, Some(Effort::Medium));
        // 思考強度に対応しないモデルは記録しない。
        assert_eq!(
            resolve(&s, Some("local"), Some(Effort::High))
                .unwrap()
                .effort,
            None
        );
    }

    #[test]
    fn disabled_unknown_and_forbidden_choices_are_rejected() {
        let s = TenantLlmSettings {
            disabled_models: vec!["opus".into()],
            ..Default::default()
        };
        assert_eq!(
            resolve(&s, Some("opus"), None),
            Err(ChoiceError::ModelNotAllowed("opus".into()))
        );
        assert_eq!(
            resolve(&s, Some("ghost"), None),
            Err(ChoiceError::ModelNotAllowed("ghost".into()))
        );
        let s = TenantLlmSettings {
            allow_user_choice: false,
            ..Default::default()
        };
        assert_eq!(
            resolve(&s, Some("opus"), None),
            Err(ChoiceError::ChoiceDisabled)
        );
        // 選択禁止でも Auto（＝既定）は使える。
        assert!(resolve(&s, Some(AUTO_MODEL), None).is_ok());
    }

    #[test]
    fn tenant_default_model_applies_when_unspecified() {
        let s = TenantLlmSettings {
            default_model: "opus".into(),
            allow_user_choice: false,
            ..Default::default()
        };
        let r = resolve(&s, None, None).unwrap();
        assert_eq!((r.model.as_str(), r.auto), ("opus", false));
    }

    fn at_run(
        settings: &TenantLlmSettings,
        snapshot: (&str, bool),
        skill: Option<&str>,
    ) -> ResolvedChoice {
        let snapshot = ResolvedChoice {
            model: snapshot.0.into(),
            effort: Some(Effort::High),
            auto: snapshot.1,
        };
        resolve_at_run(
            &catalog(),
            &AutoConfig::default(),
            settings,
            effort_except_local,
            &snapshot,
            skill,
        )
        .unwrap()
    }

    #[test]
    fn run_keeps_user_choice_while_still_allowed() {
        let r = at_run(
            &TenantLlmSettings::default(),
            ("opus", false),
            Some("sonnet"),
        );
        assert_eq!((r.model.as_str(), r.effort), ("opus", Some(Effort::High)));
    }

    #[test]
    fn run_drops_user_choice_disabled_or_forbidden_after_post() {
        let disabled = TenantLlmSettings {
            disabled_models: vec!["opus".into()],
            ..Default::default()
        };
        let r = at_run(&disabled, ("opus", false), None);
        assert_eq!((r.model.as_str(), r.auto), ("gpt-6-luna", true));
        let forbidden = TenantLlmSettings {
            allow_user_choice: false,
            ..Default::default()
        };
        assert_eq!(
            at_run(&forbidden, ("opus", false), None).model,
            "gpt-6-luna"
        );
    }

    #[test]
    fn skill_pin_obeys_disabled_models_and_choice_policy() {
        let s = TenantLlmSettings::default();
        let r = at_run(&s, ("gpt-6-luna", true), Some("opus"));
        assert_eq!((r.model.as_str(), r.effort), ("opus", None));
        let disabled = TenantLlmSettings {
            disabled_models: vec!["opus".into()],
            ..Default::default()
        };
        let r = at_run(&disabled, ("gpt-6-luna", true), Some("opus"));
        assert_eq!(
            (r.model.as_str(), r.effort),
            ("gpt-6-luna", Some(Effort::Low))
        );
        let forbidden = TenantLlmSettings {
            allow_user_choice: false,
            ..Default::default()
        };
        assert_eq!(
            at_run(&forbidden, ("gpt-6-luna", true), Some("opus")).model,
            "gpt-6-luna"
        );
        // Auto の振り先が投入後に無効化されたら、今の設定で振り直す。
        let auto_off = TenantLlmSettings {
            disabled_models: vec!["gpt-6-luna".into()],
            ..Default::default()
        };
        assert_eq!(
            at_run(&auto_off, ("gpt-6-luna", true), None).model,
            "sonnet"
        );
    }

    #[test]
    fn patch_validates_catalog_default_and_rate() {
        let c = catalog();
        let base = TenantLlmSettings::default();
        let disable_all = SettingsPatch {
            models: c.models.iter().map(|m| (m.id.clone(), false)).collect(),
            ..Default::default()
        };
        assert_eq!(
            apply_patch(&c, &base, &disable_all),
            Err(SettingsError::NoModelEnabled)
        );
        let bad_default = SettingsPatch {
            default_model: Some("opus".into()),
            models: vec![("opus".into(), false)],
            ..Default::default()
        };
        assert_eq!(
            apply_patch(&c, &base, &bad_default),
            Err(SettingsError::DefaultDisabled("opus".into()))
        );
        let unknown = SettingsPatch {
            models: vec![("ghost".into(), true)],
            ..Default::default()
        };
        assert_eq!(
            apply_patch(&c, &base, &unknown),
            Err(SettingsError::UnknownModel("ghost".into()))
        );
        let rate = SettingsPatch {
            jpy_per_usd_centi: Some(Some(50)),
            ..Default::default()
        };
        assert_eq!(
            apply_patch(&c, &base, &rate),
            Err(SettingsError::InvalidRate)
        );

        let ok = SettingsPatch {
            models: vec![
                ("local".into(), false),
                ("opus".into(), false),
                ("opus".into(), true),
            ],
            jpy_per_usd_centi: Some(Some(15_000)),
            auto_model: Some(Some("sonnet".into())),
            ..Default::default()
        };
        let next = apply_patch(&c, &base, &ok).unwrap();
        assert_eq!(next.disabled_models, vec!["local".to_string()]);
        assert_eq!(next.jpy_per_usd_centi, Some(15_000));
        assert_eq!(next.auto_model.as_deref(), Some("sonnet"));
    }
}

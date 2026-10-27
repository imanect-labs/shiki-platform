//! TypeSafe System One（決定モデル Jev）互換の HTTP アダプタ（[`DecisionProvider`] の 2 つ目の実装・FEAT-1 の T9）。
//!
//! **実験的・既定は無効**: 設定でプロバイダ種別 `typesafe_systemone` を書いたときだけ組まれ、判定用のモデルは
//! `requires_opt_in = true`（組織の管理者が有効にするまで使えない）と `processed_abroad = true`（米国で処理）を
//! 書かないと起動が止まる（構築時の検査は gateway/decide.rs）。API キーが無く、実 API では確かめていない。形は公開
//! 文書（docs.typesafe.ai の HTTP API・Models・Confidence の頁、2026-10-27 に読んだもの）から作り、HTTP の
//! モックでだけ確かめた（tests/decision_typesafe_it.rs）。分からなかった点は「推測」と書く。
//!
//! - `POST {base_url}/v1/systemone`（既定の base_url は `https://api.typesafe.ai`）・`Authorization: Bearer <キー>`。
//! - 送る形: `{model, state, questions: {<キー>: {type, instructions, criteria}}}`。選択肢（choice）は
//!   `criteria = {選択肢のキー: 説明}`、段階（score）は `criteria = [段階の名前…]`（低い順・位置が値）、はい/いいえは
//!   `type = "noul"` で `criteria = {"true": はいの説明, "false": いいえの説明}`（書いたものだけ）。
//! - 受ける形: `{model, answers: {<キー>: {type, …}}, usage: {input_tokens, output_tokens}}`。choice は
//!   `{choice, confidence, probabilities: {キー: 確率}}`、score は `{score（確率で重み付けした位置）, confidence,
//!   probabilities: {"0": 確率, …}}`、noul は `{noul（はいの確率）}`（確からしさは無い）。
//! - **確からしさは shiki の意味にそろえる**: 選んだ答えの確率（choice・score は選んだ値の確率、はい/いいえは
//!   max(p, 1-p)）。TypeSafe の `confidence`（分布の偏りの統計・均等なら 0）は使わない。決定器を替えても
//!   しきい値の意味が変わらないように（しきい値自体は版ごとに測り直す・PIT-80）。
//! - **較正は主張しない**（`calibrated = false`）: 公開文書は confidence を「分布の要約の 1 つ」とし、自分のデータで
//!   しきい値を試すよう書いている（較正の主張は見当たらない）。確率そのものは決定器が出す（`method = native`）。
//! - **版ガード**: 応答の `model` は答えた版（別名を送っても版の ID が返る）。設定の実 ID（版を固定したもの・
//!   `jev-1.13.0` など）と同じか、その直後が区切り（`-`・`+`・`@`）でなければ使わない（費用は会計に載せる）。
//!   `jev-1.1` に固定して `jev-1.13.0` が答えたものは別の版として扱う。
//! - 誤り: 429 は利用者に本文を出さない定型文（`RateLimited`）、401/403 は設定の誤り（`Config`・キー）、そのほかの
//!   4xx は `BadRequest`、5xx・タイムアウト（最初の応答 10 秒・全体 30 秒）・切断は `Unavailable`。本文はログにだけ
//!   残す（gateway の呼び出し側が利用者向けの文に写す）。

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::timeouts::RequestClock;
use crate::config::{LlmTimeouts, ModelEntry, ProviderConfig};
use crate::decision::{
    invalid_response, Answer, DecideFailure, DecideRequest, DecideResponse, DecisionMethod,
    DecisionProvider, QuestionKind,
};
use crate::model::{GenerateRequest, Usage};
use crate::provider::{rate_limit_message, DeltaStream, LlmError, LlmProvider};

/// 既定の接続先（公開文書の HTTP API の頁）。
pub const DEFAULT_BASE_URL: &str = "https://api.typesafe.ai";

/// 決定器の名前（ログ・管理画面）。
const NAME: &str = "typesafe";

/// 最初の応答・全体のタイムアウト（判定は 70〜500ms とされる。長く待たない）。
const TIMEOUTS: LlmTimeouts = LlmTimeouts {
    first_response: Duration::from_secs(10),
    idle: Duration::from_secs(30),
    total: Duration::from_secs(30),
};

/// TypeSafe System One 互換の決定器。
pub struct TypesafeDecider {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    real_id: String,
}

impl TypesafeDecider {
    /// `real_id` は版を固定した実 ID（`jev-1.13.0` など・構築時に別名でないことを確かめる）。
    #[must_use]
    pub fn new(
        http: reqwest::Client,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        real_id: impl Into<String>,
    ) -> Self {
        TypesafeDecider {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            real_id: real_id.into(),
        }
    }

    /// 決定器の版（設定の実 ID・決定キャッシュのキーに入る）。
    #[must_use]
    pub fn version(&self) -> &str {
        &self.real_id
    }

    /// 送る本文。
    fn body(&self, req: &DecideRequest) -> Value {
        let questions: Map<String, Value> = req
            .questions
            .iter()
            .map(|q| {
                let (kind, criteria) = match &q.kind {
                    QuestionKind::Choice { options } => (
                        "choice",
                        Value::Object(
                            options
                                .iter()
                                .map(|o| {
                                    // 説明の無い選択肢はキーそのものを説明にする（空の説明で答えさせない）。
                                    let d = if o.description.trim().is_empty() {
                                        o.key.clone()
                                    } else {
                                        o.description.clone()
                                    };
                                    (o.key.clone(), Value::String(d))
                                })
                                .collect(),
                        ),
                    ),
                    QuestionKind::Score { levels } => ("score", json!(levels)),
                    QuestionKind::Boolean { yes, no } => {
                        let mut c = Map::new();
                        if let Some(y) = yes.as_deref().filter(|s| !s.trim().is_empty()) {
                            c.insert("true".into(), Value::String(y.to_string()));
                        }
                        if let Some(n) = no.as_deref().filter(|s| !s.trim().is_empty()) {
                            c.insert("false".into(), Value::String(n.to_string()));
                        }
                        (
                            "noul",
                            if c.is_empty() {
                                Value::Null
                            } else {
                                Value::Object(c)
                            },
                        )
                    }
                };
                let mut obj = Map::new();
                obj.insert("type".into(), Value::String(kind.into()));
                obj.insert("instructions".into(), Value::String(q.instructions.clone()));
                if !criteria.is_null() {
                    obj.insert("criteria".into(), criteria);
                }
                (q.key.clone(), Value::Object(obj))
            })
            .collect();
        json!({ "model": self.real_id, "state": req.state, "questions": questions })
    }
}

/// 設定から決定器を組む（構築時の検査・誤りは起動を止める理由）。
///
/// 判定用のモデルは、`decision_via`・`decision`（llm_decider の項目）を書かない、`processed_abroad = true`
/// （米国で処理）、`requires_opt_in = true`（組織の管理者が有効にするまで使わせない＝既定は無効）、実 ID は版を
/// 固定したもの（`-latest`・`-preview` の別名は版ガードと決定キャッシュの版が合わない）。API キーは必須。
pub(crate) fn from_config(
    http: &reqwest::Client,
    entry: &ModelEntry,
    pc: &ProviderConfig,
) -> Result<TypesafeDecider, String> {
    if entry.decision_via.is_some() || entry.decision.is_some() {
        return Err(
            "typesafe_systemone answers natively (decision_via / decision are for llm_decider)"
                .into(),
        );
    }
    if !entry.processed_abroad {
        return Err("typesafe_systemone is processed abroad (set processed_abroad = true)".into());
    }
    if !entry.requires_opt_in {
        return Err(
            "typesafe_systemone is experimental and opt-in (set requires_opt_in = true)".into(),
        );
    }
    let real_id = entry.resolved_id();
    if real_id.contains("latest") || real_id.contains("preview") {
        return Err(format!(
            "pin a versioned real_id such as jev-1.13.0 (not the alias {real_id})"
        ));
    }
    let api_key = pc
        .api_key
        .clone()
        .filter(|k| !k.is_empty())
        .ok_or("typesafe_systemone requires api_key")?;
    let base_url = pc.base_url.as_deref().unwrap_or(DEFAULT_BASE_URL);
    Ok(TypesafeDecider::new(
        http.clone(),
        base_url,
        api_key,
        real_id,
    ))
}

/// 判定専用のプロバイダの生成の窓口（文章は生成しない）。chat モデルは構築時に振れないので呼ばれないが、
/// 呼ばれたら設定の誤りとして拒む（黙って別のプロバイダへ流さない）。
pub(crate) struct DecisionOnlyProvider;

#[async_trait::async_trait]
impl LlmProvider for DecisionOnlyProvider {
    fn name(&self) -> &'static str {
        "typesafe_systemone"
    }

    async fn stream(&self, _req: &GenerateRequest) -> Result<DeltaStream, LlmError> {
        Err(LlmError::Config(
            "typesafe_systemone serves decision models only".into(),
        ))
    }
}

/// 応答（読むのは使う項目だけ・ベンダーが項目を足しても壊れない）。
#[derive(Debug, Deserialize)]
struct WireResponse {
    model: String,
    answers: BTreeMap<String, WireAnswer>,
}

/// 答え 1 件（`type` で分かれる）。
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireAnswer {
    Noul {
        noul: f64,
    },
    Choice {
        choice: String,
        probabilities: BTreeMap<String, f64>,
    },
    Score {
        score: f64,
        probabilities: BTreeMap<String, f64>,
    },
}

/// 使用量（課金は入力だけ・出力は今は無料とされる）。本文の形が崩れていても読めるだけ読む（会計に載せる）。
fn usage_of(body: &Value) -> Usage {
    let tokens = |k: &str| {
        body.pointer(&format!("/usage/{k}"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    Usage {
        prompt_tokens: tokens("input_tokens"),
        completion_tokens: tokens("output_tokens"),
        ..Default::default()
    }
}

fn unit(x: f64) -> bool {
    x.is_finite() && (0.0..=1.0).contains(&x)
}

/// 応答の答えを中立形へ写す（質問の順に・合わなければ理由）。
fn to_answers(
    req: &DecideRequest,
    mut wire: BTreeMap<String, WireAnswer>,
) -> Result<BTreeMap<String, Answer>, String> {
    let mut out = BTreeMap::new();
    for q in &req.questions {
        let a = wire
            .remove(&q.key)
            .ok_or_else(|| format!("{}: 答えが無い", q.key))?;
        out.insert(q.key.clone(), to_answer(&q.key, &q.kind, a)?);
    }
    if let Some(extra) = wire.keys().next() {
        return Err(format!("質問に無い答え {extra}"));
    }
    Ok(out)
}

fn to_answer(key: &str, kind: &QuestionKind, a: WireAnswer) -> Result<Answer, String> {
    match (kind, a) {
        (QuestionKind::Boolean { .. }, WireAnswer::Noul { noul }) => {
            if !unit(noul) {
                return Err(format!("{key}: はいの確率が範囲の外"));
            }
            Ok(Answer::Boolean {
                value: noul >= 0.5,
                probability: noul,
                confidence: noul.max(1.0 - noul),
            })
        }
        (
            QuestionKind::Choice { options },
            WireAnswer::Choice {
                choice,
                probabilities,
            },
        ) => {
            if let Some(k) = probabilities
                .keys()
                .find(|k| !options.iter().any(|o| &o.key == *k))
            {
                return Err(format!("{key}: 選択肢に無いキーの確率 {k}"));
            }
            if !options.iter().any(|o| o.key == choice) {
                return Err(format!("{key}: 選択肢に無い答え {choice}"));
            }
            // 確率の無い選択肢は 0 とみなす（推測: 文書の例はすべての選択肢を返している）。
            let probabilities: BTreeMap<String, f64> = options
                .iter()
                .map(|o| {
                    (
                        o.key.clone(),
                        probabilities.get(&o.key).copied().unwrap_or(0.0),
                    )
                })
                .collect();
            if !probabilities.values().all(|p| unit(*p)) {
                return Err(format!("{key}: 確率が範囲の外"));
            }
            Ok(Answer::Choice {
                confidence: probabilities[&choice],
                value: choice,
                probabilities,
            })
        }
        (
            QuestionKind::Score { levels },
            WireAnswer::Score {
                score,
                probabilities,
            },
        ) => {
            let mut probs = vec![0.0; levels.len()];
            for (k, p) in probabilities {
                let i: usize = k
                    .parse()
                    .ok()
                    .filter(|i| *i < levels.len())
                    .ok_or_else(|| format!("{key}: 段階に無いキーの確率 {k}"))?;
                if !unit(p) {
                    return Err(format!("{key}: 確率が範囲の外"));
                }
                probs[i] = p;
            }
            #[allow(clippy::cast_precision_loss)] // 段階は 10 件まで
            let top = levels.len().saturating_sub(1) as f64;
            if !score.is_finite() || !(0.0..=top).contains(&score) {
                return Err(format!("{key}: 段階の位置が範囲の外"));
            }
            // 選んだ段階は確率が最も高いもの（同じなら重み付けの位置に近い方・それも同じなら低い方）。
            let level_index = probs
                .iter()
                .enumerate()
                .max_by(|(i, p), (j, q)| {
                    #[allow(clippy::cast_precision_loss)]
                    let (di, dj) = ((*i as f64 - score).abs(), (*j as f64 - score).abs());
                    p.total_cmp(q).then(dj.total_cmp(&di)).then(j.cmp(i))
                })
                .map_or(0, |(i, _)| i);
            Ok(Answer::Score {
                value: score,
                level_index,
                confidence: probs[level_index],
                probabilities: probs,
            })
        }
        _ => Err(format!("{key}: 答えの種類が質問と合わない")),
    }
}

/// 応答の `model` が設定の版か（同じか、版の直後が区切り `-`・`+`・`@` のもの）。接頭辞だけで比べると、
/// `jev-1.1` に固定した設定が `jev-1.13.0` の答えを受け入れてしまう（しきい値を測った版と違う版で振り分ける）。
pub(crate) fn same_version(real_id: &str, answered: &str) -> bool {
    answered
        .strip_prefix(real_id)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(['-', '+', '@']))
}

#[async_trait::async_trait]
impl DecisionProvider for TypesafeDecider {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn decide(&self, req: &DecideRequest) -> Result<DecideResponse, DecideFailure> {
        let fail = |e: LlmError| DecideFailure::new(e, Usage::default());
        let url = format!("{}/v1/systemone", self.base_url);
        let builder = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&self.body(req));
        let clock = RequestClock::start(TIMEOUTS, NAME);
        let resp = clock.send(builder).await.map_err(fail)?;
        let status = resp.status();
        if !status.is_success() {
            let text = clock.error_text(resp).await.map_err(fail)?;
            let error = match status.as_u16() {
                429 => LlmError::RateLimited(rate_limit_message(&text)),
                401 | 403 => {
                    tracing::warn!(%status, body = %text, "TypeSafe が認証を拒んだ");
                    LlmError::Config(format!("typesafe {status}: authentication failed"))
                }
                _ if status.is_client_error() => {
                    LlmError::BadRequest(format!("typesafe {status}: {text}"))
                }
                _ => LlmError::Unavailable(format!("typesafe {status}: {text}")),
            };
            return Err(fail(error));
        }
        let text = clock.body_text(resp).await.map_err(fail)?;
        let raw: Value = serde_json::from_str(&text).map_err(|e| {
            fail(invalid_response(format!(
                "typesafe: 応答が JSON でない: {e}"
            )))
        })?;
        // ここから先の失敗は、課金された分（使用量）を会計に載せる。
        let usage = usage_of(&raw);
        // 応答が要求と合わない（形の崩れ・版の不一致・値域の外）。送り直しても直らないので再試行させない。
        let bad = |why: String| DecideFailure::new(invalid_response(why), usage);
        let wire: WireResponse = serde_json::from_value(raw)
            .map_err(|e| bad(format!("typesafe: 応答の形が違う: {e}")))?;
        if !same_version(&self.real_id, &wire.model) {
            return Err(bad(format!(
                "typesafe: 設定の版 {} ではなく {} が答えた",
                self.real_id, wire.model
            )));
        }
        let answers =
            to_answers(req, wire.answers).map_err(|why| bad(format!("typesafe: {why}")))?;
        Ok(DecideResponse {
            resolved_model: wire.model,
            answers,
            usage,
            calibrated: false,
            method: DecisionMethod::Native,
            samples: 1,
        })
    }
}

#[cfg(test)]
mod tests;

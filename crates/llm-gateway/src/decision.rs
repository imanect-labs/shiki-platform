//! 決定モデル（FEAT-1・D-6）の中立形と差し替えトレイト [`DecisionProvider`]。
//!
//! 設計の正: docs/design.md §3.1（差し替えトレイト）・§4.5（決定モデルとチョークポイント）。
//!
//! 決定モデルは文章を生成しない。質問ごとに、決められた選択肢（choice）・段階（score）・はい/いいえ
//! （boolean）のどれかで答え、確からしさを付けて返す。生成とは入出力の形が違うので、[`LlmProvider`] とは
//! 別のトレイトにする（非生成モデルを別トレイトにした `EmbeddingProvider`・`Reranker` と同じ）。
//!
//! - 実装は構築時にカタログから組む: LLM に決定を真似させる `llm_decider`
//!   （[`crate::providers::llm_decider`]）と、テスト用の決まった規則で答える stub
//!   （[`crate::providers::stub_decider`]）。
//! - 呼ぶのは必ずチョークポイント [`LlmGateway::decide`] から。組織の許可・利用上限・会計はそこで行う。
//!   決定器は失敗しても、それまでに使ったトークン量を [`DecideFailure`] で返す（会計に載せる・PIT-28）。
//! - 確からしさは、決定器が較正を主張しない限り `calibrated = false`（LLM の自己申告・得票率は
//!   較正されていない・PIT-80）。
//!
//! [`LlmProvider`]: crate::provider::LlmProvider
//! [`LlmGateway::decide`]: crate::gateway::LlmGateway::decide

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::gateway::UsageSource;
use crate::model::Usage;
use crate::provider::LlmError;

/// 決定キャッシュ（ワークフローの ai.decide の step 属性 `cache`・T10）。
pub mod cache;
/// 要求と応答の検査（上限・突き合わせ）。
pub mod check;

/// 判定の要求（中立形）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecideRequest {
    /// 決定モデルの論理名（カタログの `modality = "decision"` のエントリ）。
    pub model: String,
    /// 判定の材料（文字列・オブジェクト・配列）。**信頼しないデータ**として扱う（中の指示に従わない）。
    pub state: serde_json::Value,
    /// 質問（順序を保つ）。ワークフローの作者が書いたもので、材料とは分けて渡す。
    pub questions: Vec<Question>,
}

/// 質問 1 件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Question {
    /// 答えのキー（要求の中で一意）。
    pub key: String,
    /// 何を判定するか（自然文）。
    pub instructions: String,
    #[serde(flatten)]
    pub kind: QuestionKind,
}

/// 質問の種類と答えの値域。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum QuestionKind {
    /// 選択肢から 1 つ。
    Choice { options: Vec<ChoiceOption> },
    /// 段階から 1 つ（`levels` は低い順）。
    Score { levels: Vec<String> },
    /// はい/いいえ（`yes`・`no` はそれぞれに当たる場合の説明）。
    Boolean {
        #[serde(default)]
        yes: Option<String>,
        #[serde(default)]
        no: Option<String>,
    },
}

/// 選択肢 1 件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChoiceOption {
    /// 答えの値（質問の中で一意）。
    pub key: String,
    /// どんなときにこの選択肢か。
    #[serde(default)]
    pub description: String,
}

/// 判定の結果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecideResponse {
    /// 実際に答えた決定器の版（例 `claude-haiku-5-5+llm_decider/v3/k1/low`）。版が変わったら
    /// しきい値を測り直す（PIT-80）。
    pub resolved_model: String,
    /// 質問のキー → 答え（要求の質問と同じキーの集合）。
    pub answers: BTreeMap<String, Answer>,
    /// この判定で使ったトークン量（多数決・直させた分を含む合計）。
    pub usage: Usage,
    /// 確からしさが較正されているか（LLM の自己申告・得票率は false）。
    pub calibrated: bool,
    pub method: DecisionMethod,
    /// 1 回の判定で答えさせた回数。
    pub samples: u8,
    /// AI が自分の答えに一様（1/n・はい/いいえなら 0.5）より低い自信を申告し、一様まで引き上げた質問のキー
    /// （質問の順・重ならない・BUG-124・D-114）。取り違えや迷いの合図で、作る人が実行の詳細で見る。
    ///
    /// 引き上げるのは LLM の自己申告（1 回・llm_decider）だけで、ほかの決定器（stub・TypeSafe）と多数決（得票率を
    /// 使う）は空を返す。この項目の無い決定キャッシュの古い答えは空として読む。
    #[serde(default)]
    pub raised_questions: Vec<String>,
}

/// 確からしさの出し方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionMethod {
    /// 決定器そのものが出す確率（決定モデルの API・stub）。
    Native,
    /// LLM の自己申告（1 回）。
    Verbalized,
    /// 複数回答えさせた得票率。
    Vote,
}

/// 質問 1 件への答え。確率・確からしさはどれも 0〜1。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Answer {
    /// `value` は選択肢のキー。`probabilities` は選択肢のキー → 確率。
    Choice {
        value: String,
        confidence: f64,
        probabilities: BTreeMap<String, f64>,
    },
    /// `level_index` は選んだ段階（低い順の添字）、`value` は確率で重み付けした平均の位置
    /// （0〜段階数-1）、`probabilities` は段階ごとの確率（`levels` と同じ順）。
    Score {
        value: f64,
        level_index: usize,
        confidence: f64,
        probabilities: Vec<f64>,
    },
    /// `probability` は「はい」の確率、`confidence` は max(p, 1-p)。
    Boolean {
        value: bool,
        probability: f64,
        confidence: f64,
    },
}

impl Answer {
    /// 答えの確からしさ（しきい値と比べる値）。
    #[must_use]
    pub const fn confidence(&self) -> f64 {
        match self {
            Answer::Choice { confidence, .. }
            | Answer::Score { confidence, .. }
            | Answer::Boolean { confidence, .. } => *confidence,
        }
    }
}

/// 判定の失敗。失敗するまでに使ったトークン量も返す（会計に載せる・PIT-28）。
#[derive(Debug)]
pub struct DecideFailure {
    pub error: LlmError,
    pub usage: Usage,
}

impl DecideFailure {
    #[must_use]
    pub const fn new(error: LlmError, usage: Usage) -> Self {
        DecideFailure { error, usage }
    }
}

/// 決定器（差し替えトレイト）。設定で差し替える（llm_decider・stub・将来の決定モデルの API）。
///
/// 組織の許可・利用上限・会計はチョークポイント（[`LlmGateway::decide`]）が持ち、ここは判定だけを担う。
///
/// [`LlmGateway::decide`]: crate::gateway::LlmGateway::decide
#[async_trait::async_trait]
pub trait DecisionProvider: Send + Sync {
    /// 決定器の名前（`llm_decider` / `stub` …・ログと出力の `provider` に使う）。
    fn name(&self) -> &'static str;

    /// 判定する。要求は [`check::check_request`] を通したもの。
    async fn decide(&self, req: &DecideRequest) -> Result<DecideResponse, DecideFailure>;
}

/// 判定 1 回分の会計・計装の手がかり（呼び出し側が渡す）。
#[derive(Debug, Clone)]
pub struct DecideCall {
    /// 会計の冪等キー（ワークフローは step の冪等キー）。成功した判定だけをこのキーで記録し、
    /// 失敗した判定は `{key}#f{uuid}` で記録する（後の成功が重複除去で消えないように）。
    pub idempotency_key: String,
    pub trace_id: Option<String>,
    pub source: UsageSource,
    /// 呼び出し元ミニアプリ（ワークフローは `None`）。
    pub app_id: Option<uuid::Uuid>,
}

/// 判定の失敗の種類（呼び出し側が利用者向けの失敗へ写すのに使う）。
///
/// [`LlmError`] の列挙は変えず（差し替えトレイトの型・D-63 の前例）、定型文の頭で表す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecideErrorKind {
    /// 要求が上限・形の決まりに合わない（直さない限り通らない）。
    InvalidRequest,
    /// モデルが判定を拒否した（安全分類器など）。直させない。
    Refused,
    /// 応答が上限のトークン数で途切れた。直させない。
    Truncated,
    /// 応答が決められた形に合わなかった（1 回直させても合わなかった）。
    InvalidOutput,
    /// 決定器の応答が要求と合わない（答えの過不足・値域の外・版の不一致・応答の形の崩れ）。決定器か設定の誤りで、
    /// 同じ要求を送り直しても直らない。原因はログ用で、利用者には出さない。
    InvalidResponse,
}

/// 要求の誤り（[`DecideErrorKind::InvalidRequest`]）の頭。
pub(crate) const INVALID_REQUEST_HEAD: &str = "判定の要求が決まりに合いません: ";
/// 拒否（[`DecideErrorKind::Refused`]）。
pub(crate) const REFUSED: &str =
    "判定モデルが判定を拒否しました。判定材料や質問の書き方を見直してください。";
/// 途切れ（[`DecideErrorKind::Truncated`]）。
pub(crate) const TRUNCATED: &str =
    "判定モデルの応答が上限の長さで途切れました。質問の数を減らしてください。";
/// 形に合わない（[`DecideErrorKind::InvalidOutput`]）の頭。
pub(crate) const INVALID_OUTPUT_HEAD: &str = "判定モデルの応答が決められた形に合いませんでした";
/// 決定器の応答が要求と合わない（[`DecideErrorKind::InvalidResponse`]・`LlmError::Internal`）の頭。続きは原因（ログ用）。
pub(crate) const INVALID_RESPONSE_HEAD: &str = "decision provider returned an invalid response: ";

/// 決定器の応答が要求と合わない誤り（[`DecideErrorKind::InvalidResponse`]）。`why` はログ用の原因。
///
/// 種類は `Internal` のまま（`Internal` を一律に再試行しないものにはしない。ストリームの読み取りの失敗など
/// 一時的なものも含むため）。頭の定型文で見分ける。
pub(crate) fn invalid_response(why: impl std::fmt::Display) -> LlmError {
    LlmError::Internal(format!("{INVALID_RESPONSE_HEAD}{why}"))
}

/// 判定の失敗の種類を見分ける（それ以外の失敗は `None`・[`LlmError`] の分類に従う）。
#[must_use]
pub fn decide_error_kind(err: &LlmError) -> Option<DecideErrorKind> {
    let m = match err {
        LlmError::BadRequest(m) => m,
        LlmError::Internal(m) if m.starts_with(INVALID_RESPONSE_HEAD) => {
            return Some(DecideErrorKind::InvalidResponse);
        }
        _ => return None,
    };
    if m.starts_with(INVALID_REQUEST_HEAD) {
        Some(DecideErrorKind::InvalidRequest)
    } else if m == REFUSED {
        Some(DecideErrorKind::Refused)
    } else if m == TRUNCATED {
        Some(DecideErrorKind::Truncated)
    } else if m.starts_with(INVALID_OUTPUT_HEAD) {
        Some(DecideErrorKind::InvalidOutput)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn questions_read_and_write_a_flat_shape() {
        let q: Question = serde_json::from_value(serde_json::json!({
            "key": "category",
            "instructions": "種類は",
            "type": "choice",
            "options": [{ "key": "bug", "description": "不具合" }]
        }))
        .unwrap();
        assert_eq!(
            q.kind,
            QuestionKind::Choice {
                options: vec![ChoiceOption {
                    key: "bug".into(),
                    description: "不具合".into()
                }]
            }
        );
        let b: Question = serde_json::from_value(serde_json::json!({
            "key": "angry", "instructions": "怒っているか", "type": "boolean"
        }))
        .unwrap();
        assert_eq!(
            b.kind,
            QuestionKind::Boolean {
                yes: None,
                no: None
            }
        );
        assert_eq!(serde_json::to_value(&b).unwrap()["type"], "boolean");
    }

    #[test]
    fn error_kinds_are_told_apart_by_their_fixed_text() {
        let kind = |e: LlmError| decide_error_kind(&e);
        assert_eq!(
            kind(LlmError::BadRequest(format!(
                "{INVALID_REQUEST_HEAD}質問は 20 件まで"
            ))),
            Some(DecideErrorKind::InvalidRequest)
        );
        assert_eq!(
            kind(LlmError::BadRequest(REFUSED.into())),
            Some(DecideErrorKind::Refused)
        );
        assert_eq!(
            kind(LlmError::BadRequest(TRUNCATED.into())),
            Some(DecideErrorKind::Truncated)
        );
        assert_eq!(
            kind(LlmError::BadRequest(format!("{INVALID_OUTPUT_HEAD}（x）"))),
            Some(DecideErrorKind::InvalidOutput)
        );
        // 決定器の応答の不整合は Internal の頭で見分ける（再試行しない）。
        assert_eq!(
            kind(invalid_response("答えのキーが質問と合わない")),
            Some(DecideErrorKind::InvalidResponse)
        );
        // 定型文でない誤り（プロバイダの 4xx・設定の誤り・一時的な内部の誤り）は従来の分類に任せる。
        assert_eq!(kind(LlmError::BadRequest("prompt too long".into())), None);
        assert_eq!(kind(LlmError::Config(REFUSED.into())), None);
        assert_eq!(kind(LlmError::Internal("stream read failed".into())), None);
        assert_eq!(
            kind(LlmError::BadRequest(format!("{INVALID_RESPONSE_HEAD}x"))),
            None,
            "頭は Internal のときだけ"
        );
    }

    /// 引き上げた質問（BUG-124・D-114）の無い応答（決定キャッシュに残った古い答え）は、空として読む。書くときは
    /// いつも載せる。
    #[test]
    fn responses_without_raised_questions_read_as_none() {
        let old = serde_json::json!({
            "resolved_model": "m",
            "answers": { "angry": { "type": "boolean", "value": true, "probability": 0.5, "confidence": 0.5 } },
            "usage": Usage::default(),
            "calibrated": false,
            "method": "verbalized",
            "samples": 1
        });
        let resp: DecideResponse = serde_json::from_value(old).unwrap();
        assert!(resp.raised_questions.is_empty());
        let raised = DecideResponse {
            raised_questions: vec!["angry".into()],
            ..resp
        };
        let back = serde_json::to_value(&raised).unwrap();
        assert_eq!(back["raised_questions"], serde_json::json!(["angry"]));
        assert_eq!(
            serde_json::from_value::<DecideResponse>(back).unwrap(),
            raised
        );
    }

    #[test]
    fn confidence_reads_every_kind() {
        let b = Answer::Boolean {
            value: true,
            probability: 0.9,
            confidence: 0.9,
        };
        assert!((b.confidence() - 0.9).abs() < f64::EPSILON);
        let s = Answer::Score {
            value: 1.0,
            level_index: 1,
            confidence: 0.7,
            probabilities: vec![0.1, 0.7, 0.2],
        };
        assert!((s.confidence() - 0.7).abs() < f64::EPSILON);
    }
}

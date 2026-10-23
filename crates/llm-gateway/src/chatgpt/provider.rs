//! ChatGPT プランで生成するアダプタ（Responses API を連携者のアクセストークンで呼ぶ）。
//!
//! 送る形は [`OpenAiResponsesProvider`] のプラン向けの形（`max_output_tokens` を送らない・
//! system を developer で送る）。呼び出し主体の照合は gateway が先に済ませる
//! （[`ChatGptAccount::ensure_owner`]）ので、ここはトークンを付けて送るだけ。

use std::sync::Arc;

use super::ChatGptAccount;
use crate::config::LlmTimeouts;
use crate::model::GenerateRequest;
use crate::provider::{DeltaStream, LlmError, LlmProvider};
use crate::providers::openai_responses::OpenAiResponsesProvider;

/// ChatGPT プランのアダプタ。
pub(crate) struct ChatGptPlanProvider {
    inner: OpenAiResponsesProvider,
    account: Arc<ChatGptAccount>,
}

impl ChatGptPlanProvider {
    pub(crate) fn new(
        http: reqwest::Client,
        account: Arc<ChatGptAccount>,
        default_model: String,
        timeouts: LlmTimeouts,
    ) -> Self {
        let inner = OpenAiResponsesProvider::new(http, account.api_base(), None, default_model)
            .for_chatgpt_plan()
            .with_timeouts(timeouts);
        ChatGptPlanProvider { inner, account }
    }

    /// プラン固有の失敗を、利用者が次に何をすればよいか分かる文言へ写す。
    async fn explain(&self, e: LlmError) -> LlmError {
        match e {
            LlmError::RateLimited(_) => LlmError::RateLimited(
                "ChatGPT プランの利用上限に達しました。ChatGPT の設定 → Usage で上限を確認してください。"
                    .into(),
            ),
            LlmError::BadRequest(m) if m.contains("subscription_sharing_user_not_eligible") => {
                LlmError::BadRequest(
                    "この ChatGPT アカウントはプランでの利用対象外です（Plus / Pro が必要です）。"
                        .into(),
                )
            }
            LlmError::BadRequest(m) if m.starts_with("responses 401") => {
                // 失効・取り消しの可能性。次の呼び出しでリフレッシュを試させる。
                tracing::warn!(error = %m, "ChatGPT の推論 API が 401 を返した");
                self.account.invalidate_access_token().await;
                LlmError::Unavailable(
                    "ChatGPT の認証が通りませんでした。続く場合は設定 → 接続済みアプリから再ログインしてください。"
                        .into(),
                )
            }
            other => other,
        }
    }
}

#[async_trait::async_trait]
impl LlmProvider for ChatGptPlanProvider {
    fn name(&self) -> &'static str {
        "chatgpt_plan"
    }

    async fn stream(&self, req: &GenerateRequest) -> Result<DeltaStream, LlmError> {
        let token = self.account.access_token().await?;
        match self.inner.stream_with_bearer(req, Some(&token)).await {
            Ok(s) => Ok(s),
            Err(e) => Err(self.explain(e).await),
        }
    }
}

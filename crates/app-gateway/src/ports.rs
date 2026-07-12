//! 重量級サブシステムへの narrow port（Task 9.8）。
//!
//! rag.query の委譲先は `rag::SearchService`（permission-aware・pre/post-filter 二段 authz）
//! だが、app-gateway が qdrant/tantivy/埋め込みまで依存するのは過剰なため、必要最小の
//! trait（[`RagPort`]）だけを定義し、実装は api 層の配線が `SearchService` を包んで提供する。
//! **可読性の担保はこの trait の実装側（SearchService の post-filter）が持つ**——ゲートウェイは
//! 呼出ユーザーの [`AuthContext`] をそのまま渡すだけで、検索結果に非可読文書は混入しない。

use async_trait::async_trait;
use authz::AuthContext;
use serde::Serialize;
use uuid::Uuid;

use crate::GatewayError;

/// permission-aware RAG 検索の 1 ヒット（ミニアプリへ返す最小 DTO）。
#[derive(Debug, Clone, Serialize)]
pub struct RagHit {
    pub chunk_id: Uuid,
    pub file_id: Uuid,
    pub file_name: String,
    pub page: Option<i32>,
    pub heading_path: Vec<String>,
    pub content: String,
    pub score: f32,
}

/// permission-aware RAG 検索の port（実装は api 配線の `SearchService` ラッパ）。
#[async_trait]
pub trait RagPort: Send + Sync {
    /// 呼出ユーザーの ReBAC で検索する（非可読文書は実装側 post-filter が落とす）。
    async fn query(
        &self,
        ctx: &AuthContext,
        query: &str,
        top_k: Option<u32>,
        trace_id: Option<&str>,
    ) -> Result<Vec<RagHit>, GatewayError>;
}

/// AI ストリーミングの 1 イベント（SSE の event/data に対応・Task 9.9）。
#[derive(Debug, Clone, Serialize)]
pub struct AiEvent {
    pub event: String,
    pub data: serde_json::Value,
}

/// AI イベントの非同期ストリーム（llm.invoke / agent.invoke 共通の SSE 源）。
pub type AiEventStream = futures::stream::BoxStream<'static, AiEvent>;

/// agent.invoke の入力（ガードレール確定済み・port 実装はこれを超えられない）。
#[derive(Debug, Clone)]
pub struct AgentInvokeSpec {
    pub app_id: uuid::Uuid,
    pub prompt: String,
    /// 論理モデル名（allowlist 検証済み・None はテナント既定）。
    pub model: Option<String>,
    /// インストール時ピンの宣言ツール（実装側で ToolName 閉集合 ∩ 実配線と交差する）。
    pub declared_tools: Vec<String>,
    /// 1 生成の出力トークン上限（インストール時ピン）。
    pub max_tokens: Option<i64>,
    pub max_steps: Option<usize>,
    /// この呼び出しで使ってよい累積コスト上限（日次残額・マイクロ USD）。
    pub max_cost_usd_micros: i64,
    pub trace_id: Option<String>,
}

/// agent.invoke の port（実装は api 配線＝agent-core run_agent ラッパ）。
///
/// **ツールと RAG は呼出ユーザーの ReBAC で絞る**（doc_search は ctx 経由の permission-aware
/// 検索・宣言外ツールは提示しない）。LLM 呼び出しは llm-gateway を通り app_id 付きで計上される。
#[async_trait]
pub trait AgentPort: Send + Sync {
    async fn invoke(
        &self,
        ctx: &AuthContext,
        spec: AgentInvokeSpec,
    ) -> Result<AiEventStream, GatewayError>;
}

/// agent 実行未構成時のフォールバック（agent.invoke は 502 を返す）。
pub struct NoAgent;

#[async_trait]
impl AgentPort for NoAgent {
    async fn invoke(
        &self,
        _ctx: &AuthContext,
        _spec: AgentInvokeSpec,
    ) -> Result<AiEventStream, GatewayError> {
        Err(GatewayError::Upstream(
            "エージェント実行がこの環境では構成されていません".into(),
        ))
    }
}

/// B2 関数起動の入力（二重ゲート通過済み・ユーザー起点）。
#[derive(Debug, Clone)]
pub struct FunctionInvokeSpec {
    pub app_id: uuid::Uuid,
    pub function: String,
    pub payload: serde_json::Value,
    /// 呼出ユーザーの gateway トークン（RFC 8693 exchange の subject・ゲストへは渡らない）。
    pub subject_token: String,
    /// インストール時ピン（server_bundle sha / ServerSpec JSON / B2 client id）。
    pub server_bundle: Option<String>,
    pub server_spec: Option<serde_json::Value>,
    pub client_id_b2: Option<String>,
}

/// B2 関数実行の port（実装は api 配線＝app-platform FunctionRunner＋token-exchange）。
#[async_trait]
pub trait FunctionPort: Send + Sync {
    async fn invoke(
        &self,
        ctx: &AuthContext,
        spec: FunctionInvokeSpec,
    ) -> Result<serde_json::Value, GatewayError>;
}

/// B2 実行未構成時のフォールバック（502）。
pub struct NoFunctions;

#[async_trait]
impl FunctionPort for NoFunctions {
    async fn invoke(
        &self,
        _ctx: &AuthContext,
        _spec: FunctionInvokeSpec,
    ) -> Result<serde_json::Value, GatewayError> {
        Err(GatewayError::Upstream(
            "B2 関数実行がこの環境では構成されていません".into(),
        ))
    }
}

/// RAG 未構成時のフォールバック（rag.query は 502 を返す）。
pub struct NoRag;

#[async_trait]
impl RagPort for NoRag {
    async fn query(
        &self,
        _ctx: &AuthContext,
        _query: &str,
        _top_k: Option<u32>,
        _trace_id: Option<&str>,
    ) -> Result<Vec<RagHit>, GatewayError> {
        Err(GatewayError::Upstream(
            "RAG 検索がこの環境では構成されていません".into(),
        ))
    }
}

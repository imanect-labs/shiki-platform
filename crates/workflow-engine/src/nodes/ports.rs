//! 能力ノードのチョークポイント・ポート（Task 10.6a/10.8/10.10・engine.md §9.5）。
//!
//! 本番 [`CapabilityNodeExecutor`](super::exec::CapabilityNodeExecutor) は各 node を能力ゲートウェイ
//! （scope ceiling → effect_journal → rate limit → 監査）を通してから、このポート越しに既存
//! チョークポイント（StorageService / SearchService / LlmGateway / Sandbox）を呼ぶ。ポートを
//! トレイト裏に置くことで **workflow-engine を storage/rag/llm/sandbox クレートから切り離す**
//! （トレイト境界不変条件）。具象結線は server 側（`crates/api`）で注入し、テストは fake ポートで
//! executor 単体を回す。認可（OpenFGA）はチョークポイント内で担保され、scope ceiling は executor が
//! 担保する＝二重ゲート（個別ポート実装に認可検査を書かせない）。

use async_trait::async_trait;
use serde_json::Value;
use uuid::Uuid;

/// ノード実行時の認可・トレース文脈（ポート実装が `AuthContext` を組む素材）。
#[derive(Debug, Clone)]
pub struct ExecCtx {
    pub tenant_id: String,
    pub org: String,
    /// 実行主体の subject local id（workflow プリンシパル or ユーザー）。
    pub principal: String,
    /// 実行主体の種別（'user' or 'workflow'）。ポートが AuthContext を組み分ける。
    pub principal_kind: String,
    /// OTel トレース id（監査 ↔ Langfuse ↔ OTel を同一トレースに束ねる）。
    pub trace_id: Option<String>,
}

/// ポート呼び出しの失敗（executor が [`NodeError`](crate::run::NodeError) に写す）。
///
/// `retryable` は [`retry::classify`](crate::retry) の分類材料になる。`code` は監査・分類の語彙。
#[derive(Debug, Clone)]
pub struct PortError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl PortError {
    #[must_use]
    pub fn new(code: &str, message: impl Into<String>, retryable: bool) -> Self {
        PortError {
            code: code.to_string(),
            message: message.into(),
            retryable,
        }
    }

    /// 権限不足（permanent）。
    #[must_use]
    pub fn forbidden(message: impl Into<String>) -> Self {
        PortError::new("forbidden", message, false)
    }

    /// 一時障害（retryable）。
    #[must_use]
    pub fn unavailable(message: impl Into<String>) -> Self {
        PortError::new("unavailable", message, true)
    }

    /// 不正入力（permanent）。
    #[must_use]
    pub fn invalid(message: impl Into<String>) -> Self {
        PortError::new("invalid", message, false)
    }
}

/// storage.write の要求。`idempotency_key`/`op_digest` はチョークポイント側 in-TX effect_journal
/// 用（副作用と journal を同一 TX にして **kill 挟みでも高々 1 バージョン** にする）。
#[derive(Debug, Clone)]
pub struct StorageWriteReq {
    pub parent_id: Option<Uuid>,
    pub name: String,
    pub bytes: Vec<u8>,
    pub content_type: String,
    pub idempotency_key: String,
    pub op_digest: String,
}

/// llm.invoke の要求（llm-gateway 直行・モデル/予算はゲートウェイ側カタログで解決）。
#[derive(Debug, Clone)]
pub struct LlmInvokeReq {
    /// 論理モデル名（未指定はゲートウェイ既定）。
    pub model: Option<String>,
    pub system: Option<String>,
    /// user メッセージ本文（Stage A は単発）。
    pub prompt: String,
    pub max_tokens: Option<u32>,
    /// 生成記録の冪等キー（`<run_id>:<attempt>:<call_ordinal>` 相当）。
    pub idempotency_key: String,
}

/// agent.invoke の要求（サンドボックス起動・**capability は縮小のみ**）。
#[derive(Debug, Clone)]
pub struct AgentInvokeReq {
    /// 実行するコード/指示（ティアは admin ポリシー・既定 gVisor・#346・制約ツールセット）。
    pub code: String,
    /// 実行時間上限（ミリ秒）。
    pub timeout_ms: Option<u64>,
    /// egress 許可ホスト（縮小のみ・空 = 全遮断）。ポートはこれを **上限として** spec を組む
    /// （ノード設定で principal の ReBAC を超える権限は付与できない）。
    pub egress_allowlist: Vec<String>,
}

/// http.request が外部へ送る 1 リクエスト（宛先束縛照合は executor 側で済ませてから渡す）。
#[derive(Debug, Clone)]
pub struct HttpSendReq {
    pub method: String,
    /// 宛先束縛（allowlist ∩ secret.allowed_hosts）を通過済みの URL。
    pub url: String,
    /// ヘッダ（secret 注入済み・Idempotency-Key 付与済み）。
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    /// リダイレクトを追わない（既定拒否・SSRF 対策）。
    pub follow_redirects: bool,
    pub timeout_ms: Option<u64>,
}

/// http.request のレスポンス（**本文・ヘッダは executor が要約し redact する**）。
#[derive(Debug, Clone)]
pub struct HttpSendResp {
    pub status: u16,
    /// レスポンス本文（次ノードへ渡すため保持・監査/journal には載せない）。
    pub body: Vec<u8>,
}

/// 宛先束縛の照合に必要な secret 解決結果（plaintext は監査/ログに出さない）。
#[derive(Debug, Clone)]
pub struct ResolvedSecretView {
    pub plaintext: Vec<u8>,
    /// 登録時に宣言された許可ホスト（宛先束縛・executor が URL ホストと照合）。
    pub allowed_hosts: Vec<String>,
}

/// skill.invoke が実行に使う skill の解決結果（#344 Task 10.1b）。
///
/// ポート実装（server 側）が **実行主体の ReBAC** でレジストリ→artifact を解決する
/// （fail-closed の実行時再検証・ir.md §8）。executor は中身に応じて script / agent 経路へ
/// dispatch するだけ（workflow-engine は artifact クレートへ依存しない・トレイト境界）。
#[derive(Debug, Clone)]
pub struct ResolvedSkillView {
    /// skill 名（監査表示用）。
    pub name: String,
    /// SKILL.md 本文（instructions）。
    pub instructions: String,
    /// 先頭の `.shiki` script 本文（あれば script-runtime 経路で実行する）。
    pub shiki_script: Option<String>,
}

/// 能力ノードが叩く既存チョークポイントの単一ポート（server 側で具象注入）。
///
/// 各メソッドは `ExecCtx` から `AuthContext` を組み、チョークポイント（OpenFGA 認可込み）を呼ぶ。
/// 返す `Value` は次ノードへ渡る出力＝**secret 平文やレスポンス本文を含めない**要約であること
/// （本文が必要な http は [`HttpSendResp`] で別途返し、executor が要約する）。
#[async_trait]
pub trait NodePorts: Send + Sync {
    /// storage.write（`write_file_internal` の in-TX 冪等版）。返り値は書込結果の要約。
    async fn storage_write(&self, ctx: &ExecCtx, req: StorageWriteReq) -> Result<Value, PortError>;

    /// storage.read（`read_file_internal`）。返り値はメタ＋本文の要約（本文は base64 等）。
    async fn storage_read(&self, ctx: &ExecCtx, file_id: Uuid) -> Result<Value, PortError>;

    /// storage.list（`list_children`）。返り値は子ノードのメタ配列。
    async fn storage_list(
        &self,
        ctx: &ExecCtx,
        parent_id: Option<Uuid>,
    ) -> Result<Value, PortError>;

    /// rag.search（`SearchService.search`・二段 authz はサービス内で担保）。
    async fn rag_search(
        &self,
        ctx: &ExecCtx,
        query: &str,
        top_k: Option<u32>,
    ) -> Result<Value, PortError>;

    /// llm.invoke（`LlmGateway.stream` ＋ `record_generation`・trace_id を記録）。
    async fn llm_invoke(&self, ctx: &ExecCtx, req: LlmInvokeReq) -> Result<Value, PortError>;

    /// agent.invoke（`Sandbox` トレイト・ティアは admin ポリシー既定・capability 縮小のみ）。
    async fn agent_invoke(&self, ctx: &ExecCtx, req: AgentInvokeReq) -> Result<Value, PortError>;

    /// http.request の外部送信（宛先束縛照合は executor 済み）。
    async fn http_send(&self, ctx: &ExecCtx, req: HttpSendReq) -> Result<HttpSendResp, PortError>;

    /// secret 解決（can_use 認可＋宛先束縛メタ取得・毎回監査）。宛先束縛照合は executor が行う。
    /// `secrets` 未構成なら `Err(forbidden)`。
    async fn resolve_secret(
        &self,
        ctx: &ExecCtx,
        name: &str,
    ) -> Result<ResolvedSecretView, PortError>;

    /// workflow.start（子ワークフローを名前で起動・権限は実行主体で評価・fire-and-forget）。
    /// 返り値は `{ "run_id": "<uuid>" }`（起動されなければ `{ "run_id": null }`）。
    async fn workflow_start(
        &self,
        ctx: &ExecCtx,
        name: &str,
        input: &Value,
    ) -> Result<Value, PortError>;

    /// skill.invoke の skill 解決（レジストリ version 照合＋**実行主体 ReBAC** の artifact 読取・
    /// fail-closed・#344）。アンインストール/剥奪済みは `Err(forbidden)`（黙って続行しない）。
    async fn skill_resolve(
        &self,
        ctx: &ExecCtx,
        name: &str,
        version: &str,
    ) -> Result<ResolvedSkillView, PortError>;

    /// csv.query（`TabularService.query`・隔離 DuckDB での RO SQL・viewer）。返り値は列＋行の要約。
    async fn csv_query(&self, ctx: &ExecCtx, file_id: Uuid, sql: &str) -> Result<Value, PortError>;

    /// csv.patch（`TabularService.patch`・editor・rev 楽観ロック）。冪等化は capability 層の
    /// effect_journal が担保するため、ポートは副作用の適用のみを行う。`ops` は `PatchOp` 配列の JSON。
    async fn csv_patch(&self, ctx: &ExecCtx, req: CsvPatchReq) -> Result<Value, PortError>;

    /// csv.write（`TabularService.save_new`・作成権限）。冪等化は capability 層が担保する。
    async fn csv_write(&self, ctx: &ExecCtx, req: CsvWriteReq) -> Result<Value, PortError>;
}

/// csv.patch の要求（冪等化は capability 層の effect_journal・ポートは適用のみ）。
#[derive(Debug, Clone)]
pub struct CsvPatchReq {
    pub file_id: Uuid,
    pub base_rev: i64,
    /// `tabular::PatchOp` の配列に解決される JSON 値（ポート実装で型変換する）。
    pub ops: Value,
}

/// csv.write の要求（冪等化は capability 層の effect_journal・ポートは適用のみ）。
#[derive(Debug, Clone)]
pub struct CsvWriteReq {
    pub parent_id: Option<Uuid>,
    pub name: String,
    pub csv_bytes: Vec<u8>,
}

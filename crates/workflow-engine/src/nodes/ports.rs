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

use crate::vocab::AgentTool;

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
    /// 実行中の run（監査で run とワークフローを特定する・ノード実行外は `None`）。
    pub run_id: Option<Uuid>,
    /// 実行中の step の path（監査用・ノード実行外は `None`）。
    pub step_path: Option<String>,
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
///
/// 構造化出力（GAP-13）の項目はどれも `Option` で、`None` は従来どおりの自由な本文。
#[derive(Debug, Clone)]
pub struct LlmInvokeReq {
    /// 論理モデル名。`None` は組織の既定（ポート実装が実行主体の組織の設定で解決する・Auto を含む）。
    /// 指定したモデルが使えなければ既定へ戻さずに失敗する（fail-closed・BUG-12）。
    pub model: Option<String>,
    pub system: Option<String>,
    /// user メッセージ本文（Stage A は単発）。
    pub prompt: String,
    pub max_tokens: Option<u32>,
    /// step の冪等キー（attempt を含まない・会計の鍵の素。使い方は [`NodePorts::llm_invoke`]）。
    pub idempotency_key: String,
    /// 思考の深さ。`None` は組織の既定。書くと組織の既定より優先する（対応しないモデルでは gateway が落とす）。
    pub effort: Option<crate::ir::params::LlmEffort>,
    /// 出力の形（閉じた JSON Schema・[`crate::ir::output_schema::close_objects`] 済み）。`Some` ならポートは
    /// 構造化出力を求める（gateway の `response_format`）。出力の検証と修復はエンジンが行う。
    pub output_schema: Option<Value>,
    /// 形に合わなかった出力の直しの依頼（エンジンが組む）。`Some` なら会話を
    /// `[user: prompt][assistant: previous_output][user: instruction]` にする（user で終える）。
    pub repair: Option<LlmRepair>,
    /// 会計の冪等キーの接尾辞（修復の呼び出しは `#c1`・engine.md §7.3 の呼び出し連番）。
    pub call_suffix: Option<String>,
}

/// ai.decide の要求（llm-gateway の判定のチョークポイント直行・FEAT-1）。
///
/// 質問は保存時に V1 を通したリテラル（params の型をそのまま渡す）。判定の材料は解決済みの値（文字列・
/// オブジェクト・配列・上限はエンジンが確かめる）で、**信頼しないデータ**（決定器は中の指示に従わない）。
#[derive(Debug, Clone)]
pub struct LlmDecideReq {
    /// 判定用のモデルの論理名（組織の許可・元の chat モデルの許可はチョークポイントが確かめる）。
    pub model: String,
    /// 判定の材料（解決済み）。
    pub state: Value,
    /// 質問（順序を保つ）。
    pub questions: Vec<crate::ir::params::DecideQuestion>,
    /// step の冪等キー（attempt を含まない）。使えた判定はこのキーで、使えなかった判定は `{key}#f{uuid}` で
    /// チョークポイントが会計する（ポートでは記録しない）。
    pub idempotency_key: String,
}

/// 構造化出力の修復の依頼（[`LlmInvokeReq::repair`]）。
#[derive(Debug, Clone)]
pub struct LlmRepair {
    /// 形に合わなかった直前の出力（モデルの本文のまま）。
    pub previous_output: String,
    /// 直す点と出し直しの依頼（エンジンが組んだ文）。
    pub instruction: String,
}

/// agent.invoke の要求（agent-core のツールループを 1 回走らせる・BUG-6・D-22）。
///
/// 上限はエンジンがノード設定とデプロイ設定から丸めて渡す（縮めることしかできない）。道具は
/// [`AgentToolGateway`] で渡し、ポート実装（api）は道具の呼び出しをそのまま中継する（権限を判断しない）。
#[derive(Debug, Clone)]
pub struct AgentInvokeReq {
    /// 依頼（自然文）。
    pub instruction: String,
    /// 参考データ（モデルには指示ではないデータとして別に渡す）。
    pub context: Option<Value>,
    /// 論理モデル名。`None` は組織の既定（llm.invoke と同じ解決・fail-closed）。
    pub model: Option<String>,
    /// 手順（モデルの応答）の上限（丸め済み）。
    pub max_steps: u32,
    /// この実行全体のトークンの上限（丸め済み）。
    pub max_total_tokens: u64,
    /// この実行全体の費用の上限（マイクロ USD・デプロイ設定）。
    pub max_cost_usd_micros: i64,
    /// 実行時間の上限（ミリ秒）。手順の境界で止める。
    pub max_duration_ms: u64,
    /// step の冪等キー（attempt 非依存）。
    pub idempotency_key: String,
    /// いまの attempt（会計キーの接頭辞に入れる。再試行・リース回収の消費を重複排除しない）。
    pub attempt: i32,
}

/// agent.invoke の道具の呼び出しを束ねる口（実装はエンジン・D-22）。
///
/// ループ（api）は道具の呼び出しごとにこれを呼び、エンジンが「提示した道具か → scope 天井 → 引数の上限 →
/// 宛先の照合（web.fetch）→ 監査（via=agent）→ [`NodePorts`] の各メソッド → 結果の上限」の順に処理する。
/// script の `Shiki.*` と同じく、権限の一段目（scope 天井・宛先・監査）はエンジンの一点で効き、二段目
/// （OpenFGA・二段 authz）はチョークポイントが担う。
#[async_trait]
pub trait AgentToolGateway: Send + Sync {
    /// 提示してよい道具（allowed_tools ∩ 今のステージで選べる ∩ scope 天井 ∩ web の天井）。
    fn tools(&self) -> &[AgentTool];

    /// 止める合図（run の取り消し・リースの喪失）が立っているか。ループは手順の境界で見る。
    fn is_cancelled(&self) -> bool;

    /// 道具を 1 回呼ぶ。提示していない道具は `invalid`（監査に残す）。
    async fn call(&self, tool: AgentTool, args: &Value) -> Result<Value, PortError>;
}

/// web.fetch の要求（宛先の照合はゲートウェイで済ませてから渡す）。
#[derive(Debug, Clone)]
pub struct WebFetchReq {
    /// 照合を通った URL。
    pub url: String,
    /// 知りたい論点（長いページから関係する節だけを返す）。
    pub query: Option<String>,
    /// 続きを読む開始位置（字）。
    pub offset: Option<u64>,
}

/// code.run の要求（隔離環境で Python を実行する・外部への通信は遮断）。
#[derive(Debug, Clone)]
pub struct CodeRunReq {
    pub code: String,
    /// 実行時間上限（ミリ秒）。
    pub timeout_ms: Option<u64>,
}

/// doc.read の要求（索引済みの文書の本文を窓で読む）。
#[derive(Debug, Clone, Default)]
pub struct DocReadReq {
    pub file_id: Uuid,
    /// 読み始めた版（違えば結果で知らせる）。
    pub version: Option<i64>,
    /// このページ以降。
    pub page: Option<i64>,
    /// この文字列を含む見出しから。
    pub heading: Option<String>,
    /// 続きの位置（前回の結果に書かれた値）。
    pub from: Option<i64>,
    /// 続きの位置の中の字数。
    pub offset: Option<i64>,
    /// 1 回に読む字数。
    pub max_chars: u32,
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
    /// skill の宣言スコープ（`allowed_tools` を能力 API スコープへ写したもの・None は絞らない・#344）。
    /// script 実行時に workflow の scope ceiling とこの集合の**積**を取り、skill 宣言外の
    /// `Shiki.*` 呼び出しを弾く（実効 = workflow scope ∩ skill 宣言 ∩ 実行主体 ReBAC）。
    pub allowed_scopes: Option<Vec<String>>,
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
    ///
    /// 返す Value は `{ text, model, stop_reason? }`。`model` は解決後の論理モデル名（実際に使ったモデル・会計と
    /// 同じ名前）で、executor が監査に使う。`stop_reason`（`end_turn`・`max_tokens`・`refusal` など）は任意で、
    /// 無ければ自然終了とみなす。node の出力には載せない。
    ///
    /// 会計の冪等キー（engine.md §7.2-7.3）:
    /// - 使えた呼び出しは `idempotency_key` ＋ `call_suffix`（1 回目は step のキー・修復は `#c1`）。
    /// - 使えなかった呼び出し（途中で切れた・`output_schema` があって上限で切れた・拒否された・形に合わない）で
    ///   使用量のあるものは `{idempotency_key}#f{uuid}`（呼び出しごとに一意）。step のキーで記録すると、
    ///   再試行・再開で後から成功した分が重複除去で消えるため。形に合うかは
    ///   [`crate::ir::output_schema::validate_output`]（エンジンと同じ検査）で見る。
    async fn llm_invoke(&self, ctx: &ExecCtx, req: LlmInvokeReq) -> Result<Value, PortError>;

    /// ai.decide（`LlmGateway::decide`・組織の許可・利用上限・会計はチョークポイントの中・FEAT-1）。
    ///
    /// 返す Value は判定の結果 `{ resolved_model, answers: { <key>: { type, … } }, calibrated, method, samples }`
    /// （llm-gateway の `DecideResponse` の形）。エンジンが型付きで読み、質問と突き合わせてから出力を組む。
    /// 既定は「未構成」（判定のチョークポイントを結線しない実装・テスト用・H1）。
    async fn llm_decide(&self, _ctx: &ExecCtx, _req: LlmDecideReq) -> Result<Value, PortError> {
        Err(PortError::new(
            "unsupported",
            "AI の判定（ai.decide）はこの環境では使えません",
            false,
        ))
    }

    /// agent.invoke（agent-core のツールループを 1 回走らせる・LLM は llm-gateway を通す・BUG-6）。
    ///
    /// 道具の呼び出しは `tools`（エンジンの能力ゲートウェイ）へ中継する。返す Value は
    /// `{ text, complete, stop, model, citations, tool_calls, skipped_tools, usage }`。
    async fn agent_invoke(
        &self,
        ctx: &ExecCtx,
        req: AgentInvokeReq,
        tools: std::sync::Arc<dyn AgentToolGateway>,
    ) -> Result<Value, PortError>;

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

    // --- 構造化データ（Task 15.7）。既定は「未構成」（data を結線しない実装・テスト用）。---
    //
    // 実装は data の DataStore（行述語・フィールドマスク・OpenFGA の認可込み）を呼ぶ。schedule / event の
    // run（実行主体 = workflow）は、①ワークフロー主体にテーブルが委譲されていること（タプル）を確かめ、
    // ②委譲元ユーザーの権限で読み書きする（行は委譲元の行述語で絞られる）。

    /// data.list_tables（実行主体が viewer のテーブル一覧）。
    async fn data_list_tables(&self, _ctx: &ExecCtx) -> Result<Value, PortError> {
        Err(data_unconfigured())
    }

    /// data.query（`DataStore::run_query`・data の `DataQuery` をそのまま渡す）。返り値は `QueryResult`。
    async fn data_query(
        &self,
        _ctx: &ExecCtx,
        _table_id: Uuid,
        _query: &data::DataQuery,
    ) -> Result<Value, PortError> {
        Err(data_unconfigured())
    }

    /// data.get（`DataStore::get_record`）。返り値はマスク済みのレコード。
    async fn data_get(
        &self,
        _ctx: &ExecCtx,
        _table_id: Uuid,
        _record_id: Uuid,
    ) -> Result<Value, PortError> {
        Err(data_unconfigured())
    }

    /// data の書込（作成・更新・削除・遷移・一括 upsert）。冪等化は capability 層の effect_journal が
    /// 担保し、ポートは適用のみを行う（一括 upsert は `idempotency_key` から取込 ID を決めて二重取込を防ぐ）。
    async fn data_write(&self, _ctx: &ExecCtx, _req: DataWriteReq) -> Result<Value, PortError> {
        Err(data_unconfigured())
    }

    /// 承認（human.approval）の停止時に、束縛した行のいまの状態・rev・FSM でその状態から進める状態
    /// （遷移の定義だけ・actor 述語は評価しない）・担当者の列の値を、実行主体の権限で読む（GAP-08）。
    ///
    /// 実装は data の `FsmStore`（テーブルの viewer・行述語・フィールドマスク込み）。既定は「未構成」。
    async fn data_approval_target(
        &self,
        _ctx: &ExecCtx,
        _table_id: Uuid,
        _record_id: Uuid,
    ) -> Result<ApprovalTarget, PortError> {
        Err(data_unconfigured())
    }

    // --- agent.invoke の道具（BUG-6・D-22）。既定は「未構成」（結線しない実装・テスト用）。---
    //
    // ゲートウェイ（scope 天井・宛先・監査）を通った呼び出しだけが来る。実装は実行主体の権限で
    // チョークポイントを呼び、結果は道具の上限の中に収める。

    /// web.search（`SearchProvider`・検索語が検索サービスへ出る）。返り値は `{ results: [{title, url, snippet}] }`。
    async fn web_search(&self, _ctx: &ExecCtx, _query: &str) -> Result<Value, PortError> {
        Err(tool_unconfigured("web.search"))
    }

    /// web.fetch（解決後の IP の検証・リダイレクト非追従）。返り値は `{ url, text, citations? }`。
    async fn web_fetch(&self, _ctx: &ExecCtx, _req: WebFetchReq) -> Result<Value, PortError> {
        Err(tool_unconfigured("web.fetch"))
    }

    /// code.run（`Sandbox`・egress 全遮断・成果物は保存しない）。返り値は `{ stdout, exit_code }`。
    async fn code_run(&self, _ctx: &ExecCtx, _req: CodeRunReq) -> Result<Value, PortError> {
        Err(tool_unconfigured("code.run"))
    }

    /// doc.read（`SearchService::read_document`＋viewer 判定・権限なしと不在は同じ応答）。返り値は `{ text }`。
    async fn doc_read(&self, _ctx: &ExecCtx, _req: DocReadReq) -> Result<Value, PortError> {
        Err(tool_unconfigured("doc.read"))
    }

    /// storage.read の窓読み（テキストだけ・`offset` 字目から `max_chars` 字）。
    /// 返り値は `{ id, name, content_type, size, text, offset, next_offset }`（続きが無ければ next_offset は null）。
    async fn storage_read_window(
        &self,
        _ctx: &ExecCtx,
        _file_id: Uuid,
        _offset: u64,
        _max_chars: u32,
    ) -> Result<Value, PortError> {
        Err(tool_unconfigured("storage.read"))
    }
}

/// 道具のポートが未構成のエラー（permanent・モデルには失敗として返る）。
fn tool_unconfigured(tool: &str) -> PortError {
    PortError::new(
        "unsupported",
        format!("道具 {tool} はこの環境では使えません"),
        false,
    )
}

/// 承認の停止時に読む行の状態（[`NodePorts::data_approval_target`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalTarget {
    /// いまの状態（未設定なら `None`）。
    pub current: Option<String>,
    /// いまの rev。
    pub rev: i64,
    /// FSM でいまの状態から進める状態（定義順・重複なし）。
    pub targets: Vec<String>,
    /// ユーザー・ロールの列のうち実行主体に読めるもの（参照名 → 種別と値）。読めない列は入らない。
    pub refs: std::collections::BTreeMap<String, (crate::approval::AssigneeKind, Option<String>)>,
}

/// data ポート未構成のエラー（permanent・黙って成功扱いにしない）。
fn data_unconfigured() -> PortError {
    PortError::new(
        "unsupported",
        "構造化データ（data）が構成されていません",
        false,
    )
}

/// data 書込の要求。
#[derive(Debug, Clone)]
pub struct DataWriteReq {
    pub table_id: Uuid,
    pub op: DataWriteOp,
    /// ステップ（script ならホスト呼び出し）の冪等キー。
    pub idempotency_key: String,
}

/// data 書込の操作（ポートは DataStore の対応する入口を呼ぶ・status は Transition でしか動かない）。
#[derive(Debug, Clone)]
pub enum DataWriteOp {
    Create {
        data: Value,
    },
    Update {
        record_id: Uuid,
        patch: Value,
        /// 省略時は実行時点の rev を読んでから書く。
        expected_rev: Option<i64>,
    },
    Delete {
        record_id: Uuid,
        expected_rev: Option<i64>,
    },
    Transition {
        record_id: Uuid,
        to: String,
        expected_rev: Option<i64>,
    },
    BulkUpsert {
        rows: Vec<data::BulkUpsertRow>,
        abort_on_invalid: bool,
    },
}

impl DataWriteOp {
    /// 能力 API 名（監査・スコープの語彙）。
    #[must_use]
    pub fn api(&self) -> &'static str {
        match self {
            DataWriteOp::Create { .. } => "data.record.create",
            DataWriteOp::Update { .. } => "data.record.update",
            DataWriteOp::Delete { .. } => "data.record.delete",
            DataWriteOp::Transition { .. } => "data.transition",
            DataWriteOp::BulkUpsert { .. } => "data.bulk_upsert",
        }
    }
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

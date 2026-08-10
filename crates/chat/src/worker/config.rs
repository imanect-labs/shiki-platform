//! ワーカーの設定と依存束（`mod.rs` から分割・行数規約）。
//!
//! 差し替え点（トレイト裏）と数値の既定値をここに集める。`ChatWorker` 本体はループと
//! run のライフサイクルだけを持つ。

use std::sync::Arc;

use llm_gateway::LlmGateway;
use rag::SearchService;

/// ワーカーの設定。
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// システムプロンプト。
    pub system_prompt: String,
    /// 論理モデル名（未指定は gateway 既定）。
    pub model: Option<String>,
    /// リース秒（ハートビート間隔の数倍を推奨）。
    pub lease_secs: i64,
    /// エージェントモードの最大ステップ。
    pub max_steps: usize,
    /// 通常チャットで旧・無条件 RAG 注入経路（`run_classic_mode`）を使うか（既定 false）。
    /// false ならモデル裁量ループ（issue #102）。運用で確実に毎ターン検索したい場合のみ true。
    pub classic_rag: bool,
    /// 自律プロファイルの最大ステップ（長ホライズン）。
    pub autonomous_max_steps: usize,
    /// 自律プロファイルの累積トークン上限（予算ガード・Task 5.7）。
    ///
    /// 委譲（#391）は子の消費が親へ積まれるため 1 run の累計が**単一エージェントの 10 倍規模**に
    /// なる。絞る deployment はここと `subagent.max_per_run` を一緒に下げる。
    pub autonomous_max_tokens: u64,
    /// 通常チャット 1 応答の最大トークン。reasoning 系は思考も消費するため、
    /// 長文成果物・大きなツール引数に耐える値にする（旧 2048 は引数切れを起こした）。
    pub max_tokens: u32,
    /// 同一ステップ内で並列実行する冪等 read ツール（doc_search/web_search/web_fetch）の上限。
    ///
    /// deep research の検索→複数取得ファンアウトを直列にしないための有界並列度（#349）。
    /// 検索 API の rate limit に合わせて絞れる（1 で従来どおりの逐次）。
    pub parallel_read_tools: usize,
    /// 自律プロファイルの累積コスト上限（マイクロ USD・Task 5.7）。
    pub autonomous_max_cost_usd_micros: i64,
    /// サブエージェント委譲（#391）の上限。1 体の予算・1 run の体数・子の並列度の三重で縛る。
    pub subagent: agent_core::SubagentLimits,
    /// 自律 shell に同梱するゲストコマンドパッケージ（coreutils 等・Task 5.4）。
    pub sandbox_software: Vec<String>,
    /// コード実行系（code_interpreter / shell）の隔離ティア（admin ポリシー・design §4.6）。
    /// 既定は gVisor（#346・native Python。rootfs は numpy/pandas 同梱がビルドで保証される）。
    /// runsc の無い開発ホストは wasm へ明示退避する（自動降格はしない）。
    pub sandbox_backend: agent_core::SandboxBackend,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        WorkerConfig {
            system_prompt: "あなたは社内文書に基づいて日本語で丁寧に回答するアシスタントです。\
                根拠がある場合は検索結果を活用し、分からない場合は正直に伝えてください。"
                .to_string(),
            model: None,
            lease_secs: 30,
            max_steps: 6,
            classic_rag: false,
            autonomous_max_steps: 50,
            // 既定: 新規ぶん 250 万トークン・課金 8 USD。**委譲込みの累計**（子の消費は
            // 親へ積まれる）。100 件規模の調査を 16 体へ委譲すると、子だけで新規 120k × 16
            // ≒ 190 万。上限判定は再送を数えない（`Spent::fresh_tokens`・#404）ので、この値が
            // そのまま「1 本の調査で読み書きできる分量」になる。テナント/skill で上書き可。
            autonomous_max_tokens: 2_500_000,
            max_tokens: 8192,
            // 委譲を既定にしたので、この並列度は**同時に走るサブエージェント数**を決める。
            // 子はそれぞれ 4 並列で取得するため、同時取得は最大 24 件になる（別ホスト前提。
            // 同一ホストは `politeness_key` が直列化する）。画面のロールが実際に速く流れる。
            parallel_read_tools: 6,
            autonomous_max_cost_usd_micros: 8_000_000,
            // 既定は `SubagentLimits::default()`（1 体 8 ステップ・20 万トークン・1 run 16 体）。
            // 同時実行数は `parallel_read_tools`（親側 4）が決める。
            subagent: agent_core::SubagentLimits::default(),
            sandbox_software: vec!["coreutils".to_string()],
            // 既定ティアの単一ソースは enum の `#[default]`（gVisor・#346）。ここに別のリテラルを
            // 持たない（「もう一つの正」を作らない）。
            sandbox_backend: agent_core::SandboxBackend::default(),
        }
    }
}

/// ワーカーの依存一式（トレイト裏の差し替え点を束ねる）。
///
/// `Option` の依存は未配線ならその機能（ツール）を提示しない。個別引数で渡すと
/// 依存追加のたびに全呼び出し箇所が壊れるため struct で束ねる。
pub struct WorkerDeps {
    /// LLM ゲートウェイ（単一チョークポイント）。
    pub gateway: LlmGateway,
    /// 社内文書検索（doc_search / 古典 RAG 注入）。
    pub search: Option<Arc<SearchService>>,
    /// サンドボックス（code_interpreter / shell 用）。
    pub sandbox: Option<Arc<dyn agent_core::Sandbox>>,
    /// 成果物の保存先（code_interpreter が /workspace のファイルを保存する・Task 4.11）。
    pub artifacts: Option<Arc<dyn agent_core::ArtifactStore>>,
    /// web 検索プロバイダ（web_search 用・Brave/SearXNG/Stub）。
    pub web_search: Option<Arc<dyn websearch::SearchProvider>>,
    /// 文書パーサ（web_fetch が取得した PDF/Office を Docling で読む・#405）。
    /// RAG と同一インスタンスを共有する。未配線なら web_fetch は文書を拒否する。
    pub parser: Option<Arc<dyn rag::DocumentParser>>,
    /// StorageService（自律プロファイルの file CRUD/shell ワークスペース・Task 5.4）。
    /// 未配線なら自律ツール（fs_*/grep/shell）を提示しない。
    pub storage: Option<Arc<storage::StorageService>>,
    /// UI スペック検証（emit_ui ツール・Task 6.4）。未配線なら emit_ui を提示しない。
    pub ui_validator: Option<Arc<gui::SpecValidator>>,
    /// skill / ミニアプリのピン解決（Task 6.7/6.9/6.10）。未配線でピンがある run は失敗する
    /// （fail-closed・skill 無しで黙って生成しない）。
    pub skill_artifacts: Option<Arc<artifact::ArtifactStore>>,
    /// skill カタログ源（skill ツールの動的 description・#344 Task 10.11）。
    /// skill_artifacts と両方揃った時のみ skill ツールを提示する。
    pub skill_catalog: Option<Arc<dyn crate::skill_catalog::SkillCatalogSource>>,
    /// ワークフロー IR ストア（emit_workflow / read_workflow・Task 10.13）。
    /// カタログ源と両方揃った時のみツールを提示する。
    pub workflow_store: Option<Arc<workflow_engine::WorkflowStore>>,
    /// 保存 API と同一のカタログ源（secret 名→許可ホスト・モデル一覧・Task 10.13）。
    pub workflow_catalog: Option<Arc<dyn crate::workflow_tool::WorkflowCatalogSource>>,
    /// ノート共同編集ハブ（document.edit / document.read・Task 11P.4）。
    /// storage と両方揃った時のみノート編集ツールを提示する。
    pub collab: Option<Arc<collab::CollabHub>>,
    /// CSV クエリ/パッチサービス（csv.query / csv.patch / csv.write・Task 11P.9）。
    pub tabular: Option<Arc<tabular::TabularService>>,
    /// AI Office 編集（office.edit・Task 11.8）。office 有効時のみ配線し、
    /// 未配線なら office.edit を提示しない。
    pub office: Option<Arc<office::OfficeEditor>>,
    /// AI ライブ編集（office.live_edit・CoolWSD headless 参加・issue #352）。
    /// office 有効時のみ配線し、未配線なら office.live_edit を提示しない。
    pub office_live: Option<Arc<office::live::LiveEditor>>,
    /// Office の新規作成（save_document / save_sheet・#381）。空テンプレ実体化＋
    /// Collabora への paste を束ねる。未配線なら作成ツールを提示しない。
    pub office_creator: Option<Arc<office::OfficeCreator>>,
}

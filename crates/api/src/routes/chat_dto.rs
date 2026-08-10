//! チャット API の DTO（`routes/chat.rs` から分割・行数規約）。
//!
//! 型は chat 側のドメイン型をそのまま OpenAPI へ流し、フロント `chat-api.ts` と同型に保つ
//! （手書きミラー禁止・codegen が正）。

use chat::{Attachment, Message, Thread, ThreadRole};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use storage::ShareTarget;
use utoipa::ToSchema;
use uuid::Uuid;

/// keyset カーソル（更新日時＋id）。
pub(super) type Cursor = (Option<DateTime<Utc>>, Option<Uuid>);

/// アーティファクト参照（skill / ミニアプリの選択・version 未指定は current をピン）。
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
pub struct ArtifactPinRequest {
    pub artifact_id: Uuid,
    #[serde(default)]
    pub version: Option<i64>,
}

/// エージェントモードのワークスペース作成場所（Phase 6 UX）。
///
/// `existing`＝選んだフォルダをそのままワークスペースにする、`new_under`＝選んだ親フォルダの
/// 配下に `agent-workspace-<thread>` を新規作成する。いずれも `folder_id` に editor が要る。
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum WorkspaceChoiceRequest {
    Existing { folder_id: Uuid },
    NewUnder { folder_id: Uuid },
}

/// スレッド作成リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateThreadRequest {
    #[serde(default)]
    pub title: Option<String>,
    /// エージェントモード既定（既定 false＝通常チャット）。
    #[serde(default)]
    pub agent_mode: Option<bool>,
    /// 初期コンテキストに適用する skill（Task 6.7・version 込みでピンされる）。
    /// `skills` の単数形（後方互換）。`skills` と併用時は `skills` が優先。
    #[serde(default)]
    pub skill: Option<ArtifactPinRequest>,
    /// 最初からロード済みにする skill（複数可・順序付き・#344）。
    #[serde(default)]
    pub skills: Option<Vec<ArtifactPinRequest>>,
    /// ミニアプリ経由のセッション（Task 6.10・skill ピンはバンドルから解決される）。
    /// skill と併用した場合はミニアプリ側が優先。
    #[serde(default)]
    pub mini_app: Option<ArtifactPinRequest>,
    /// エージェントモードのワークスペース作成場所（未指定は Drive 直下＝現行挙動）。
    #[serde(default)]
    pub workspace: Option<WorkspaceChoiceRequest>,
    /// 由来ノート（ノートの分割ビューから作るスレッド・issue #282）。指定時はサイドバー
    /// 履歴で「ノート由来」と分かり、当該ノートの会話一覧に載る。通常チャットは未指定。
    #[serde(default)]
    pub origin_note_id: Option<Uuid>,
}

/// スレッド一覧レスポンス（keyset ページング）。
#[derive(Debug, Serialize, ToSchema)]
pub struct ThreadListResponse {
    pub threads: Vec<Thread>,
    pub next_cursor: Option<String>,
}

/// メッセージ一覧レスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct MessagesResponse {
    pub messages: Vec<Message>,
    /// 進行中（非端末）の run id。再訪時に承認 API を叩けるよう UI へ渡す（Task 5.6）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_run_id: Option<Uuid>,
    /// 進行中 run が自律プロファイルか（再訪時にエージェントモード UI・モードセレクタを復元する・#350）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_run_autonomous: Option<bool>,
    /// 進行中 run の生成先 assistant メッセージ id。この run が出す genui カードの
    /// アクション照合先であり、**run 完了後に**実行できるようになる（それまでは順番待ち）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_assistant_message_id: Option<Uuid>,
    /// **順番待ち**の発話（投入順）。生成中でも発話は受け付けてサーバ側で直列化するため、
    /// 再訪時に「送ったがまだ生成が始まっていない」を復元して取り消せるようにする。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queued_runs: Vec<QueuedRun>,
}

/// 順番待ちの発話 1 件。
#[derive(Debug, Serialize, ToSchema)]
pub struct QueuedRun {
    /// 発話（user メッセージ）の id。トランスクリプト上の並べ替えに使う。
    pub user_message_id: Uuid,
    /// 取り消し用の run id。
    pub run_id: Uuid,
}

/// 発話送信リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct PostMessageRequest {
    pub text: String,
    #[serde(default)]
    pub attachments: Option<Vec<Attachment>>,
    /// エディタの選択コンテキスト（選択→AI 指示・Task 11.10）。node_id は実行主体の
    /// viewer 権限で再解決できた場合のみ受理される（fail-closed・design §4.8.3）。
    #[serde(default)]
    pub context: Option<chat::SelectionContext>,
    /// このメッセージのエージェントモード上書き（未指定はスレッド既定）。
    #[serde(default)]
    pub agent_mode: Option<bool>,
    /// 自律プロファイルで実行するか（長ホライズン・フルツール・予算・承認・Task 5.1）。
    #[serde(default)]
    pub autonomous: Option<bool>,
    /// **この発話にだけ**適用する skill（スラッシュコマンド起動・#387）。
    ///
    /// thread のピン（「最初からロード済み」）とは別で、次の発話には持ち越さない。
    /// version 省略は current を解決する。読めない skill は 403/404（fail-closed）。
    #[serde(default)]
    pub skills: Option<Vec<ArtifactPinRequest>>,
}

/// 発話送信レスポンス（202・生成は接続非依存ジョブで継続）。
#[derive(Debug, Serialize, ToSchema)]
pub struct PostMessageResponse {
    pub run_id: Uuid,
    pub user_message_id: Uuid,
    pub assistant_message_id: Uuid,
    pub agent_mode: bool,
}

/// 共有/解除リクエスト（viewer/commenter/editor）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct ShareThreadRequest {
    pub target: ShareTarget,
    pub role: ThreadRole,
}

/// 共有相手 1 件。
#[derive(Debug, Serialize, ToSchema)]
pub struct ThreadShareEntry {
    pub target: ShareTarget,
    pub role: ThreadRole,
}

/// 共有相手一覧レスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct ThreadSharesResponse {
    pub shares: Vec<ThreadShareEntry>,
}

/// スレッドの skill ピン集合の置換リクエスト（途中変更・owner のみ・#344）。
/// ミニアプリ経由のスレッドは 409 相当で拒否される（バンドル定義のピンが正）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct SetThreadSkillsRequest {
    pub skills: Vec<ArtifactPinRequest>,
}

/// スレッドの由来ノート設定リクエスト（下書き確定→ノート実体化の紐付け・issue #282）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct SetOriginNoteRequest {
    pub note_id: Uuid,
}

/// 一覧クエリ。
#[derive(Debug, Deserialize)]
pub struct ListThreadsQuery {
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
    /// 由来ノートで絞り込む（ノート側の会話一覧・issue #282）。未指定は全件（履歴）。
    #[serde(default)]
    pub origin_note_id: Option<Uuid>,
}

/// SSE クエリ（Last-Event-ID の代替）。
#[derive(Debug, Deserialize)]
pub struct StreamQuery {
    #[serde(default)]
    pub last_event_id: Option<i64>,
}

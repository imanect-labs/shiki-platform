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

use crate::error::ApiError;

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
    /// 進行中 run の生成先 assistant メッセージ id。この run が本文に書く図のボタンは
    /// **run 完了後に**押せるようになる（回答の確定時に束縛を解決する・#604）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_assistant_message_id: Option<Uuid>,
    /// **順番待ち**の発話（投入順）。生成中でも発話は受け付けてサーバ側で直列化するため、
    /// 再訪時に「送ったがまだ生成が始まっていない」を復元して取り消せるようにする。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queued_runs: Vec<QueuedRun>,
    /// 最後の LLM リクエストのコンテキスト使用量（再訪時のコンテキスト表示）。生成中は SSE の
    /// `context_usage` が新しい値を届ける。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_usage: Option<agent_core::ContextUsage>,
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
    /// この発話のモデル（`auto` かモデル ID・#553）。省略は組織の既定（通常 Auto）。
    /// 組織で使えないモデルは 400。
    #[serde(default)]
    pub model: Option<String>,
    /// 思考の深さ。特定のモデルで動くとき（モデルを選んだとき、または `model` を省いて
    /// 組織の既定が特定のモデルのとき）に効く。Auto では組織の設定に従う。
    #[serde(default)]
    pub effort: Option<super::llm_models::EffortLevel>,
    /// **この発話にだけ**使ってほしいツール（利用者が指定できるツールの名前・#734）。
    ///
    /// `skills` と同じく次の発話には持ち越さない。**誘導であって権限ではない**: 検索なしで
    /// 定義を載せ、使うよう促すだけで、提示ツール・承認・認可は変えない（いまの会話で使えない
    /// ツールは提示されない）。指定できるのは [`agent_core::USER_SELECTABLE_TOOLS`] だけで、
    /// 未知の名前・それ以外のツール（`shell` / `fs_delete` 等）は 400。重複は除く。
    ///
    /// 受け口は文字列で受けて照合はハンドラで行う（enum で受けると未知名が `Json` 抽出の時点で
    /// 422 になり、他の検証エラーと同じ 400＋理由の形で返せない）。schema には指定できる集合を
    /// 同じ定数から出す（手書きの名前一覧は作らない）。
    #[serde(default)]
    #[schema(schema_with = requested_tools_schema)]
    pub tools: Option<Vec<String>>,
}

/// `PostMessageRequest.tools` の schema（[`agent_core::USER_SELECTABLE_TOOLS`] の名前の閉集合）。
fn requested_tools_schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
    use utoipa::openapi::schema::{ArrayBuilder, ObjectBuilder, Type};
    ArrayBuilder::new()
        .items(
            ObjectBuilder::new()
                .schema_type(Type::String)
                .enum_values(Some(
                    agent_core::USER_SELECTABLE_TOOLS.iter().map(|t| t.as_str()),
                )),
        )
        .into()
}

/// 発話ごとのツール指定を照合する（投入順・重複除去・#734）。
///
/// 未知の名前と、語彙にはあるが利用者が指定できないツールは 400（`{status,title,detail}`・
/// モデル指定の検証エラーと同じ形）。件数は「指定できる集合（閉集合）から重複を除いたもの」
/// なので、その集合の大きさを超えない（別途の上限は要らない）。
pub(super) fn parse_requested_tools(
    raw: Option<&[String]>,
) -> Result<Vec<agent_core::ToolName>, ApiError> {
    let mut out: Vec<agent_core::ToolName> = Vec::new();
    for name in raw.unwrap_or_default() {
        // 応答に返す名前は短く切る（任意長の入力をそのまま反射しない）。
        let shown = || name.chars().take(64).collect::<String>();
        let Some(tool) = agent_core::ToolName::parse(name) else {
            return Err(bad_request(&format!(
                "tools に未知のツール名があります: `{}`",
                shown()
            )));
        };
        if !tool.is_user_selectable() {
            return Err(bad_request(&format!(
                "tools には指定できないツールです: `{}`",
                shown()
            )));
        }
        if !out.contains(&tool) {
            out.push(tool);
        }
    }
    Ok(out)
}

fn bad_request(detail: &str) -> ApiError {
    ApiError::UnprocessableJson(serde_json::json!({
        "status": 400,
        "title": "Bad Request",
        "detail": detail,
    }))
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

/// 共有相手 1 件。`display_name` / `email` は呼び出し元テナント（＋ org）のディレクトリから
/// 解決した表示用の補助（#741・別テナント・未登録は `null`。`email` は user のみ）。
#[derive(Debug, Serialize, ToSchema)]
pub struct ThreadShareEntry {
    pub target: ShareTarget,
    pub role: ThreadRole,
    pub display_name: Option<String>,
    pub email: Option<String>,
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
    /// タイトルの部分一致で絞り込む（検索パレット・issue #591）。
    #[serde(default)]
    pub q: Option<String>,
}

/// SSE クエリ（Last-Event-ID の代替）。
#[derive(Debug, Deserialize)]
pub struct StreamQuery {
    #[serde(default)]
    pub last_event_id: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::ToolName;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    fn detail(err: ApiError) -> String {
        match err {
            ApiError::UnprocessableJson(v) => {
                assert_eq!(v["status"], 400);
                v["detail"].as_str().unwrap().to_string()
            }
            other => panic!("400 の構造化ボディを期待: {other:?}"),
        }
    }

    #[test]
    fn requested_tools_dedupe_in_order() {
        let raw = names(&["web_search", "doc_search", "web_search"]);
        assert_eq!(
            parse_requested_tools(Some(&raw)).unwrap(),
            vec![ToolName::WebSearch, ToolName::DocSearch]
        );
        assert!(parse_requested_tools(None).unwrap().is_empty());
        assert!(parse_requested_tools(Some(&[])).unwrap().is_empty());
        // 指定できる集合は全部まとめて指定できる（重複込みでも集合の大きさに収まる）。
        let mut all: Vec<String> = agent_core::USER_SELECTABLE_TOOLS
            .iter()
            .map(|t| t.as_str().to_string())
            .collect();
        all.extend(all.clone());
        assert_eq!(
            parse_requested_tools(Some(&all)).unwrap(),
            agent_core::USER_SELECTABLE_TOOLS
        );
    }

    #[test]
    fn unknown_tool_is_rejected_with_reason() {
        // メタツール（plan / tool_search）は語彙の外＝指定できない。
        for bad in ["sudo", "plan", "tool_search", "WEB_SEARCH", ""] {
            let raw = names(&["doc_search", bad]);
            let d = detail(parse_requested_tools(Some(&raw)).unwrap_err());
            assert!(d.contains("未知のツール名"), "{bad}: {d}");
        }
        let long = "x".repeat(500);
        let d = detail(parse_requested_tools(Some(&[long])).unwrap_err());
        assert!(d.chars().count() < 120, "{d}");
    }

    /// 語彙にはあるが利用者が指定できないツール（破壊系・内部用）は 400（格上げしない）。
    #[test]
    fn non_selectable_tool_is_rejected_with_reason() {
        for bad in ToolName::ALL.iter().filter(|t| !t.is_user_selectable()) {
            let raw = names(&["web_search", bad.as_str()]);
            let d = detail(parse_requested_tools(Some(&raw)).unwrap_err());
            assert!(
                d.contains("指定できないツール") && d.contains(bad.as_str()),
                "{bad:?}: {d}"
            );
        }
    }

    /// OpenAPI の schema は指定できる集合（同じ定数から）を出す。
    #[test]
    fn tools_schema_lists_only_selectable_names() {
        let schema = serde_json::to_value(requested_tools_schema()).unwrap();
        assert_eq!(schema["type"], "array");
        let wire: Vec<&str> = schema["items"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let expected: Vec<&str> = agent_core::USER_SELECTABLE_TOOLS
            .iter()
            .map(|t| t.as_str())
            .collect();
        assert_eq!(wire, expected);
    }

    #[test]
    fn post_message_request_accepts_tools_field() {
        let req: PostMessageRequest =
            serde_json::from_str(r#"{"text":"hi","tools":["web_search","nope"]}"#).unwrap();
        assert_eq!(req.tools, Some(names(&["web_search", "nope"])));
        let req: PostMessageRequest = serde_json::from_str(r#"{"text":"hi"}"#).unwrap();
        assert!(req.tools.is_none());
    }
}

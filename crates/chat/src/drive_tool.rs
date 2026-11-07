//! ドライブのファイル・フォルダを探す／移動するツール（drive.find / drive.move・#942）。
//!
//! 「経理フォルダに保存して」「この資料を 2026 年度フォルダへ移して」を AI に任せるための
//! 最小の組: `drive.find` で名前から id を引き、作成系ツールの `parent_id` か `drive.move` に渡す。
//! 種別（ノート/Office/CSV/スライド/フォルダ）を問わない。
//!
//! 認可は StorageService に一任する（チョークポイント）。発話ユーザーの `AuthContext` のまま
//! 呼び、検索は viewer で post-filter、移動は対象と移動先の editor を要求する（昇格しない）。
//! 権限不足と未検出は同じ文言に畳む（存在秘匿）。

use std::sync::Arc;

use agent_core::{Tool, ToolError, ToolName, ToolOutcome};
use authz::AuthContext;
use serde::Deserialize;
use storage::{ChildSort, Crumb, Node, NodeKind, StorageError, StorageService};
use uuid::Uuid;

/// 1 回の検索で返す件数の上限（パス解決を件数分行うため小さく保つ）。
const MAX_FIND_RESULTS: usize = 20;

/// 作成系ツールの保存先 `parent_id` を読む（#942）。省略・空文字・`"root"` はルート（`None`）。
///
/// 省略可能な 1 引数の書き方の揺れ（空で埋める・drive.move と同じ `"root"`）で呼び出し全体を
/// 失敗させない。それ以外で UUID でなければ入力エラー（モデルが drive.find で引き直せる）。
pub(crate) fn de_parent_id<'de, D>(d: D) -> Result<Option<Uuid>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: Option<String> = Option::deserialize(d)?;
    match raw.as_deref().map(str::trim) {
        None | Some("" | "root") => Ok(None),
        Some(s) => Uuid::parse_str(s)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

/// 下書き系ツールの保存先 `parent_id` を読む（#942）。保存ダイアログの初期値になるだけなので、
/// 「指定なし（前回の指定を保つ）」と「ルートに戻す（`"root"`）」を区別して下書きへ載せる。
pub(crate) fn de_draft_parent<'de, D>(d: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: Option<String> = Option::deserialize(d)?;
    match raw.as_deref().map(str::trim) {
        None | Some("") => Ok(None),
        Some("root") => Ok(Some("root".into())),
        Some(s) => Uuid::parse_str(s)
            .map(|id| Some(id.to_string()))
            .map_err(serde::de::Error::custom),
    }
}

/// ツールが使うドライブ操作（テストで差し替える境界・本番は [`StorageService`]）。
#[async_trait::async_trait]
pub trait DriveOps: Send + Sync {
    /// 名前の部分一致で、本人に見えるノードを探す（system ノードは含まない）。
    async fn find(
        &self,
        ctx: &AuthContext,
        query: &str,
        kind: Option<NodeKind>,
        limit: usize,
        trace_id: Option<&str>,
    ) -> Result<Vec<Node>, StorageError>;
    /// 自身まで読める祖先列（root→自身）。末尾が自身。
    async fn path(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Vec<Crumb>, StorageError>;
    /// 移動・名前変更（`new_parent`: `None`=移動しない／`Some(None)`=ルートへ）。
    #[allow(clippy::option_option)]
    async fn update(
        &self,
        ctx: &AuthContext,
        kind: NodeKind,
        node_id: Uuid,
        new_name: Option<&str>,
        new_parent: Option<Option<Uuid>>,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError>;
}

#[async_trait::async_trait]
impl DriveOps for StorageService {
    async fn find(
        &self,
        ctx: &AuthContext,
        query: &str,
        kind: Option<NodeKind>,
        limit: usize,
        trace_id: Option<&str>,
    ) -> Result<Vec<Node>, StorageError> {
        let page = self
            .search_nodes_by_name_kind(
                ctx,
                query,
                kind,
                ChildSort::default(),
                None,
                limit,
                trace_id,
            )
            .await?;
        Ok(page.items)
    }

    async fn path(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Vec<Crumb>, StorageError> {
        self.breadcrumb(ctx, node_id, trace_id).await
    }

    async fn update(
        &self,
        ctx: &AuthContext,
        kind: NodeKind,
        node_id: Uuid,
        new_name: Option<&str>,
        new_parent: Option<Option<Uuid>>,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        match kind {
            NodeKind::File => {
                self.update_file(ctx, node_id, new_name, new_parent, trace_id)
                    .await
            }
            NodeKind::Folder => {
                self.update_folder(ctx, node_id, new_name, new_parent, trace_id)
                    .await
            }
        }
    }
}

fn kind_label(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Folder => "フォルダ",
        NodeKind::File => "ファイル",
    }
}

/// 祖先列を「A / B / C」で表す（読める接尾のみ・読めない祖先名は出さない）。
fn path_text(crumbs: &[Crumb]) -> String {
    crumbs
        .iter()
        .map(|c| c.name.as_str())
        .collect::<Vec<_>>()
        .join(" / ")
}

/// 名前でファイル・フォルダを探すツール（読み取りのみ）。
pub struct DriveFindTool {
    drive: Arc<dyn DriveOps>,
}

impl DriveFindTool {
    pub fn new(drive: Arc<dyn DriveOps>) -> Self {
        DriveFindTool { drive }
    }
}

#[derive(Debug, Deserialize)]
struct FindInput {
    /// 名前の一部（空白区切りは AND）。
    query: String,
    /// `folder` / `file` / 省略（両方）。
    #[serde(default)]
    kind: Option<String>,
    /// 返す件数（1..=20・既定 10）。
    #[serde(default)]
    limit: Option<usize>,
}

#[async_trait::async_trait]
impl Tool for DriveFindTool {
    fn name(&self) -> &str {
        ToolName::DriveFind.as_str()
    }
    fn description(&self) -> &'static str {
        "ドライブのファイル・フォルダを名前で探し、id と場所（親フォルダの並び）を返す。\
         ユーザーが「〇〇フォルダに保存して」「〇〇へ移動して」と言ったときに、保存先・移動先の\
         フォルダ id を調べるために使う（見つけた id を作成系ツールの parent_id や drive.move に渡す）。\
         同じ名前が複数あるときは場所で見分け、決められなければユーザーに確認する。"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "名前の一部（空白区切りは AND）" },
                "kind": { "type": "string", "enum": ["folder", "file"], "description": "種別で絞る（省略で両方）" },
                "limit": { "type": "integer", "minimum": 1, "maximum": MAX_FIND_RESULTS, "description": "返す件数（既定 10）" }
            },
            "required": ["query"]
        })
    }
    fn is_read_only(&self) -> bool {
        true
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let input: FindInput = serde_json::from_value(input)
            .map_err(|e| ToolError::Invalid(format!("入力が不正です: {e}")))?;
        let query = input.query.trim();
        if query.is_empty() {
            return Err(ToolError::Invalid("query を指定してください".into()));
        }
        let kind = match input.kind.as_deref() {
            None => None,
            Some(k) => Some(NodeKind::parse(k).ok_or_else(|| {
                ToolError::Invalid("kind は folder か file を指定してください".into())
            })?),
        };
        let limit = input.limit.unwrap_or(10).clamp(1, MAX_FIND_RESULTS);
        // 種別の絞り込みと件数の上限は検索側（SQL）で効かせる（取ってから捨てない）。
        let hits = match self.drive.find(ctx, query, kind, limit, trace_id).await {
            Ok(nodes) => nodes,
            Err(StorageError::Forbidden | StorageError::NotFound) => {
                return Ok(ToolOutcome::error("ドライブを検索する権限がありません。"));
            }
            Err(e) => {
                tracing::warn!(error = %e, "drive.find の検索に失敗");
                return Ok(ToolOutcome::error("ドライブの検索に失敗しました。"));
            }
        };
        if hits.is_empty() {
            return Ok(ToolOutcome::ok(format!(
                "「{query}」に一致する{}は見つかりませんでした。",
                kind.map_or("ファイル・フォルダ", kind_label)
            )));
        }
        // 場所は並列に引く（件数は上限 20）。読めない・消えた場合は場所を省く。
        let paths =
            futures::future::join_all(hits.iter().map(|n| self.drive.path(ctx, n.id, trace_id)))
                .await;
        let lines = hits
            .iter()
            .zip(paths)
            .map(|(node, path)| {
                let place = path
                    .ok()
                    .filter(|p| !p.is_empty())
                    .map(|p| format!(" 場所: {}", path_text(&p)))
                    .unwrap_or_default();
                format!(
                    "- [{}] {} (id: {}){place}",
                    kind_label(node.kind),
                    node.name,
                    node.id
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolOutcome::ok(format!(
            "「{query}」の検索結果（{} 件）:\n{lines}\n",
            hits.len()
        )))
    }
}

/// ファイル・フォルダを移動・名前変更するツール（承認ゲート対象）。
pub struct DriveMoveTool {
    drive: Arc<dyn DriveOps>,
}

impl DriveMoveTool {
    pub fn new(drive: Arc<dyn DriveOps>) -> Self {
        DriveMoveTool { drive }
    }
}

#[derive(Debug, Deserialize)]
struct MoveInput {
    /// 対象のファイル・フォルダの id。
    node_id: Uuid,
    /// 移動先フォルダの id（`"root"` でマイドライブ直下・省略で移動しない）。
    #[serde(default)]
    destination_folder_id: Option<String>,
    /// 新しい名前（省略で変えない・ファイルの拡張子は変えられない）。
    #[serde(default)]
    new_name: Option<String>,
}

/// 移動先の指定（`None`=移動しない／`Some(None)`=ルート）。
#[allow(clippy::option_option)]
fn parse_destination(raw: Option<&str>) -> Result<Option<Option<Uuid>>, ToolError> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some("root") => Ok(Some(None)),
        Some(s) => Uuid::parse_str(s).map(|id| Some(Some(id))).map_err(|_| {
            ToolError::Invalid(
                "destination_folder_id はフォルダの id か \"root\" を指定してください".into(),
            )
        }),
    }
}

/// 移動・名前変更の失敗をモデルが読む観測へ写す（権限不足と未検出は同じ文言・存在秘匿）。
fn move_error(err: &StorageError) -> ToolOutcome {
    let msg = match err {
        StorageError::Forbidden | StorageError::NotFound => {
            "対象または移動先にアクセスできません（存在しないか、移動する権限がありません）。"
                .into()
        }
        StorageError::Conflict => {
            "移動先に同じ名前のものがあります。new_name で別の名前を指定してください。".into()
        }
        StorageError::Invalid(m) => format!("移動できません: {m}"),
        other => {
            tracing::warn!(error = %other, "drive.move に失敗");
            "移動に失敗しました。".into()
        }
    };
    ToolOutcome::error(msg)
}

#[async_trait::async_trait]
impl Tool for DriveMoveTool {
    fn name(&self) -> &str {
        ToolName::DriveMove.as_str()
    }
    fn description(&self) -> &'static str {
        "ドライブのファイル・フォルダを別のフォルダへ移動する（名前の変更も同時にできる）。\
         ノート・Word・Excel・スライド・CSV・フォルダなど種別を問わない。移動先のフォルダ id は\
         drive.find で調べる。マイドライブ直下へ移すときは destination_folder_id に \"root\" を渡す。\
         ファイルの拡張子は変えられない。"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "node_id": { "type": "string", "format": "uuid", "description": "移動するファイル・フォルダの id" },
                "destination_folder_id": { "type": "string", "description": "移動先フォルダの id。マイドライブ直下は \"root\"。省略すると移動せず名前だけ変える" },
                "new_name": { "type": "string", "description": "新しい名前（省略で変えない）" }
            },
            "required": ["node_id"]
        })
    }
    /// 既存ファイルの置き場所を変える（共有・RAG の可視範囲が変わる）ため承認ゲート対象。
    fn requires_confirmation(&self) -> bool {
        true
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let input: MoveInput = serde_json::from_value(input)
            .map_err(|e| ToolError::Invalid(format!("入力が不正です: {e}")))?;
        let destination = parse_destination(input.destination_folder_id.as_deref())?;
        let new_name = input
            .new_name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty());
        if destination.is_none() && new_name.is_none() {
            return Err(ToolError::Invalid(
                "destination_folder_id か new_name のどちらかを指定してください".into(),
            ));
        }
        // 種別と今の名前は自身のパンくず（viewer 必須）から、移動先の表示名は移動先のパンくずから
        // 得る（互いに独立なので並列に引く・実際の権限判定は update が editor で行う）。
        let dest_folder = destination.flatten();
        let (current, dest_path) =
            futures::join!(self.drive.path(ctx, input.node_id, trace_id), async {
                match dest_folder {
                    Some(folder) => Some(self.drive.path(ctx, folder, trace_id).await),
                    None => None,
                }
            });
        let current = match current {
            Ok(path) => match path.last() {
                Some(c) if c.id == input.node_id => c.clone(),
                _ => return Ok(move_error(&StorageError::NotFound)),
            },
            Err(e) => return Ok(move_error(&e)),
        };
        let dest_label = match (destination, dest_path) {
            (None, _) => None,
            (Some(None), _) | (Some(Some(_)), None) => Some("マイドライブ（ルート）".to_string()),
            (Some(Some(_)), Some(path)) => match path {
                Ok(path) if path.last().is_some_and(|c| c.kind == NodeKind::Folder) => {
                    Some(path_text(&path))
                }
                Ok(_) => {
                    return Ok(ToolOutcome::error(
                        "destination_folder_id がフォルダではありません。",
                    ));
                }
                Err(e) => return Ok(move_error(&e)),
            },
        };
        // 今と同じ名前への変更は変更なしとして扱う（版と更新日時を無駄に進めない）。
        let new_name = new_name.filter(|n| *n != current.name);
        if new_name.is_none() && destination.is_none() {
            return Ok(ToolOutcome::error(
                "変更がありません（名前が今と同じです）。別の名前か移動先を指定してください。",
            ));
        }
        let node = match self
            .drive
            .update(
                ctx,
                current.kind,
                input.node_id,
                new_name,
                destination,
                trace_id,
            )
            .await
        {
            Ok(node) => node,
            Err(e) => return Ok(move_error(&e)),
        };
        let done = match (&dest_label, new_name) {
            (Some(dest), None) => format!("「{dest}」へ移動しました。"),
            (Some(dest), Some(_)) => {
                format!("「{dest}」へ移動し、「{}」に名前を変えました。", node.name)
            }
            (None, _) => format!("「{}」に名前を変えました。", node.name),
        };
        let mut outcome = ToolOutcome::ok(format!(
            "{}「{}」を{done}",
            kind_label(node.kind),
            current.name
        ));
        // ファイルは参照カードで開けるようにする（フォルダはカードにしない）。
        if node.kind == NodeKind::File {
            outcome
                .document_refs
                .push(crate::document_ref::moved_payload(
                    node.id,
                    &node.name,
                    dest_label.as_deref(),
                ));
        }
        Ok(outcome)
    }
}

#[cfg(test)]
#[path = "drive_tool/tests.rs"]
mod tests;

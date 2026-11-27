//! フォルダ操作＋階層 API（Task 1.5）。
//!
//! フォルダの作成/リネーム/移動/削除、子一覧（権限フィルタ済みページング・PIT-13）、
//! パンくず（祖先列）を提供する。全操作は StorageService 経由で認可・監査・closure 整合を担う
//! （単一チョークポイント）。move は closure をサブツリーごと張り替え、循環移動を 400 で拒否する。

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use chrono::{DateTime, Utc};
use storage::service::PurgeOutcome;
use storage::trash_retention::TrashRetentionSetting;

use crate::{
    error::ApiError,
    error_body::{to_body, PurgeRefusalBody, PurgeRefusalCode},
    extract::{AuthContextExt, TraceIdExt},
    routes::files::NodeResponse,
    state::AppState,
};

/// フォルダ作成リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateFolderRequest {
    /// 配置先フォルダ。未指定は org ルート直下。
    pub parent_id: Option<Uuid>,
    pub name: String,
}

/// フォルダのリネーム/移動リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateFolderRequest {
    /// 新しい名前（指定時にリネーム）。
    pub name: Option<String>,
    /// 移動先フォルダ。`null` 明示は「ルートへ移動」、省略は「移動しない」。
    #[serde(default, deserialize_with = "crate::routes::files::double_option")]
    pub parent_id: Option<Option<Uuid>>,
}

/// 子一覧の並び替えキー（クエリ値）。keyset カーソルへサーバ側で織り込む。
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SortField {
    #[default]
    Name,
    Updated,
    Size,
}

impl SortField {
    fn to_key(&self) -> storage::ChildSortKey {
        match self {
            SortField::Name => storage::ChildSortKey::Name,
            SortField::Updated => storage::ChildSortKey::Updated,
            SortField::Size => storage::ChildSortKey::Size,
        }
    }
}

/// 子一覧のクエリ（親・並び順・カーソル・件数）。
#[derive(Debug, Deserialize, IntoParams)]
pub struct ChildrenQuery {
    /// 親フォルダ。未指定は org ルート直下。
    pub parent_id: Option<Uuid>,
    /// 並び替えキー（name|updated|size。既定 name）。
    #[serde(default)]
    pub sort: SortField,
    /// 降順にするか（既定 false=昇順）。
    #[serde(default)]
    pub desc: bool,
    /// 前回応答の `next_cursor`。続きから取得する（省略で先頭）。同じ親（検索なら同じ `q`）・同じ並びの応答の値だけを
    /// 使える。読めない値（別の親・別の並び・前の版の位置を含む）は 400（`cursor_invalid`）。
    pub cursor: Option<String>,
    /// 1 ページの最大件数（1..=100。既定 50）。
    pub limit: Option<usize>,
    /// 名前検索（フォルダ横断・空白区切り語の AND 部分一致）。指定時は `parent_id` を無視する。
    pub q: Option<String>,
}

/// 名前の完全一致の引き当てのクエリ（親・名前・BUG-199）。
#[derive(Debug, Deserialize, IntoParams)]
pub struct ChildByNameQuery {
    /// 親フォルダ。未指定は org ルート直下。
    pub parent_id: Option<Uuid>,
    /// 名前（完全一致・大文字と小文字を区別する）。
    pub name: String,
}

/// 名前の完全一致の引き当ての結果。
#[derive(Debug, Serialize, ToSchema)]
pub struct ChildByNameResponse {
    /// 同じフォルダにある、その名前の見えるノード（無い・見えなければ `null`）。
    pub node: Option<NodeResponse>,
}

/// ゴミ箱一覧のクエリ（カーソル・件数）。
#[derive(Debug, Deserialize, IntoParams)]
pub struct PageQuery {
    /// 前回応答の `next_cursor`。続きから取得する（省略で先頭）。
    pub cursor: Option<String>,
    /// 1 ページの最大件数（1..=100。既定 50）。
    pub limit: Option<usize>,
}

/// 子一覧の 1 ページ（権限フィルタ済み）。
#[derive(Debug, Serialize, ToSchema)]
pub struct ChildrenResponse {
    pub items: Vec<NodeResponse>,
    /// 続きがあれば次回 `cursor` に渡す値（末尾なら `null`）。テナントに束縛した不透明な文字列。`items` が `limit` より
    /// 少ないか空でも付くことがある（サーバーが OpenFGA に問う数を抑えるため・BUG-162・BUG-187）。`null` になるまで
    /// 続きを読む。
    pub next_cursor: Option<String>,
}

/// NodeResponse 群の `updated_by` を表示名で補完する（Task 11P.10）。
/// ディレクトリ（テナント＋org スコープ）で一括解決し、未登録 subject（AI 等）は null のまま。
/// 解決失敗は非致命（クライアントがフォールバック表示する）。
async fn resolve_updated_by_names(
    state: &AppState,
    ctx: &authz::AuthContext,
    items: &mut [NodeResponse],
) {
    let ids: Vec<String> = items.iter().map(|n| n.updated_by.clone()).collect();
    if let Ok(names) = state.directory.resolve_display_names(ctx, &ids).await {
        for n in &mut *items {
            n.updated_by_name = names.get(&n.updated_by).cloned();
        }
    }
}

/// パンくず 1 要素（祖先ノード）。
#[derive(Debug, Serialize, ToSchema)]
pub struct CrumbResponse {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
}

/// フォルダを作成する。
#[utoipa::path(
    post,
    path = "/folders",
    request_body = CreateFolderRequest,
    responses(
        (status = 200, description = "作成したフォルダ", body = NodeResponse),
        (status = 400, description = "不正なリクエスト"),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "親フォルダが無い"),
        (status = 409, description = "同名衝突"),
    ),
    security(("session" = [])),
)]
pub async fn create_folder(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<CreateFolderRequest>,
) -> Result<Json<NodeResponse>, ApiError> {
    let node = state
        .storage
        .create_folder(&ctx, req.parent_id, &req.name, trace.as_deref())
        .await?;
    Ok(Json(node.into()))
}

/// フォルダ/ルートの子を権限フィルタ済みで 1 ページ返す。
/// `q` 指定時はフォルダ横断の名前検索（同じく権限フィルタ済み）になる。
///
/// 見えないものが多い場所（組織の最上位・名前の検索）では、1 ページが `limit` より少ないか空のまま `next_cursor`
/// だけが付いて返ることがある（サーバーが OpenFGA に問う数を抑えるため・BUG-187）。`next_cursor` が無くなるまで
/// 続きを読む。`next_cursor` はテナント・一覧（親か検索の語）・並びに束縛した不透明な文字列（見えないものの名前を
/// 含む位置を外へ出さない）。
#[utoipa::path(
    get,
    path = "/nodes",
    params(ChildrenQuery),
    responses(
        (status = 200, description = "子一覧（読めるもののみ）", body = ChildrenResponse),
        (status = 400, description = "cursor が読めない（cursor_invalid）"),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "親フォルダが無い"),
    ),
    security(("session" = [])),
)]
pub async fn list_children(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Query(q): Query<ChildrenQuery>,
) -> Result<Json<ChildrenResponse>, ApiError> {
    let sort = storage::ChildSort {
        key: q.sort.to_key(),
        desc: q.desc,
    };
    let limit = q.limit.unwrap_or(50);
    let name_query = q.q.as_deref().map(str::trim).filter(|s| !s.is_empty());
    // 続きの位置は見えないノードの名前を含みうるので、テナント・一覧・並びに束縛して封をする（BUG-187）。
    let (kind, scope) = match name_query {
        Some(text) => (data::ListCursorKind::DriveSearch, sort.cursor_scope(text)),
        None => (
            data::ListCursorKind::DriveFolder,
            sort.cursor_scope(&q.parent_id.map_or_else(|| "root".into(), |p| p.to_string())),
        ),
    };
    let cursor = q
        .cursor
        .as_deref()
        .filter(|c| !c.is_empty())
        .map(|c| {
            state
                .data
                .open_list_cursor(kind, ctx.tenant_id(), &scope, c)
        })
        .transpose()?;
    let page = match name_query {
        Some(name_query) => {
            state
                .storage
                .search_nodes_by_name(
                    &ctx,
                    name_query,
                    sort,
                    cursor.as_deref(),
                    limit,
                    trace.as_deref(),
                )
                .await?
        }
        None => {
            state
                .storage
                .list_children(
                    &ctx,
                    q.parent_id,
                    sort,
                    cursor.as_deref(),
                    limit,
                    // ドライブ UI にシステム領域（エージェントのワークスペース）は出さない（#392）。
                    false,
                    trace.as_deref(),
                )
                .await?
        }
    };
    let next_cursor = page
        .next_cursor
        .map(|c| {
            state
                .data
                .seal_list_cursor(kind, ctx.tenant_id(), &scope, &c)
        })
        .transpose()?;
    let mut items: Vec<NodeResponse> = page.items.into_iter().map(Into::into).collect();
    resolve_updated_by_names(&state, &ctx, &mut items).await;
    Ok(Json(ChildrenResponse { items, next_cursor }))
}

/// フォルダ（未指定はルート）の中の、名前が完全に一致するノードを 1 件返す（アップロードの前の同じ名前の確かめ・
/// BUG-199）。
///
/// 名前の検索（`GET /nodes?q=`）の先頭のページは、見えるものが多い人では 0 件で `next_cursor` だけを返すことがあるので、
/// 同じ名前の確かめには使えない。同じフォルダの名前は一意なので、ここは高々 1 件を引き、OpenFGA に問うのも高々 1 回。
/// 見えないノードは `null`（その名前で保存すると、確定の 409 で同じ名前があると分かる）。
#[utoipa::path(
    get,
    path = "/nodes/by-name",
    params(ChildByNameQuery),
    responses(
        (status = 200, description = "その名前の見えるノード（無ければ node が null）", body = ChildByNameResponse),
        (status = 400, description = "name が無い"),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "親フォルダが無い"),
    ),
    security(("session" = [])),
)]
pub async fn find_child_by_name(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Query(q): Query<ChildByNameQuery>,
) -> Result<Json<ChildByNameResponse>, ApiError> {
    let node = state
        .storage
        .find_child_by_name(&ctx, q.parent_id, &q.name, trace.as_deref())
        .await?;
    let mut items: Vec<NodeResponse> = node.into_iter().map(Into::into).collect();
    resolve_updated_by_names(&state, &ctx, &mut items).await;
    Ok(Json(ChildByNameResponse { node: items.pop() }))
}

/// ノードのパンくず（root→自身。読める接頭辞のみ）。
#[utoipa::path(
    get,
    path = "/nodes/{id}/breadcrumb",
    params(("id" = Uuid, Path, description = "ノード ID")),
    responses(
        (status = 200, description = "祖先列（root が先頭）", body = [CrumbResponse]),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "ノードが無い"),
    ),
    security(("session" = [])),
)]
pub async fn breadcrumb(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<CrumbResponse>>, ApiError> {
    let crumbs = state.storage.breadcrumb(&ctx, id, trace.as_deref()).await?;
    Ok(Json(
        crumbs
            .into_iter()
            .map(|c| CrumbResponse {
                id: c.id,
                name: c.name,
                kind: c.kind.as_str().to_string(),
            })
            .collect(),
    ))
}

/// フォルダのリネーム・移動。
#[utoipa::path(
    patch,
    path = "/folders/{id}",
    params(("id" = Uuid, Path, description = "フォルダ ID")),
    request_body = UpdateFolderRequest,
    responses(
        (status = 200, description = "更新後のフォルダ", body = NodeResponse),
        (status = 400, description = "不正なリクエスト（循環移動など）"),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "フォルダが無い"),
        (status = 409, description = "同名衝突"),
    ),
    security(("session" = [])),
)]
pub async fn update_folder(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateFolderRequest>,
) -> Result<Json<NodeResponse>, ApiError> {
    let node = state
        .storage
        .update_folder(
            &ctx,
            id,
            req.name.as_deref(),
            req.parent_id,
            trace.as_deref(),
        )
        .await?;
    Ok(Json(node.into()))
}

/// フォルダをサブツリーごと論理削除する（ゴミ箱へ）。
#[utoipa::path(
    delete,
    path = "/folders/{id}",
    params(("id" = Uuid, Path, description = "フォルダ ID")),
    responses(
        (status = 204, description = "削除済み"),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "フォルダが無い"),
    ),
    security(("session" = [])),
)]
pub async fn delete_folder(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    state
        .storage
        .soft_delete_folder(&ctx, id, trace.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// ゴミ箱からのフォルダ復元（サブツリーを同時復元）。
#[utoipa::path(
    post,
    path = "/folders/{id}/restore",
    params(("id" = Uuid, Path, description = "フォルダ ID")),
    responses(
        (status = 200, description = "復元したフォルダ", body = NodeResponse),
        (status = 400, description = "祖先が削除済みで復元不可"),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "フォルダが無い"),
        (status = 409, description = "同名衝突"),
    ),
    security(("session" = [])),
)]
pub async fn restore_folder(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<NodeResponse>, ApiError> {
    let node = state
        .storage
        .restore_folder(&ctx, id, trace.as_deref())
        .await?;
    Ok(Json(node.into()))
}

/// ゴミ箱の 1 項目（削除の根ノード ＋ 完全削除の添え物・GAP-108b）。
#[derive(Debug, Serialize, ToSchema)]
pub struct TrashItemResponse {
    #[serde(flatten)]
    pub node: NodeResponse,
    /// 呼んだ人が完全に削除できるか（根の持ち主か組織の管理者・D-201）。表示のための値で、削除の口が毎回確かめ直す。
    pub can_purge: bool,
    /// フォルダなら配下のファイルの数（ファイルなら `null`）。完全削除で一緒に消える数を確かめのダイアログに出す。
    pub file_count: Option<i64>,
    /// フォルダなら、配下のファイルのうち呼んだ人が作ったのでないものの数（ファイルなら `null`）。
    pub others_file_count: Option<i64>,
    /// ゴミ箱に入れた時刻。
    pub deleted_at: DateTime<Utc>,
    /// テナントに保持期間があれば、この時刻を過ぎると自動で完全に削除される（保持期間が無ければ `null`・D-202）。
    /// 掃除は中断アップロードの回収と同じ周（既定 1 時間ごと）に走るので、実際に消えるのはこの後の周。
    pub purge_after: Option<DateTime<Utc>>,
}

/// ゴミ箱の 1 ページ。
#[derive(Debug, Serialize, ToSchema)]
pub struct TrashResponse {
    pub items: Vec<TrashItemResponse>,
    /// 続きがあれば次回 `cursor` に渡す値（末尾なら `null`）。
    pub next_cursor: Option<String>,
    /// テナントのゴミ箱の保持日数（ゴミ箱に入れてからこの日数で自動で完全に削除する）。`null` は自動では消さない。
    pub retention_days: Option<i32>,
}

/// 保持期間による自動の完全削除の期限（`greatest(削除した時刻, 猶予の起点) + 日数`）。
///
/// 掃除が消すかを決める式（`storage::trash_retention` の `due_on`・候補の SQL）と同じ。Postgres の `greatest` は
/// null を無視するので、猶予の起点が無ければ削除した時刻から数える。
fn purge_after(
    deleted_at: DateTime<Utc>,
    setting: Option<&TrashRetentionSetting>,
) -> Option<DateTime<Utc>> {
    let setting = setting?;
    let days = setting.days?;
    let start = setting
        .since
        .map_or(deleted_at, |since| since.max(deleted_at));
    Some(start + chrono::Duration::days(i64::from(days)))
}

/// ゴミ箱の中身（削除の根ノード）を新しい順に 1 ページ返す。
#[utoipa::path(
    get,
    path = "/trash",
    params(PageQuery),
    responses(
        (status = 200, description = "ゴミ箱の根ノード（復元できるもののみ）", body = TrashResponse),
        (status = 400, description = "cursor が読めない"),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
    ),
    security(("session" = [])),
)]
pub async fn list_trash(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Query(q): Query<PageQuery>,
) -> Result<Json<TrashResponse>, ApiError> {
    let page = state
        .storage
        .list_trash_entries(
            &ctx,
            q.cursor.as_deref(),
            q.limit.unwrap_or(50),
            trace.as_deref(),
        )
        .await?;
    let retention = state
        .storage
        .trash_retention()
        .setting(ctx.tenant_id())
        .await?;
    let mut nodes: Vec<NodeResponse> = page.items.iter().map(|e| e.node.clone().into()).collect();
    resolve_updated_by_names(&state, &ctx, &mut nodes).await;
    let items = nodes
        .into_iter()
        .zip(page.items)
        .map(|(node, e)| {
            // 論理削除は根に deleted_at と updated_at を同じ時刻で書く（無いことはないが、無ければ更新の時刻）。
            let deleted_at = e.node.deleted_at.unwrap_or(e.node.updated_at);
            TrashItemResponse {
                node,
                can_purge: e.can_purge,
                file_count: e.file_count,
                others_file_count: e.others_file_count,
                deleted_at,
                purge_after: purge_after(deleted_at, retention.as_ref()),
            }
        })
        .collect();
    Ok(Json(TrashResponse {
        items,
        next_cursor: page.next_cursor,
        retention_days: retention.and_then(|r| r.days),
    }))
}

/// 完全削除で何も消さなかったときの 409（`code` と、利用者に見せてよい文言・[`PurgeRefusalBody`]）。
fn purge_refused(outcome: &PurgeOutcome) -> Option<ApiError> {
    let code = match outcome {
        PurgeOutcome::Purged(_) => return None,
        PurgeOutcome::NotInTrash => PurgeRefusalCode::NotInTrash,
        PurgeOutcome::InsideDeletedFolder => PurgeRefusalCode::InsideDeletedFolder,
        PurgeOutcome::HasLiveDescendants => PurgeRefusalCode::LiveDescendants,
    };
    Some(ApiError::ConflictJson(to_body(&PurgeRefusalBody::new(
        code,
    ))))
}

/// ゴミ箱の根（ファイルかフォルダ）を、配下ごと完全に削除する（元に戻せない・GAP-108b・D-201）。
///
/// 消せるのは根の持ち主と組織の管理者。フォルダなら、その下のほかの人のファイルも消える。消したファイルを指して
/// いた参照（ワークフロー・チャットの引用・レコードの添付）は見つからなくなる。
#[utoipa::path(
    delete,
    path = "/trash/{id}",
    params(("id" = Uuid, Path, description = "ゴミ箱の根のノード ID")),
    responses(
        (status = 204, description = "完全に削除した"),
        (status = 401, description = "未認証"),
        (status = 403, description = "持ち主でも組織の管理者でもない"),
        (status = 404, description = "無い（システム領域・ほかの組織を含む）"),
        (status = 409, description = "消さなかった（code で理由を返す）", body = PurgeRefusalBody),
    ),
    security(("session" = [])),
)]
pub async fn purge_trash_item(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let outcome = state.storage.purge_node(&ctx, id, trace.as_deref()).await?;
    match purge_refused(&outcome) {
        Some(err) => Err(err),
        None => Ok(StatusCode::NO_CONTENT),
    }
}

/// 「ゴミ箱を空にする」のリクエスト。
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct EmptyTrashRequest {
    /// 前回の応答の `next_cursor`（省略で先頭から）。
    #[serde(default)]
    pub cursor: Option<String>,
}

/// 「ゴミ箱を空にする」1 回の結果。
#[derive(Debug, Serialize, ToSchema)]
pub struct EmptyTrashResponse {
    /// 完全に削除した根の数。
    pub purged: usize,
    /// 一覧に出ているが残した根の数（持ち主か管理者だけが削除できる・中に削除の後で作られたものがある）。
    pub skipped: usize,
    /// 続きがあれば、次の呼び出しの `cursor` に渡す値（`null` になるまで繰り返す）。
    pub next_cursor: Option<String>,
}

/// 呼んだ人のゴミ箱の一覧に出ている根のうち、呼んだ人が消せるものを古い順に完全に削除する（GAP-108b・D-201）。
///
/// 1 回に消す数に上限があり、続きがあれば `next_cursor` を返す。一覧に出ない他人の根は、管理者が呼んでも消さない。
#[utoipa::path(
    post,
    path = "/trash/empty",
    request_body = EmptyTrashRequest,
    responses(
        (status = 200, description = "消した数・残した数・続きの位置", body = EmptyTrashResponse),
        (status = 400, description = "cursor が読めない"),
        (status = 401, description = "未認証"),
        (status = 403, description = "組織のメンバーでない"),
    ),
    security(("session" = [])),
)]
pub async fn empty_trash(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<EmptyTrashRequest>,
) -> Result<Json<EmptyTrashResponse>, ApiError> {
    let cursor = req.cursor.as_deref().filter(|c| !c.is_empty());
    let out = state
        .storage
        .empty_trash(&ctx, cursor, trace.as_deref())
        .await?;
    Ok(Json(EmptyTrashResponse {
        purged: out.purged,
        skipped: out.skipped,
        next_cursor: out.next_cursor,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 消さなかった理由は、それぞれの code を持つ 409 になる（画面が文言を出し分ける）。
    #[test]
    fn purge_refusals_map_to_coded_conflicts() {
        for (outcome, code) in [
            (PurgeOutcome::NotInTrash, "not_in_trash"),
            (PurgeOutcome::InsideDeletedFolder, "inside_deleted_folder"),
            (PurgeOutcome::HasLiveDescendants, "live_descendants"),
        ] {
            match purge_refused(&outcome) {
                Some(ApiError::ConflictJson(body)) => {
                    assert_eq!(body["code"], code);
                    assert_eq!(body["status"], 409);
                }
                other => panic!("{code}: {other:?}"),
            }
        }
        let report = storage::service::PurgeReport {
            kind: storage::NodeKind::File,
            nodes: 1,
            files: 1,
            versions: 1,
            freed_bytes: 0,
        };
        assert!(purge_refused(&PurgeOutcome::Purged(report)).is_none());
    }

    /// 自動の完全削除の期限は、削除した時刻と猶予の起点の遅いほうに日数を足した時刻（掃除の式と同じ）。
    #[test]
    fn purge_after_counts_from_later_of_deleted_and_since() {
        let t = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        let deleted = t("2026-11-01T00:00:00Z");
        let setting = |days, since| TrashRetentionSetting { days, since };
        // 保持期間が無い（設定の行が無い・日数が null）なら期限は無い。
        assert_eq!(purge_after(deleted, None), None);
        assert_eq!(purge_after(deleted, Some(&setting(None, None))), None);
        // 猶予の起点が無ければ、削除した時刻から数える。
        assert_eq!(
            purge_after(deleted, Some(&setting(Some(30), None))),
            Some(t("2026-12-01T00:00:00Z"))
        );
        // 起点が削除より後（日数を入れる前に削除した）なら起点から、前なら削除した時刻から数える。
        assert_eq!(
            purge_after(
                deleted,
                Some(&setting(Some(30), Some(t("2026-11-10T00:00:00Z"))))
            ),
            Some(t("2026-12-10T00:00:00Z"))
        );
        assert_eq!(
            purge_after(
                deleted,
                Some(&setting(Some(1), Some(t("2026-10-01T00:00:00Z"))))
            ),
            Some(t("2026-11-02T00:00:00Z"))
        );
    }
}

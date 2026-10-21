//! skill カタログの読み出し API（#387）。
//!
//! **モデルが `skill` ツールで見ているカタログと同一の源**（[`crate::skill_catalog::ApiSkillCatalogSource`]
//! ＝インストール済み ∪ 本人 owner・first-party → in-house → own の順）を UI にも配る。
//! 別の一覧ロジックをフロントで組み立てると、補完に出るスキルとモデルが引けるスキルが
//! ずれる（「補完に出たのに呼べない」）。単一ソースを守るための薄いアダプタ。
//!
//! 返すのは name / description / version / スラッシュコマンド宣言まで。**instructions は返さない**
//! （本文は `skill` ツール経由でのみ、発話ユーザー権限で解決される）。

use axum::{
    extract::{Query, State},
    Json,
};
use chat::SkillCatalogSource;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{error::ApiError, extract::AuthContextExt, extract::TraceIdExt, state::AppState};

/// カタログ 1 件（UI 表示・スラッシュコマンド補完用）。
#[derive(Debug, Serialize, ToSchema)]
pub struct SkillCatalogItem {
    pub id: Uuid,
    pub version: i64,
    pub name: String,
    pub description: String,
    /// スラッシュコマンド宣言（未宣言なら null＝コマンドでは起動できない）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<SkillCommandDto>,
}

/// スラッシュコマンド宣言（`gui::SkillCommand` の DTO）。
#[derive(Debug, Serialize, ToSchema)]
pub struct SkillCommandDto {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    pub variants: Vec<SkillCommandVariantDto>,
}

/// 引数プリセット。
#[derive(Debug, Serialize, ToSchema)]
pub struct SkillCommandVariantDto {
    pub args: String,
    pub summary: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SkillCatalogResponse {
    pub skills: Vec<SkillCatalogItem>,
}

/// クエリ。`thread_id` 指定でそのスレッドのピン済み skill もマージする。
#[derive(Debug, Deserialize, IntoParams)]
pub struct CatalogQuery {
    /// スレッド内のコンポーザから引くとき（ピン統合の対象）。
    #[serde(default)]
    pub thread_id: Option<Uuid>,
}

/// スレッドにピン済みの skill をカタログエントリへ写す。
///
/// 認可は既存チョークポイント（`ChatStore::get_thread` の viewer / `ArtifactStore::get_version`
/// の viewer）に委ねる。**読めないピンは黙って落とす**（存在秘匿。ピン自体が見えても
/// 中身が読めないなら候補に出す意味がない）。
async fn thread_pinned_entries(
    state: &AppState,
    ctx: &authz::AuthContext,
    thread_id: Uuid,
    trace_id: Option<&str>,
) -> Result<Vec<chat::SkillCatalogEntry>, ApiError> {
    let Some(chat) = state.chat.as_ref() else {
        return Ok(Vec::new());
    };
    let thread = chat.get_thread(ctx, thread_id, trace_id).await?;
    let mut out = Vec::with_capacity(thread.skill_pins.len());
    for pin in thread.skill_pins {
        let Ok(version) = state
            .artifacts
            .get_version(ctx, pin.skill_id, pin.skill_version, trace_id)
            .await
        else {
            continue;
        };
        let Ok(body) = gui::validate_skill_body(&version.body) else {
            continue;
        };
        let Ok(meta) = state.artifacts.get(ctx, pin.skill_id, trace_id).await else {
            continue;
        };
        out.push(chat::SkillCatalogEntry {
            id: pin.skill_id,
            version: pin.skill_version,
            name: meta.name,
            description: body.description,
            pinned: true,
            command: body.command,
        });
    }
    Ok(out)
}

/// 実行主体から見える skill カタログ（モデルの `skill` ツールと同一の源）。
#[utoipa::path(
    get, path = "/skills/catalog",
    params(CatalogQuery),
    responses((status = 200, description = "カタログ", body = SkillCatalogResponse)),
    security(("session" = [])),
)]
pub async fn list_skill_catalog(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Query(q): Query<CatalogQuery>,
) -> Result<Json<SkillCatalogResponse>, ApiError> {
    let source = crate::skill_catalog::ApiSkillCatalogSource::new(
        state.skill_installs.clone(),
        state.artifacts.clone(),
    );
    let mut entries = source.entries(&ctx, trace.0.as_deref()).await?;
    // スレッド内のコンポーザは**モデルと同じ集合**を見る必要がある。ワーカーの
    // `push_skill_tool` はカタログ源へ run のピンをマージするため、共有で読めてピン済みだが
    // 本人が所有/インストールしていない skill は、ここでマージしないとモデルにだけ見えて
    // 補完には出ない（「モデルと同一のカタログ」という契約が崩れる）。
    if let Some(thread_id) = q.thread_id {
        let pinned = thread_pinned_entries(&state, &ctx, thread_id, trace.0.as_deref()).await?;
        entries = chat::merge_skill_catalog_entries(pinned, entries);
    }
    let skills = entries
        .into_iter()
        .map(|e| SkillCatalogItem {
            id: e.id,
            version: e.version,
            name: e.name,
            description: e.description,
            command: e.command.map(|c| SkillCommandDto {
                name: c.name,
                hint: c.hint,
                variants: c
                    .variants
                    .into_iter()
                    .map(|v| SkillCommandVariantDto {
                        args: v.args,
                        summary: v.summary,
                    })
                    .collect(),
            }),
        })
        .collect();
    Ok(Json(SkillCatalogResponse { skills }))
}

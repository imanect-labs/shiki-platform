//! skill カタログの読み出し API（#387）。
//!
//! **モデルが `skill` ツールで見ているカタログと同一の源**（[`crate::skill_catalog::ApiSkillCatalogSource`]
//! ＝インストール済み ∪ 本人 owner・first-party → in-house → own の順）を UI にも配る。
//! 別の一覧ロジックをフロントで組み立てると、補完に出るスキルとモデルが引けるスキルが
//! ずれる（「補完に出たのに呼べない」）。単一ソースを守るための薄いアダプタ。
//!
//! 返すのは name / description / version / スラッシュコマンド宣言まで。**instructions は返さない**
//! （本文は `skill` ツール経由でのみ、発話ユーザー権限で解決される）。

use axum::{extract::State, Json};
use chat::SkillCatalogSource;
use serde::Serialize;
use utoipa::ToSchema;
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

/// 実行主体から見える skill カタログ（モデルの `skill` ツールと同一の源）。
#[utoipa::path(
    get, path = "/skills/catalog",
    responses((status = 200, description = "カタログ", body = SkillCatalogResponse)),
    security(("session" = [])),
)]
pub async fn list_skill_catalog(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
) -> Result<Json<SkillCatalogResponse>, ApiError> {
    let source = crate::skill_catalog::ApiSkillCatalogSource::new(
        state.skill_installs.clone(),
        state.artifacts.clone(),
    );
    let entries = source.entries(&ctx, trace.0.as_deref()).await?;
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

//! 試運転の下見（会話の AI の試運転〔try_workflow〕の承認カード・D-10・D-197）。
//!
//! `GET /workflows/try-preview?workflow=<UUID>&version=<n>` は、承認カードが持つ道具の入力（`call.input`）から、
//! 固定した版を試運転すると何が起きるかを本人の権限で返す。カードはこの応答だけで描く（TS に対応表を手書きしない）。
//!
//! - **id だけを受ける。** 名前は受けない（400）。承認を待つ間に名前が付け替わると、カードで見たものと別の
//!   ワークフローが走る（TOCTOU）。試運転の道具も id だけを受けるので、カードと道具は同じ id を見る。
//! - **引き方は道具と同じ。** [`workflow_engine::WorkflowStore::resolve_ref`]（viewer・kind=workflow）で引き、
//!   無い・見えない・別のテナント・ワークフローでない・版が無いは、どれも同じ 404 にする。
//! - **試運転できるか**（`can_try`）は、起動の入口（`WorkflowRunLauncher::start_version_as_editor`）が止める判定と
//!   同じ [`artifact::ArtifactStore::allows`]（editor・`HigherConsistency`）で決める。表示のためだけの判定で、拒否の
//!   監査は残さない（ハンドラから OpenFGA を直接問わない）。
//! - **この版を保存した人**（`saved_by`）は版の記録から取る。持ち主（`owner`）と違えば、カードは別に出す
//!   （共同編集者が書いた版を、持ち主の名前で安心させない）。
//! - **副作用**（`effects`）は [`RegistrationService::run_effects`]（純関数）の結果に、本人が見られるテーブルの
//!   名前だけを入れたもの。見られないテーブルは名前を入れない（`target_id` があって `target_name` が無い）。
//!   `workflow.start` の起動先は名前で本人が見られるものを引き、`starts` に id を添える。名前は表示のためだけに
//!   使い、権限の判定には使わない（実行のときは run の中で改めて引く）。フォルダ・ファイルの名前はここでは引かない。

use std::collections::HashMap;

use axum::{
    extract::{Query, State},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;
use workflow_engine::registration::{EffectTarget, RunEffects};
use workflow_engine::{RegistrationService, WorkflowIr};

use crate::{
    error::ApiError,
    error_body::{to_body, BadRequestBody},
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// 下見の問い（どちらも必須）。
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TryPreviewQuery {
    /// ワークフローの id（UUID だけ・名前は受けない）。
    #[param(required = true, value_type = Uuid)]
    pub workflow: Option<String>,
    /// 試運転する版（必須・省略は 400）。
    #[param(required = true, value_type = i64)]
    pub version: Option<String>,
}

/// `workflow.start` の起動先 1 つ（同じ名前は 1 つ・IR の順）。
#[derive(Debug, Serialize, ToSchema)]
pub struct TryPreviewStart {
    /// IR に書いた起動先の名前（`effects.starts` の `workflow_name`）。
    pub name: String,
    /// その名前で本人が見られるワークフローの id（無い・見えないなら null）。表示のためだけに使う。
    pub workflow_id: Option<Uuid>,
}

/// 試運転の下見。
#[derive(Debug, Serialize, ToSchema)]
pub struct TryPreviewResponse {
    /// ワークフローの id（試運転の道具が受ける値と同じ・カードに大きく出す）。
    pub workflow_id: Uuid,
    /// 参照名（テナントの中で一意・作成者が書いた文字）。
    pub name: String,
    /// 試運転する版。
    pub version: i64,
    /// いまの最新の版（`version` より新しい版があればカードが知らせる）。
    pub latest_version: i64,
    /// 持ち主（subject の id）。
    pub owner: String,
    /// 持ち主が本人か。
    pub owned_by_me: bool,
    /// この版を保存した人（subject の id）。持ち主と違うことがある（共同編集者）。
    pub saved_by: String,
    /// この版を保存した時刻。
    pub saved_at: DateTime<Utc>,
    /// この版を保存したのが本人か。
    pub saved_by_me: bool,
    /// 本人がこのワークフローを試運転できるか（editor 以上・D-197）。閲覧だけ共有されたものは false で、承認しても
    /// 「閲覧のみのワークフローは試運転できません」で返る。表示のためだけの判定（起動の入口が改めて確かめる）。
    pub can_try: bool,
    /// 実行すると起きること。テーブルの `target_name` は本人が見られるものだけ入れる（`target_id` があって
    /// `target_name` が無ければ見られないテーブル）。フォルダ・ファイルの名前は入れない。
    pub effects: RunEffects,
    /// `workflow.start` の起動先（名前がリテラルのものだけ）。
    pub starts: Vec<TryPreviewStart>,
}

/// 固定した版を試運転すると何が起きるかを返す（承認カード用・保存も実行もしない）。
#[utoipa::path(
    get,
    path = "/workflows/try-preview",
    params(TryPreviewQuery),
    responses(
        (status = 200, description = "試運転の下見", body = TryPreviewResponse),
        (status = 400, description = "workflow が無いか UUID でない（名前は受けない）・version が無いか整数でない（invalid_input・field つき）", body = crate::error_body::BadRequestBody),
        (status = 401, description = "未認証"),
        (status = 404, description = "無い・見えない・ワークフローでない・版が無い（区別しない）"),
    ),
    tag = "workflows"
)]
pub async fn try_preview(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Query(q): Query<TryPreviewQuery>,
) -> Result<Json<TryPreviewResponse>, ApiError> {
    let (id, version) = parse_query(&q)?;
    let trace = trace.as_deref();
    // 道具と同じ引き方（viewer・kind=workflow・見えないと無いは同じ 404）。
    let meta = state
        .workflows
        .resolve_ref(&ctx, &id.to_string(), trace)
        .await
        .map_err(super::save::map_store_err)?;
    // 版の本文と保存した人を 1 回で読む（viewer）。上の確かめの後に権限が外れたときも、在否を明かさない。
    let saved = state
        .artifacts
        .get_version(&ctx, id, version, trace)
        .await
        .map_err(|e| match e {
            artifact::ArtifactError::Forbidden => ApiError::NotFound,
            other => other.into(),
        })?;
    let ir = WorkflowIr::from_json(&saved.body)
        .map_err(|e| ApiError::Internal(format!("保存済みの IR を読めません: {e}")))?;
    let can_try = state
        .artifacts
        .allows(&ctx, id, authz::Relation::Editor)
        .await?;
    let mut effects = RegistrationService::run_effects(&ir);
    name_tables(&state, &ctx, &mut effects, trace).await?;
    let starts = start_targets(&state, &ctx, &effects, trace).await?;
    let me = ctx.principal().id.as_str();
    Ok(Json(TryPreviewResponse {
        workflow_id: meta.id,
        owned_by_me: meta.owner == me,
        saved_by_me: saved.created_by == me,
        name: meta.name,
        version: saved.version,
        latest_version: meta.current_version,
        owner: meta.owner,
        saved_by: saved.created_by,
        saved_at: saved.created_at,
        can_try,
        effects,
        starts,
    }))
}

/// 問いを読む（`workflow` は UUID だけ・`version` は整数）。誤りは欄つきの 400。
fn parse_query(q: &TryPreviewQuery) -> Result<(Uuid, i64), ApiError> {
    let invalid = |message: &str, field: &str| {
        ApiError::UnprocessableJson(to_body(&BadRequestBody::new(
            data::InvalidCode::InvalidInput,
            message,
            Some(field),
        )))
    };
    let workflow = q.workflow.as_deref().map(str::trim).unwrap_or_default();
    if workflow.is_empty() {
        return Err(invalid(
            "ワークフローの id を指定してください。",
            "workflow",
        ));
    }
    let id = Uuid::parse_str(workflow).map_err(|_| {
        invalid(
            "ワークフローは id（UUID）で指定してください。名前では試運転できません。",
            "workflow",
        )
    })?;
    let version = q.version.as_deref().map(str::trim).unwrap_or_default();
    if version.is_empty() {
        return Err(invalid("試運転する版を指定してください。", "version"));
    }
    let version = version
        .parse::<i64>()
        .ok()
        .filter(|v| *v >= 1)
        .ok_or_else(|| invalid("版は 1 以上の整数で指定してください。", "version"))?;
    Ok((id, version))
}

/// 書く先のテーブルの名前を、本人の見え方（閲覧者に見せるテーブル）で入れる。見られない・無いテーブルは入れない。
async fn name_tables(
    state: &AppState,
    ctx: &authz::AuthContext,
    effects: &mut RunEffects,
    trace: Option<&str>,
) -> Result<(), ApiError> {
    let ids: Vec<String> = effects
        .target_ids(EffectTarget::Table)
        .into_iter()
        .map(str::to_string)
        .collect();
    let mut names = HashMap::new();
    for literal in ids {
        let Ok(table_id) = Uuid::parse_str(literal.trim()) else {
            continue;
        };
        match state.data.get_table_for_viewer(ctx, table_id, trace).await {
            Ok(table) => {
                names.insert(literal, table.name);
            }
            Err(data::DataError::NotFound | data::DataError::Forbidden) => {}
            Err(e) => return Err(e.into()),
        }
    }
    effects.set_target_names(EffectTarget::Table, &names);
    Ok(())
}

/// `workflow.start` の起動先を、名前で本人が見られるワークフローに引く（同じ名前は 1 回・IR の順）。
async fn start_targets(
    state: &AppState,
    ctx: &authz::AuthContext,
    effects: &RunEffects,
    trace: Option<&str>,
) -> Result<Vec<TryPreviewStart>, ApiError> {
    let mut out: Vec<TryPreviewStart> = Vec::new();
    for name in effects
        .starts
        .iter()
        .filter_map(|s| s.workflow_name.as_ref())
    {
        if out.iter().any(|s| &s.name == name) {
            continue;
        }
        let workflow_id = match state
            .artifacts
            .get_by_name(ctx, artifact::ArtifactKind::Workflow, name, trace)
            .await
        {
            Ok(a) => Some(a.id),
            Err(artifact::ArtifactError::NotFound | artifact::ArtifactError::Forbidden) => None,
            Err(e) => return Err(e.into()),
        };
        out.push(TryPreviewStart {
            name: name.clone(),
            workflow_id,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(workflow: Option<&str>, version: Option<&str>) -> TryPreviewQuery {
        TryPreviewQuery {
            workflow: workflow.map(str::to_string),
            version: version.map(str::to_string),
        }
    }

    /// 誤りの欄（構造化 400 の `field`・ほかの誤りは `None`）。
    fn field_of(err: ApiError) -> Option<String> {
        match err {
            ApiError::UnprocessableJson(body) if body["code"] == "invalid_input" => {
                body["field"].as_str().map(str::to_string)
            }
            _ => None,
        }
    }

    #[test]
    fn reads_an_id_and_a_version() {
        let id = Uuid::new_v4();
        let got = parse_query(&q(Some(&format!(" {id} ")), Some(" 3 "))).unwrap();
        assert_eq!(got, (id, 3));
        // 大文字の UUID も同じ id（道具と同じ Uuid::parse_str）。
        let upper = id.to_string().to_uppercase();
        assert_eq!(parse_query(&q(Some(&upper), Some("1"))).unwrap().0, id);
    }

    #[test]
    fn rejects_names_and_missing_or_bad_values() {
        let id = Uuid::new_v4().to_string();
        for (workflow, version, field) in [
            (None, Some("1"), "workflow"),
            (Some("  "), Some("1"), "workflow"),
            (Some("my-flow"), Some("1"), "workflow"),
            (Some(id.as_str()), None, "version"),
            (Some(id.as_str()), Some(""), "version"),
            (Some(id.as_str()), Some("latest"), "version"),
            (Some(id.as_str()), Some("0"), "version"),
            (Some(id.as_str()), Some("-2"), "version"),
        ] {
            let err = parse_query(&q(workflow, version)).unwrap_err();
            assert_eq!(
                field_of(err).as_deref(),
                Some(field),
                "{workflow:?} {version:?}"
            );
        }
    }
}

//! 有効化・同意・トリガ実体化 API（Task 10.4a 残・engine.md §10）。
//!
//! - GET registration — 有効化状態＋委譲一覧（再同意バナー用）＋いま見送り続けている自動実行（GAP-67）
//! - GET consent-plan — 同意画面の提案 grants（IR 静的分析＋secret 名→id 解決）
//! - POST enable — 明示委譲付き有効化（scope 拡大で grants 不足なら 409 missing_scopes。同意の画面は
//!   `consent: true` で送り、grants が空でも同意し直す・BUG-102）
//! - POST disable — トリガ停止（`revoke_delegations: true` なら委譲もすべて取り消す・D-90）
//!
//! authz: 参照系は artifact viewer（`WorkflowStore` 経由の取得が担保）、enable/disable は
//! artifact **editor** を明示 check（閲覧者による有効化 = 権限行使の踏み台を防ぐ）。
//! 委譲範囲の検証は `DelegationStore` が fail-closed で行う。監査は 1 操作 1 行。

use axum::{
    extract::{Path, State},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use storage::audit::AuditAction;
use storage::audit::{AuditEntry, Decision};
use utoipa::ToSchema;
use uuid::Uuid;
use workflow_engine::{EnableError, GrantRequest, RegistrationService, Revocation, WorkflowIr};

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// 委譲 1 件の表示。
#[derive(Debug, Serialize, ToSchema)]
pub struct DelegationItem {
    pub delegator: String,
    pub scope: String,
    /// FGA オブジェクト参照（例: `folder:<tenant>|<id>`）。
    pub object_ref: String,
    /// 対象の種類（`GrantItem::object_type` と同じ語・folder / file / secret / workflow / data_table）。
    /// 台帳の参照が読めないときは null（画面は「名前を読み込めない対象」と出す）。
    pub object_kind: Option<String>,
    /// 対象の id（テナントの前置きを外したもの・名前を引くのに使う）。
    pub object_id: Option<String>,
    pub relation: String,
    pub granted_at: String,
}

/// いま見送り続けている自動実行（GAP-67）。
///
/// 自動実行（スケジュール・ファイルやテーブルの変更）の起動を、定義を読めない・オンにした人の権限が外れた・実行の
/// 前に止める設定がある、のどれかで見送ったとき。自動実行が動くか、オンにし直す・オフにすると消える。
#[derive(Debug, Serialize, ToSchema)]
pub struct LaunchSkipItem {
    /// 理由の code（definition_unreadable / delegation_invalid / not_ready）。
    pub code: String,
    /// 作成者向けの理由の文（内部の文・ほかの人の情報を含まない）。
    pub message: String,
    /// 直し方の文。
    pub fix: String,
    /// 見送った版（いまオンにしている版）。
    pub version: i64,
    /// 続く見送りの最初の時刻（RFC 3339）。
    pub first_at: String,
    /// 最後に見送った時刻（RFC 3339）。
    pub last_at: String,
    /// 続けて見送った回数。
    pub count: i64,
}

/// registration の現況。
#[derive(Debug, Serialize, ToSchema)]
pub struct RegistrationResponse {
    /// enabled / disabled / suspended_reconsent / none。
    pub status: String,
    pub enabled_version: Option<i64>,
    pub consented_scopes: Vec<String>,
    pub enabled_by: Option<String>,
    pub delegations: Vec<DelegationItem>,
    /// いま見送り続けている自動実行（無ければ null・GAP-67）。
    pub launch_skip: Option<LaunchSkipItem>,
}

/// 同意画面の提案 grant。
#[derive(Debug, Serialize, ToSchema)]
pub struct SuggestedGrantItem {
    pub scope: String,
    /// folder / file / secret / workflow / data_table。
    pub object_kind: String,
    /// 確定済み対象 id（リテラル参照・secret 名解決済みのとき）。
    pub object_id: Option<String>,
    /// 参照名（secret の name 等・表示用）。
    pub object_name: Option<String>,
    pub relation: String,
    /// 提案の根拠（`node:<id>` / `trigger:<index>`）。
    pub source: String,
    /// 対象をユーザーが選ぶ必要があるか。
    pub needs_user_pick: bool,
}

/// 同意計画レスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct ConsentPlanResponse {
    /// 宣言スコープ（権限の天井・同意対象）。
    pub declared_scopes: Vec<String>,
    pub grants: Vec<SuggestedGrantItem>,
    /// 有効化できない理由（必須の文字の欄が空・schedule / event で保存先フォルダを省略・D-40・D-43、schedule / event で
    /// 動く IR の、有効化する人のアカウントで使えないモデル・D-196）。
    /// 空なら有効化できる。enable は同じ理由を 409 `not_ready` で返す。
    pub blockers: Vec<workflow_engine::ValidationError>,
}

/// 委譲 1 件の付与指定。
#[derive(Debug, Deserialize, ToSchema)]
pub struct GrantItem {
    /// declared_scope の 1 要素（例: `storage.read`）。
    pub scope: String,
    /// 対象種別（folder / file / secret / workflow / data_table）。
    pub object_type: String,
    /// 対象 id。
    pub object_id: String,
    /// 付与 relation（viewer / editor / can_use）。
    pub relation: String,
}

/// 有効化リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct EnableRequest {
    /// 有効化する IR バージョン（enable は version 単位・ir.md §9）。
    pub version: i64,
    /// 明示委譲（省略時は軽量切替 = scope 拡大が無い場合のみ可。`consent` なら空でも同意し直す）。
    #[serde(default)]
    pub grants: Vec<GrantItem>,
    /// 同意の画面から送る（使ってよい操作と対象を見せたうえでの同意）。true なら grants が空でも軽量切替に
    /// せず同意し直す（対象の無いワークフローも、使ってよい操作が増えた版へ切り替えられる・BUG-102・D-90）。
    #[serde(default)]
    pub consent: bool,
}

/// 有効化レスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct EnableResponse {
    pub status: String,
    pub enabled_version: i64,
}

/// 無効化リクエスト（本文は省略できる）。
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct DisableRequest {
    /// 委譲もすべて取り消すか（「権限も取り消す」・D-90）。省略は false（委譲を残す＝今までどおり）。
    #[serde(default)]
    pub revoke_delegations: bool,
}

/// 無効化レスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct DisableResponse {
    /// 常に `disabled`。
    pub status: String,
    /// 取り消した委譲の数（`revoke_delegations` のときだけ数える・ほかは 0）。
    pub revoked_delegations: usize,
}

/// 委譲の対象の種類（API の `object_type`）と FGA の型の対応。付与（[`to_grant_request`]）と一覧の表示
/// （[`object_kind_of`]）が同じ表を引く（向きごとに手で書き分けない）。
const OBJECT_KINDS: &[(&str, &str)] = &[
    ("folder", "folder"),
    ("file", "file"),
    ("secret", "secret"),
    ("workflow", "artifact"),
    ("data_table", "data_table"),
];

/// 台帳の参照（`<型>:<tenant>|<id>`）から対象の種類と id を取り出す。読めなければ `None`。
fn object_kind_of(ns: &authz::Namespace<'_>, object_ref: &str) -> Option<(String, String)> {
    let (fga_type, rest) = object_ref.split_once(':')?;
    let kind = OBJECT_KINDS
        .iter()
        .find(|(_, t)| *t == fga_type)
        .map(|(k, _)| *k)?;
    let id = ns.strip_object_id(rest)?;
    Some((kind.to_string(), id.to_string()))
}

/// enable/disable は artifact **editor** 必須。viewer 以下は存在秘匿の 404。
async fn require_editor(
    state: &AppState,
    ctx: &authz::AuthContext,
    workflow_id: Uuid,
) -> Result<(), ApiError> {
    let obj = ctx.ns().artifact(&workflow_id.to_string());
    let ok = state
        .authz
        .check(
            &ctx.subject(),
            authz::Relation::Editor,
            &obj,
            authz::Consistency::HigherConsistency,
        )
        .await?;
    if ok {
        Ok(())
    } else {
        Err(ApiError::NotFound)
    }
}

/// 有効化状態と委譲一覧を返す。
#[utoipa::path(
    get,
    path = "/workflows/{id}/registration",
    params(("id" = Uuid, Path, description = "ワークフロー id")),
    responses(
        (status = 200, description = "現況", body = RegistrationResponse),
        (status = 404, description = "存在しない/権限なし"),
    ),
    tag = "workflows"
)]
pub async fn get_registration(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<RegistrationResponse>, ApiError> {
    // viewer ＋ kind=workflow ゲート（存在秘匿 404）。
    let meta = state.artifacts.get(&ctx, id, trace.as_deref()).await?;
    if meta.kind != artifact::ArtifactKind::Workflow {
        return Err(ApiError::NotFound);
    }
    let view = state
        .workflow_registration
        .view(ctx.tenant_id(), id)
        .await
        .map_err(map_enable_err)?;
    // 見送りの記録はワークフローを見られる人にだけ返す（上の viewer の確かめの後）。
    let launch_skip = state
        .workflow_registration
        .launch_skip(ctx.tenant_id(), id)
        .await
        .map_err(map_enable_err)?
        .map(|s| LaunchSkipItem {
            fix: s.fix().to_string(),
            code: s.code,
            message: s.message,
            version: s.version,
            first_at: s.first_at.to_rfc3339(),
            last_at: s.last_at.to_rfc3339(),
            count: s.count,
        });
    let ns = ctx.ns();
    Ok(Json(RegistrationResponse {
        launch_skip,
        status: view.status,
        enabled_version: view.enabled_version,
        consented_scopes: view.consented_scopes,
        enabled_by: view.enabled_by,
        delegations: view
            .delegations
            .into_iter()
            .map(|d| {
                let (object_kind, object_id) = object_kind_of(&ns, &d.object_ref).unzip();
                DelegationItem {
                    delegator: d.delegator,
                    scope: d.scope,
                    object_ref: d.object_ref,
                    object_kind,
                    object_id,
                    relation: d.relation,
                    granted_at: d.granted_at.to_rfc3339(),
                }
            })
            .collect(),
    }))
}

/// 指定バージョンの同意計画（提案 grants）を返す。
#[utoipa::path(
    get,
    path = "/workflows/{id}/versions/{version}/consent-plan",
    params(
        ("id" = Uuid, Path, description = "ワークフロー id"),
        ("version" = i64, Path, description = "IR バージョン")
    ),
    responses(
        (status = 200, description = "同意計画", body = ConsentPlanResponse),
        (status = 404, description = "存在しない/権限なし"),
    ),
    tag = "workflows"
)]
pub async fn consent_plan(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, version)): Path<(Uuid, i64)>,
) -> Result<Json<ConsentPlanResponse>, ApiError> {
    let ir = fetch_ir(&state, &ctx, id, version, trace.as_deref()).await?;
    let mut grants: Vec<SuggestedGrantItem> = RegistrationService::consent_plan(&ir)
        .into_iter()
        .map(|g| SuggestedGrantItem {
            scope: g.scope,
            object_kind: g.object_kind,
            object_id: g.object_id,
            object_name: g.object_name,
            relation: g.relation,
            source: g.source,
            needs_user_pick: g.needs_user_pick,
        })
        .collect();
    // secret 参照名 → id 解決（有効化者が見える secret のみ・見えない名前は選択要に落とす）。
    if let Some(secrets) = state.secrets.as_deref() {
        if grants.iter().any(|g| g.object_kind == "secret") {
            let mine = secrets.list_mine(&ctx).await?;
            for g in grants.iter_mut().filter(|g| g.object_kind == "secret") {
                if let Some(name) = &g.object_name {
                    g.object_id = mine
                        .iter()
                        .find(|m| &m.name == name)
                        .map(|m| m.id.to_string());
                    g.needs_user_pick = g.object_id.is_none();
                }
            }
        }
    }
    // workflow.start の参照名 → 子ワークフロー id 解決（tenant×kind 内で name 一意・Codex P2）。
    // 解決できない（未作成/権限なし）場合は選択要に落とす（grant を組み立て可能に保つ）。
    for g in grants
        .iter_mut()
        .filter(|g| g.object_kind == "workflow" && g.object_id.is_none())
    {
        if let Some(name) = &g.object_name {
            g.object_id = state
                .artifacts
                .get_by_name(
                    &ctx,
                    artifact::ArtifactKind::Workflow,
                    name,
                    trace.as_deref(),
                )
                .await
                .ok()
                .map(|a| a.id.to_string());
            g.needs_user_pick = g.object_id.is_none();
        }
    }
    let blockers = enable_blockers_for(&state, &ctx, &ir).await?;
    Ok(Json(ConsentPlanResponse {
        declared_scopes: ir.declared_scopes.clone(),
        grants,
        blockers,
    }))
}

/// 有効化を止める理由（同意計画と有効化が同じものを返す）。動かせない設定（[`workflow_engine::enable_blockers`]）と、
/// 有効化する人（`ctx`）のアカウントでは使えないモデルで自動実行されるもの
/// （[`crate::workflow_catalog::enable_model_blockers`]・保存のときと同じ材料・D-196）。
async fn enable_blockers_for(
    state: &AppState,
    ctx: &authz::AuthContext,
    ir: &WorkflowIr,
) -> Result<Vec<workflow_engine::ValidationError>, ApiError> {
    let mut blockers = workflow_engine::enable_blockers(ir);
    let models = super::save::workflow_models(state);
    blockers.extend(
        crate::workflow_catalog::enable_model_blockers(
            &models,
            Some(&state.skill_installs),
            ctx,
            ir,
        )
        .await
        .map_err(ApiError::Internal)?,
    );
    Ok(blockers)
}

/// 有効化（明示委譲・単一論理操作・fail-closed）。
#[utoipa::path(
    post,
    path = "/workflows/{id}/enable",
    params(("id" = Uuid, Path, description = "ワークフロー id")),
    request_body = EnableRequest,
    responses(
        (status = 200, description = "有効化済み", body = EnableResponse),
        (status = 400, description = "同じ操作と対象に違う relation の委譲が並んでいる（invalid_input）"),
        (status = 403, description = "有効化者の権限範囲外の委譲が含まれる"),
        (status = 404, description = "存在しない/editor でない"),
        (status = 409, description = "scope 拡大に grants が不足（missing_scopes 付き）か、有効化できない設定がある（not_ready・errors 付き）"),
    ),
    tag = "workflows"
)]
pub async fn enable_workflow(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<EnableRequest>,
) -> Result<Json<EnableResponse>, ApiError> {
    require_editor(&state, &ctx, id).await?;
    let ir = fetch_ir(&state, &ctx, id, req.version, trace.as_deref()).await?;
    // 動かせない設定と、有効化する人のアカウントでは使えないモデル（D-196）は、委譲のタプルに触る前に止める。
    // 同意し直しも軽量切替（`consent: false` を API から直接呼んだとき）も同じ。
    let blockers = enable_blockers_for(&state, &ctx, &ir).await?;
    if !blockers.is_empty() {
        return Err(map_enable_err(EnableError::NotReady(blockers)));
    }
    let grants = req
        .grants
        .iter()
        .map(|g| to_grant_request(&ctx, g))
        .collect::<Result<Vec<_>, _>>()?;
    // 有効化の前後で data_table の委譲（ワークフロー主体のタプル）が付け外しされ得る（全面再同意は
    // 外れた委譲を撤去する）。前の委譲先も控えておき、後でまとめて data へ知らせる。
    let before = state
        .workflow_registration
        .view(ctx.tenant_id(), id)
        .await
        .map_err(map_enable_err)?;
    let registration = &state.workflow_registration;
    let revoked = if req.consent {
        registration
            .consent(&ctx, id, req.version, &ir, &grants)
            .await
    } else {
        registration
            .enable(&ctx, id, req.version, &ir, &grants)
            .await
    }
    .map_err(map_enable_err)?;
    let mut tables: std::collections::BTreeSet<Uuid> = req
        .grants
        .iter()
        .filter(|g| g.object_type == "data_table")
        .filter_map(|g| Uuid::parse_str(&g.object_id).ok())
        .collect();
    let ns = ctx.ns();
    tables.extend(before.delegations.iter().filter_map(|d| {
        let (ty, rest) = d.object_ref.split_once(':')?;
        (ty == "data_table")
            .then(|| ns.strip_object_id(rest))
            .flatten()
            .and_then(|local| Uuid::parse_str(local).ok())
    }));
    for table_id in tables {
        // テーブルの ReBAC を data の外で変えた（配信の材料・材料化の前提を引き直させる・best-effort）。
        state.data.after_table_rebac_changed(&ctx, table_id).await;
    }
    record_audit(
        &state,
        &ctx,
        AuditAction::WorkflowEnable,
        id,
        trace.as_deref(),
        json!({
            "version": req.version,
            "grants": req.grants.len(),
            "consent": req.consent,
            "light_switch": req.grants.is_empty() && !req.consent,
        }),
    )
    .await;
    // 同意から外れて取り消した委譲（ほかの委譲元の行も含む）は、「権限も取り消す」と同じ形で残す。テーブルの分の
    // 知らせは上で済ませた（前の委譲先を含む）。
    record_revoked(
        &state,
        &ctx,
        id,
        trace.as_deref(),
        &revoked,
        Some("reconsent"),
    )
    .await;
    Ok(Json(EnableResponse {
        status: "enabled".into(),
        enabled_version: req.version,
    }))
}

/// 無効化（トリガ停止・委譲タプルは温存＝再有効化で再同意不要。run 開始は status で fail-closed）。
///
/// `revoke_delegations: true` なら委譲もすべて取り消す（「権限も取り消す」・D-90。取り消しは再同意で外れた
/// 委譲の撤去と同じ経路）。取り消した委譲は監査に 1 行で残す。
#[utoipa::path(
    post,
    path = "/workflows/{id}/disable",
    params(("id" = Uuid, Path, description = "ワークフロー id")),
    request_body(content = DisableRequest, description = "省略できる（省略は委譲を残す）"),
    responses(
        (status = 200, description = "無効化済み", body = DisableResponse),
        (status = 404, description = "存在しない/editor でない"),
    ),
    tag = "workflows"
)]
pub async fn disable_workflow(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    body: Option<Json<DisableRequest>>,
) -> Result<Json<DisableResponse>, ApiError> {
    require_editor(&state, &ctx, id).await?;
    let revoke = body.is_some_and(|Json(b)| b.revoke_delegations);
    let revoked = if revoke {
        // 自動実行で動いている run も取り消す（engine.md §10.4・BUG-131）。
        state
            .workflow_registration
            .disable_and_revoke(ctx.tenant_id(), id)
            .await
            .map_err(map_enable_err)?
    } else {
        state
            .workflow_registration
            .disable(ctx.tenant_id(), id)
            .await
            .map_err(map_enable_err)?;
        Revocation::default()
    };
    record_audit(
        &state,
        &ctx,
        AuditAction::WorkflowDisable,
        id,
        trace.as_deref(),
        json!({ "revoke_delegations": revoke, "cancelled_runs": revoked.cancelled_runs.len() }),
    )
    .await;
    if revoke {
        audit_revocation(&state, &ctx, id, trace.as_deref(), &revoked, None).await;
    }
    Ok(Json(DisableResponse {
        status: "disabled".into(),
        revoked_delegations: revoked.delegations.len(),
    }))
}

/// ワークフローを消したあとで止める（`DELETE /artifacts/{id}` の kind=workflow・BUG-128・D-105）。
///
/// 論理削除の commit の後に、「権限も取り消す」と同じ経路で委譲を取り消し、自動実行をオフにし、動いている run を
/// すべて取り消す（承認の依頼も取り下げる）。DB の側は論理削除のトリガ（migration 0132）が済ませているので、ここが
/// 失敗しても動き出さない。失敗は削除の結果を変えず、ログに残す（外せなかった委譲は scheduler の掃除が取り消す）。
pub(crate) async fn after_workflow_deleted(
    state: &AppState,
    ctx: &authz::AuthContext,
    id: Uuid,
    trace: Option<&str>,
) {
    match state
        .workflow_registration
        .retire(ctx.tenant_id(), id)
        .await
    {
        Ok(r) => {
            if r.registered || !r.cancelled_runs.is_empty() {
                record_audit(
                    state,
                    ctx,
                    AuditAction::WorkflowDisable,
                    id,
                    trace,
                    json!({
                        "reason": "workflow_deleted",
                        "revoke_delegations": true,
                        "cancelled_runs": r.cancelled_runs.len(),
                    }),
                )
                .await;
            }
            audit_revocation(state, ctx, id, trace, &r, Some("workflow_deleted")).await;
        }
        Err(e) => {
            tracing::error!(error = ?e, workflow = %id, "ワークフローを消したあとの停止に失敗（委譲は scheduler の掃除が取り消す）");
        }
    }
}

/// 取り消した委譲の後始末: テーブルの分を data に知らせ（配信の材料・材料化の前提を引き直させる・有効化と同じ）、
/// `workflow.delegation.revoke` を 1 行残す（取り消したものが無ければ何もしない）。
async fn audit_revocation(
    state: &AppState,
    ctx: &authz::AuthContext,
    id: Uuid,
    trace: Option<&str>,
    r: &Revocation,
    reason: Option<&str>,
) {
    if r.delegations.is_empty() {
        return;
    }
    let ns = ctx.ns();
    let tables: std::collections::BTreeSet<Uuid> = r
        .delegations
        .iter()
        .filter_map(|d| object_kind_of(&ns, &d.object_ref))
        .filter(|(kind, _)| kind == "data_table")
        .filter_map(|(_, local)| Uuid::parse_str(&local).ok())
        .collect();
    for table_id in tables {
        state.data.after_table_rebac_changed(ctx, table_id).await;
    }
    record_revoked(state, ctx, id, trace, &r.delegations, reason).await;
}

/// 取り消した委譲を `workflow.delegation.revoke` に 1 行で残す（委譲元・scope・対象・relation・取り消したものが
/// 無ければ何もしない）。
async fn record_revoked(
    state: &AppState,
    ctx: &authz::AuthContext,
    id: Uuid,
    trace: Option<&str>,
    delegations: &[workflow_engine::RevokedDelegation],
    reason: Option<&str>,
) {
    if delegations.is_empty() {
        return;
    }
    let mut metadata = json!({ "revoked": delegations.len(), "delegations": delegations });
    if let Some(reason) = reason {
        metadata["reason"] = json!(reason);
    }
    record_audit(
        state,
        ctx,
        AuditAction::WorkflowDelegationRevoke,
        id,
        trace,
        metadata,
    )
    .await;
}

/// 監査はコミット済み操作の記録（失敗しても操作結果は変えず、欠落をログで顕在化する）。
async fn record_audit(
    state: &AppState,
    ctx: &authz::AuthContext,
    action: AuditAction,
    id: Uuid,
    trace: Option<&str>,
    metadata: serde_json::Value,
) {
    if let Err(e) = state
        .audit
        .record(
            ctx,
            AuditEntry {
                action,
                object_type: "workflow",
                object_id: &id.to_string(),
                decision: Decision::Allow,
                trace_id: trace,
                metadata,
            },
        )
        .await
    {
        tracing::error!(error = %e, workflow = %id, action = action.as_str(), "ワークフローの監査記録に失敗");
    }
}

/// 指定バージョンの IR を取得しパースする（artifact viewer ゲート込み・kind 不一致は 404 秘匿）。
async fn fetch_ir(
    state: &AppState,
    ctx: &authz::AuthContext,
    id: Uuid,
    version: i64,
    trace: Option<&str>,
) -> Result<WorkflowIr, ApiError> {
    // **kind=workflow を必須にする**: script 等の IR 風 body を enable させると保存時 V1〜V7 を
    // バイパスして workflow_trigger を実体化できてしまう（Codex P1）。存在秘匿は 404。
    let meta = state.artifacts.get(ctx, id, trace).await?;
    if meta.kind != artifact::ArtifactKind::Workflow {
        return Err(ApiError::NotFound);
    }
    let body = state.artifacts.get_version(ctx, id, version, trace).await?;
    WorkflowIr::from_json(&body.body).map_err(|_| ApiError::NotFound)
}

/// API の grant 指定を FGA オブジェクトへ写像する（未知の種別/関係は 400）。
fn to_grant_request(ctx: &authz::AuthContext, g: &GrantItem) -> Result<GrantRequest, ApiError> {
    let ns = ctx.ns();
    // 種類の語は一覧の表示（object_kind_of）と同じ表にあるものだけを受ける。
    if !OBJECT_KINDS.iter().any(|(k, _)| *k == g.object_type) {
        return Err(ApiError::BadRequest(format!(
            "未知の object_type: {}",
            g.object_type
        )));
    }
    let object = match g.object_type.as_str() {
        "folder" => ns.folder(&g.object_id),
        "file" => ns.file(&g.object_id),
        "secret" => ns.secret(&g.object_id),
        "workflow" => ns.artifact(&g.object_id),
        // 構造化データのテーブル（data.* ノード・Task 15.7）。ワークフロー主体を data_table の
        // viewer / editor の subject にする（認可モデルは既存・relation 名は増やさない）。
        "data_table" => ns.data_table(&g.object_id),
        other => return Err(ApiError::BadRequest(format!("未知の object_type: {other}"))),
    };
    let relation = authz::Relation::parse(&g.relation)
        .ok_or_else(|| ApiError::BadRequest(format!("未知の relation: {}", g.relation)))?;
    Ok(GrantRequest {
        scope: g.scope.clone(),
        object,
        relation,
    })
}

/// エンジンのエラーを HTTP へ写像する。
fn map_enable_err(e: EnableError) -> ApiError {
    match e {
        EnableError::ScopeExpansion { missing } => ApiError::ConflictJson(json!({
            "code": "scope_expansion",
            "message": "scope が拡大しています（再同意が必要）",
            "missing_scopes": missing,
        })),
        EnableError::Delegation(workflow_engine::DelegationError::OutOfScope(d)) => {
            tracing::warn!(detail = %d, "権限範囲外の委譲要求を拒否");
            ApiError::Forbidden
        }
        // 同じ操作と対象に relation の違う委譲（画面からは送らない・API を直接呼んだとき）。
        EnableError::Delegation(workflow_engine::DelegationError::ConflictingGrants(d)) => {
            tracing::warn!(detail = %d, "同じ対象に relation の違う委譲要求を拒否");
            ApiError::invalid_input(
                "同じ操作と対象に、違う権限が 2 つ選ばれています。どちらか 1 つにしてください。",
            )
        }
        // 保存は通しても動かせない設定（D-40・D-43）。理由は検証エラーと同じ形で全件返す（欄に写せる）。
        EnableError::NotReady(errors) => ApiError::ConflictJson(json!({
            "code": "not_ready",
            "message": "有効化できない設定があります",
            "errors": errors,
        })),
        EnableError::Delegation(err) => ApiError::Internal(format!("delegation: {err}")),
        EnableError::Internal(m) => ApiError::Internal(m),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> authz::AuthContext {
        authz::AuthContext::new(
            authz::Principal {
                kind: authz::PrincipalKind::User,
                id: "alice".into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some("t1".into()),
            },
            "acme".into(),
            authz::TenantId::parse("t1").unwrap(),
        )
    }

    /// 付与で受ける種類はどれも、一覧の表示で同じ種類と id に戻る（向きごとの対応を書き分けない・BUG-102）。
    #[test]
    fn every_grant_kind_round_trips_to_the_listing() {
        let ctx = ctx();
        for (kind, _) in OBJECT_KINDS {
            let g = GrantItem {
                scope: "x.y".into(),
                object_type: (*kind).into(),
                object_id: "0f0e".into(),
                relation: "viewer".into(),
            };
            let req = to_grant_request(&ctx, &g).expect(kind);
            assert_eq!(
                object_kind_of(&ctx.ns(), req.object.as_str()),
                Some(((*kind).to_string(), "0f0e".to_string())),
                "{kind}"
            );
        }
        // 表に無い種類は受けない。読めない参照（型が無い・別のテナント）は種類を返さない。
        let g = GrantItem {
            scope: "x.y".into(),
            object_type: "thread".into(),
            object_id: "1".into(),
            relation: "viewer".into(),
        };
        assert!(matches!(
            to_grant_request(&ctx, &g),
            Err(ApiError::BadRequest(_))
        ));
        assert_eq!(object_kind_of(&ctx.ns(), "thread:t1|1"), None);
        assert_eq!(object_kind_of(&ctx.ns(), "folder:t2|1"), None);
        assert_eq!(object_kind_of(&ctx.ns(), "folder"), None);
    }
}

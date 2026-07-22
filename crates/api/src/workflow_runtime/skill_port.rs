//! skill.invoke の実行時解決（`NodePorts::skill_resolve` の実体・#344 Task 10.1b）。
//!
//! レジストリ（kind='skill'）で `name@version` を引き、**実行主体の ReBAC** で artifact を
//! 読む（fail-closed の実行時再検証・ir.md §8。保存時 OK でも yank/剥奪で消えていれば失敗）。
//! インストールは保存時照合（V4）の材料であり、実行時の最終防衛線はこの ReBAC 読取である。

use authz::AuthContext;
use workflow_engine::nodes::{PortError, ResolvedSkillView};

/// レジストリ→artifact→body 検証の実行時解決（executor のポートから呼ばれる）。
pub(super) async fn resolve_skill(
    db: &sqlx::PgPool,
    artifacts: &artifact::ArtifactStore,
    ctx: &AuthContext,
    name: &str,
    version: &str,
    trace_id: Option<&str>,
) -> Result<ResolvedSkillView, PortError> {
    let registry = app_platform::Registry::new(db.clone());
    let entry = registry
        .get(ctx, "skill", name, version)
        .await
        .map_err(|e| PortError::unavailable(format!("skill レジストリ: {e}")))?
        .ok_or_else(|| {
            PortError::forbidden(format!("skill {name}@{version} はレジストリに存在しません"))
        })?;
    if entry.yanked {
        return Err(PortError::forbidden(format!(
            "skill {name}@{version} は yank 済みです"
        )));
    }
    // 実行主体の viewer で読む（fail-closed・存在と権限を秘匿して畳む）。
    let v = artifacts
        .get_version(ctx, entry.artifact_id, entry.artifact_version, trace_id)
        .await
        .map_err(|_| PortError::forbidden(format!("skill {name}@{version} を読めません")))?;
    let body = gui::validate_skill_body(&v.body)
        .map_err(|e| PortError::invalid(format!("skill body が不正です: {e:?}")))?;
    // 先頭の `.shiki` script（あれば script-runtime 経路・shell script は agent 経路の将来対応）。
    let shiki_script = body
        .scripts
        .iter()
        .find(|s| s.kind == gui::ScriptKind::Shiki)
        .map(|s| s.source.clone());
    Ok(ResolvedSkillView {
        name: name.to_string(),
        instructions: body.instructions,
        shiki_script,
    })
}

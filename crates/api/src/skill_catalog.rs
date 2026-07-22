//! skill ツールのカタログ源（インストール済み ∪ 本人 owner・#344 Task 10.11）。
//!
//! chat の [`chat::SkillCatalogSource`] へ注入する実装。並び順は
//! **first-party → in-house（インストール済み）→ 本人 owner** で、信頼ティアを
//! 既定表示へ反映する（description スクワッティング防御の一部）。掲載は
//! 「明示的な人間の行為」（インストール／所有）に限る。

use std::collections::HashSet;
use std::sync::Arc;

use authz::AuthContext;
use chat::{ChatError, SkillCatalogEntry, SkillCatalogSource};

/// インストール済み ∪ 本人 owner のカタログ源。
pub struct ApiSkillCatalogSource {
    installs: Arc<app_platform::SkillInstallService>,
    artifacts: Arc<artifact::ArtifactStore>,
}

impl ApiSkillCatalogSource {
    #[must_use]
    pub fn new(
        installs: Arc<app_platform::SkillInstallService>,
        artifacts: Arc<artifact::ArtifactStore>,
    ) -> Self {
        ApiSkillCatalogSource {
            installs,
            artifacts,
        }
    }
}

#[async_trait::async_trait]
impl SkillCatalogSource for ApiSkillCatalogSource {
    async fn entries(
        &self,
        ctx: &AuthContext,
        _trace_id: Option<&str>,
    ) -> Result<Vec<SkillCatalogEntry>, ChatError> {
        // インストール済み（first-party → in-house・インストール順）。
        let installed = self
            .installs
            .list_installed_summaries(ctx)
            .await
            .map_err(|e| ChatError::Internal(format!("skill カタログ: {e}")))?;
        let mut seen: HashSet<uuid::Uuid> = HashSet::new();
        let mut out: Vec<SkillCatalogEntry> = Vec::with_capacity(installed.len());
        for s in installed {
            seen.insert(s.skill_id);
            out.push(SkillCatalogEntry {
                id: s.skill_id,
                version: s.skill_version,
                name: s.name,
                description: s.description,
                pinned: false,
            });
        }
        // 本人 owner（未インストールの自作 skill・名前順）。
        let own = self
            .artifacts
            .list_my_skill_summaries(ctx, 100)
            .await
            .map_err(|e| ChatError::Internal(format!("skill カタログ: {e}")))?;
        for s in own {
            if seen.contains(&s.id) {
                continue;
            }
            out.push(SkillCatalogEntry {
                id: s.id,
                version: s.current_version,
                name: s.name,
                description: s.description,
                pinned: false,
            });
        }
        Ok(out)
    }
}

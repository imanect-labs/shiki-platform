//! skill ツールのカタログ源（first-party 掲載 ∪ インストール済み ∪ 本人 owner・#344 / #387）。
//!
//! chat の [`chat::SkillCatalogSource`] へ注入する実装。並び順は
//! **first-party → in-house（インストール済み）→ 本人 owner** で、信頼ティアを
//! 既定表示へ反映する（description スクワッティング防御の一部）。
//!
//! # first-party は install 不要（#387）
//!
//! 署名 publish 済み（＝信頼鍵で検証済み）の first_party skill は、明示的なインストールを
//! 待たず全ユーザーのカタログへ載せる。`/deep-research` のような公式スキルが「最初から在る」
//! ためにこれが要る。読める根拠は publish 時に書かれる `organization#member → viewer`
//! タプル（`app_platform::SkillInstallService::publish`）で、認可は artifact
//! チョークポイントのまま（掲載を権限の代わりにしない）。
//!
//! in-house / marketplace の掲載は従来どおり「明示的な人間の行為」（インストール／所有）に限る。

use std::collections::HashSet;
use std::sync::Arc;

use authz::AuthContext;
use chat::{ChatError, SkillCatalogEntry, SkillCatalogSource};

/// body の `command` JSON を宣言型へ読む（壊れていれば None）。
///
/// 保存時に [`gui::validate_skill_body`] を通っている前提だが、署名 import された
/// バンドルなど経路が複数あるため、ここは**読めなければ落とす**（カタログ全体を失敗させない）。
fn parse_command(raw: Option<serde_json::Value>) -> Option<gui::SkillCommand> {
    serde_json::from_value(raw?).ok()
}

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
        // first-party 掲載（install 不要・#387）→ インストール済み → 本人 owner の順に積む。
        // 先頭に置くことで、同名/同 id が後段に現れても dedup で公式版が残る。
        let first_party = self
            .installs
            .list_first_party_summaries(ctx)
            .await
            .map_err(|e| ChatError::Internal(format!("skill カタログ: {e}")))?;
        let installed = self
            .installs
            .list_installed_summaries(ctx)
            .await
            .map_err(|e| ChatError::Internal(format!("skill カタログ: {e}")))?;
        let mut seen: HashSet<uuid::Uuid> = HashSet::new();
        let mut out: Vec<SkillCatalogEntry> =
            Vec::with_capacity(first_party.len() + installed.len());
        for s in first_party.into_iter().chain(installed) {
            // 自動掲載と install 済みが重なるケース（公式を明示 install したユーザー）で
            // 二重に出さない。
            if seen.contains(&s.skill_id) {
                continue;
            }
            seen.insert(s.skill_id);
            out.push(SkillCatalogEntry {
                id: s.skill_id,
                version: s.skill_version,
                name: s.name,
                description: s.description,
                pinned: false,
                // 宣言が壊れていても（手書き JSON の import 等）カタログ全体を落とさない。
                // 読めないものはコマンド無しとして扱う（fail-soft: 起動導線が 1 つ減るだけ）。
                command: parse_command(s.command),
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
                // 宣言が壊れていても（手書き JSON の import 等）カタログ全体を落とさない。
                // 読めないものはコマンド無しとして扱う（fail-soft: 起動導線が 1 つ減るだけ）。
                command: parse_command(s.command),
            });
        }
        Ok(out)
    }
}

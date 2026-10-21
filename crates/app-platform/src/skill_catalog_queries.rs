//! skill カタログ源のクエリ（`skill_install.rs` から分割・#344 / #387）。
//!
//! 「モデルが見ているカタログ」と「UI のスラッシュ補完」が同じ 1 実装から出るようにするための
//! 読み取り専用クエリ群（正本の合成は `api::skill_catalog::ApiSkillCatalogSource`）。
//! いずれも本文は載せず、`description` と `command`（起動宣言）だけを引く。

use authz::AuthContext;

use super::skill_install::{InstalledSkillSummary, SkillInstallService, SKILL_KIND};
use crate::{map_db, AppPlatformError};

impl SkillInstallService {
    /// **first-party 掲載**の要約（install 不要でカタログに出す・#387）。
    ///
    /// レジストリに署名 publish 済み（＝信頼鍵で検証済み）の first_party skill は、明示的な
    /// インストール行為を待たず全ユーザーのカタログへ載せる。deep research のような
    /// 公式スキルが「最初から `/deep-research` として在る」ためにこれが要る。
    /// 読める根拠は publish 時に書く `organization#member → viewer` タプル（[`Self::publish`]）。
    ///
    /// `name` ごとに**最新 publish の 1 件**（`created_at desc`）。yank 済みは出さない
    /// （新規利用を止める意味を掲載側にも効かせる）。artifact が削除されたものも除外する。
    ///
    /// 絞り込みは **tenant ＋ org** の両方（org は隔離境界・PIT-45/#371）。読取自体は
    /// `organization#member` タプルで org に閉じているが、掲載（name / description）も
    /// 同じ境界で切る。`Registry` の名前解決系は tenant だけで絞っており、そちらは
    /// 「名前を知っていて署名/viewer 検証を通る」ことが前提の別経路。
    pub async fn list_first_party_summaries(
        &self,
        ctx: &AuthContext,
    ) -> Result<Vec<InstalledSkillSummary>, AppPlatformError> {
        let rows: Vec<InstalledSkillSummary> = sqlx::query_as(
            "SELECT DISTINCT ON (r.name) \
                    r.name, r.artifact_id AS skill_id, r.artifact_version AS skill_version, \
                    r.trust_tier, \
                    coalesce(v.body->>'description', '') AS description, \
                    v.body->'command' AS command \
             FROM registry_entry r \
             JOIN artifact_version v \
               ON v.tenant_id = r.tenant_id AND v.artifact_id = r.artifact_id \
              AND v.version = r.artifact_version \
             JOIN artifact a \
               ON a.tenant_id = r.tenant_id AND a.id = r.artifact_id AND a.deleted_at IS NULL \
             WHERE r.tenant_id = $1 AND r.org = $2 AND r.artifact_kind = $3 \
               AND r.trust_tier = 'first_party' AND r.yanked = false \
             ORDER BY r.name, r.created_at DESC LIMIT 100",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(SKILL_KIND)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(rows)
    }

    /// 本人のインストール済み要約（skill ツールのカタログ用・description 込み・単一クエリ）。
    ///
    /// 並び順は first-party → in-house → 新しい順（信頼ティアを既定表示へ反映・#344）。
    pub async fn list_installed_summaries(
        &self,
        ctx: &AuthContext,
    ) -> Result<Vec<InstalledSkillSummary>, AppPlatformError> {
        let rows: Vec<InstalledSkillSummary> = sqlx::query_as(
            "SELECT i.name, i.skill_id, i.skill_version, i.trust_tier, \
                    coalesce(v.body->>'description', '') AS description, \
                    v.body->'command' AS command \
             FROM skill_installation i \
             JOIN artifact_version v \
               ON v.tenant_id = i.tenant_id AND v.artifact_id = i.skill_id \
              AND v.version = i.skill_version \
             JOIN artifact a \
               ON a.tenant_id = i.tenant_id AND a.id = i.skill_id AND a.deleted_at IS NULL \
             WHERE i.tenant_id = $1 AND i.user_id = $2 \
             ORDER BY (i.trust_tier <> 'first_party'), i.created_at DESC LIMIT 200",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.principal.id)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(rows)
    }
}

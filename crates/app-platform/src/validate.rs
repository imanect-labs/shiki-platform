//! マニフェスト検証（Task 9.1・語彙照合＝ハルシネーション境界・design §4.1）。
//!
//! 要求スコープ・宣言ツールを**閉じた語彙集合**（[`authz::CapabilityScope`] /
//! [`agent_core::ToolName`]）へ照合し、実在しない権限名（LLM/開発者由来）を拒否する。
//! 未知名は列挙して 1 度に返す（1 つずつ直させない）。

use agent_core::ToolName;
use authz::CapabilityScope;

use crate::manifest::{MiniAppManifest, TrustTier};
use crate::AppPlatformError;

/// テーブル名の上限。
const MAX_NAME_LEN: usize = 128;
/// 所有テーブル数の上限（プロビジョンコストの防御的上限）。
const MAX_TABLES: usize = 50;

/// マニフェストを検証する（語彙照合・スキーマ・semver・信頼ティア）。
///
/// エラーは `AppPlatformError::Invalid`（人間可読・未知語彙は列挙）。
pub fn validate_manifest(manifest: &MiniAppManifest) -> Result<(), AppPlatformError> {
    let name = manifest.name.trim();
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return Err(invalid(format!(
            "name は 1〜{MAX_NAME_LEN} 文字で指定してください"
        )));
    }
    // semver 検証（publish の一意キー）。
    semver::Version::parse(&manifest.version)
        .map_err(|e| invalid(format!("version が semver ではありません: {e}")))?;

    // marketplace ティアは Phase 9 アルファでは拒否（将来トラック）。
    if manifest.trust_tier == TrustTier::Marketplace {
        return Err(invalid(
            "trust_tier=marketplace は現時点では未対応です（審査付き第三者公開は将来）".into(),
        ));
    }

    // 語彙照合（ハルシネーション境界）。未知名を全て集めて 1 度に返す。
    let mut unknown: Vec<String> = Vec::new();
    for s in &manifest.requested_scopes {
        if CapabilityScope::parse(s).is_none() {
            unknown.push(format!("scope:{s}"));
        }
    }
    for t in &manifest.tools {
        if ToolName::parse(t).is_none() {
            unknown.push(format!("tool:{t}"));
        }
    }
    if !unknown.is_empty() {
        return Err(invalid(format!(
            "実在しない権限名を参照しています: {}",
            unknown.join(", ")
        )));
    }

    // 所有テーブル: 数・名前・スキーマ検証（data のスキーマ検証を再利用）。
    if manifest.tables.len() > MAX_TABLES {
        return Err(invalid(format!(
            "所有テーブルが多すぎます（最大 {MAX_TABLES}）"
        )));
    }
    let mut names = std::collections::HashSet::new();
    for t in &manifest.tables {
        let tname = t.name.trim();
        if tname.is_empty() || tname.len() > MAX_NAME_LEN {
            return Err(invalid(format!("テーブル名 '{tname}' が不正です")));
        }
        if !names.insert(tname.to_string()) {
            return Err(invalid(format!("テーブル名 '{tname}' が重複しています")));
        }
        // data のスキーマ検証（フィールド・row_policy・field_policy・fsm_ref 整合）を借用する。
        data::validate_table_schema_public(&t.schema)
            .map_err(|e| invalid(format!("テーブル '{tname}': {e}")))?;
    }

    // B2 の egress allowlist は完全一致 or `*.suffix` のみ（部分文字列は禁止）。
    if let Some(server) = &manifest.server {
        for host in &server.egress_allowlist {
            if !is_valid_egress_pattern(host) {
                return Err(invalid(format!(
                    "egress allowlist の指定が不正です: {host}"
                )));
            }
        }
    }

    // budget の整合（非負）。
    if manifest.budget.daily_usd_micros.is_some_and(|v| v < 0)
        || manifest.budget.max_tokens.is_some_and(|v| v < 0)
    {
        return Err(invalid("budget は非負で指定してください".into()));
    }
    Ok(())
}

/// egress パターンが完全一致ホスト or `*.suffix` か（部分文字列マッチを禁止）。
fn is_valid_egress_pattern(pat: &str) -> bool {
    let host = pat.strip_prefix("*.").unwrap_or(pat);
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

fn invalid(msg: String) -> AppPlatformError {
    AppPlatformError::Invalid(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::MiniAppManifest;

    fn base() -> MiniAppManifest {
        MiniAppManifest {
            name: "expense-app".into(),
            version: "1.0.0".into(),
            description: String::new(),
            requested_scopes: vec!["data.read".into(), "data.write".into()],
            tools: vec!["doc_search".into()],
            tables: vec![],
            workflows: vec![],
            budget: crate::manifest::Budget::default(),
            frontend: None,
            server: None,
            trust_tier: TrustTier::InHouse,
        }
    }

    #[test]
    fn accepts_valid_manifest() {
        assert!(validate_manifest(&base()).is_ok());
    }

    #[test]
    fn rejects_unknown_scope_and_tool() {
        let mut m = base();
        m.requested_scopes.push("storage.delete".into());
        m.tools.push("nuke".into());
        let e = validate_manifest(&m).unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("scope:storage.delete"), "{msg}");
        assert!(msg.contains("tool:nuke"), "{msg}");
    }

    #[test]
    fn rejects_bad_semver_and_marketplace() {
        let mut m = base();
        m.version = "v1".into();
        assert!(validate_manifest(&m).is_err());
        let mut m = base();
        m.trust_tier = TrustTier::Marketplace;
        assert!(validate_manifest(&m).is_err());
    }

    #[test]
    fn egress_pattern_rules() {
        assert!(is_valid_egress_pattern("api.slack.com"));
        assert!(is_valid_egress_pattern("*.slack.com"));
        assert!(!is_valid_egress_pattern(""));
        assert!(!is_valid_egress_pattern("has space"));
        assert!(!is_valid_egress_pattern("*."));
    }

    #[test]
    fn rejects_empty_and_overlong_name() {
        let mut m = base();
        m.name = "   ".into();
        assert!(validate_manifest(&m).is_err());
        let mut m = base();
        m.name = "a".repeat(MAX_NAME_LEN + 1);
        assert!(validate_manifest(&m).is_err());
    }

    #[test]
    fn rejects_too_many_and_duplicate_tables() {
        use crate::manifest::ManifestTable;
        // 1 フィールドの有効スキーマ（重複名チェックがスキーマ検証より先に効くことを見る）。
        let schema: data::TableSchema =
            serde_json::from_value(serde_json::json!({"fields":[{"name":"title","type":"text"}]}))
                .unwrap();
        let tbl = |n: &str| ManifestTable {
            name: n.into(),
            schema: schema.clone(),
        };
        // 数の上限。
        let mut m = base();
        m.tables = (0..=MAX_TABLES).map(|i| tbl(&format!("t{i}"))).collect();
        assert!(validate_manifest(&m).is_err());
        // 重複名。
        let mut m = base();
        m.tables = vec![tbl("dup"), tbl("dup")];
        assert!(validate_manifest(&m).is_err());
        // 空テーブル名。
        let mut m = base();
        m.tables = vec![tbl("  ")];
        assert!(validate_manifest(&m).is_err());
    }

    #[test]
    fn rejects_negative_budget_and_bad_egress() {
        let mut m = base();
        m.budget.daily_usd_micros = Some(-1);
        assert!(validate_manifest(&m).is_err());
        let mut m = base();
        m.server = Some(crate::manifest::ServerSpec {
            egress_allowlist: vec!["bad host".into()],
            ..Default::default()
        });
        assert!(validate_manifest(&m).is_err());
    }
}

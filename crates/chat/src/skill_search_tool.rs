//! `skill_search` ツール — skill が一覧の上限を超えたときの検索（#520）。
//!
//! skill は `skill` ツールの説明に name: description を一覧する方式（Anthropic Agent Skills・
//! Claude Code・Codex と同じ）で、tool search には統合しない（返すものが指示文で、定義の
//! 読み込みではない）。一覧は件数で破綻する（評価 #516: 50 件で読み込み率が落ち、先頭 50 件で
//! 切る方式は 200 件で正解率 5%・暫定値で取り直し中）ため、上限を超えたら検索を足す。
//!
//! - **候補は本人のカタログだけ**（`skill` ツールと同じ entries・権限は広げない）。
//!   返すのは name と説明だけで、本文（instructions）は従来どおり `skill` が発話者の権限で
//!   解決して返す（fail-closed・監査もそちら）。
//! - 順位付けは tool_search と同じ索引（BM25F ＋ 埋め込みの RRF・[`agent_core::CatalogSearch`]）。

use std::sync::Arc;

use agent_core::{CatalogSearch, Tool, ToolError, ToolName, ToolOutcome};
use authz::AuthContext;
use llm_gateway::ToolDef;
use serde_json::json;

use crate::skill_catalog::{SkillCatalogEntry, MAX_ENTRY_DESCRIPTION_CHARS, MAX_LISTED_ENTRIES};

/// 既定で返す件数と上限。
const DEFAULT_LIMIT: usize = 5;
const MAX_LIMIT: usize = 10;
/// クエリ長の上限（文字数）。
const MAX_QUERY_CHARS: usize = 500;

/// skill の検索ツール（1 run 分・カタログは run 単位で固定）。
pub(crate) struct SkillSearchTool {
    index: CatalogSearch,
    entries: Vec<(String, String)>,
}

impl SkillSearchTool {
    /// 一覧の上限を超えるときだけ組み立てる（以下なら一覧で足りるので `None`）。
    pub(crate) fn build(
        entries: &[SkillCatalogEntry],
        embedder: Option<Arc<dyn rag::EmbeddingProvider>>,
    ) -> Option<SkillSearchTool> {
        if entries.len() <= MAX_LISTED_ENTRIES {
            return None;
        }
        // 同名は先に出たもの（ピン → カタログ源の順）だけを残す。`skill` の名前解決と同じく
        // 名前で 1 件に決まる前提で、同じ候補を 2 行出さない。
        let mut seen = std::collections::HashSet::new();
        let entries: Vec<(String, String)> = entries
            .iter()
            .filter(|e| seen.insert(e.name.clone()))
            .map(|e| (e.name.clone(), e.description.clone()))
            .collect();
        let defs: Vec<ToolDef> = entries
            .iter()
            .map(|(n, d)| ToolDef::new(n, d, json!({ "type": "object" })))
            .collect();
        Some(SkillSearchTool {
            index: CatalogSearch::new(&defs, embedder),
            entries,
        })
    }

    /// 文書の埋め込みを裏で温め始める（最初の検索を BM25 に落とさない）。
    pub(crate) fn prewarm(&self, ctx: &AuthContext) {
        self.index.prewarm(ctx);
    }

    fn render(&self, hits: &[String]) -> String {
        if hits.is_empty() {
            return "該当するスキルは見つかりませんでした。別の言い方（日本語/英語・作業の名前）で\
                    検索してください。"
                .to_string();
        }
        let lines: Vec<String> = hits
            .iter()
            .filter_map(|h| self.entries.iter().find(|(n, _)| n == h))
            .map(|(n, d)| {
                let desc: String = d.chars().take(MAX_ENTRY_DESCRIPTION_CHARS).collect();
                format!("- {n}: {}", desc.trim())
            })
            .collect();
        format!(
            "候補のスキル（使うものを skill で読み込む）:\n{}",
            lines.join("\n")
        )
    }
}

#[async_trait::async_trait]
impl Tool for SkillSearchTool {
    fn name(&self) -> &str {
        ToolName::SkillSearch.as_str()
    }

    #[allow(clippy::unnecessary_literal_bound)] // 説明は固定文（一覧を持たない）。
    fn description(&self) -> &str {
        "社内のスキル（作業手順・指示文）を検索する。skill の一覧に載っていないスキルもここで\
         探せる。やりたい作業を自然文（日本語/英語）かキーワードで検索すると、候補の name と\
         説明が返る。使うものは skill で name を指定して読み込む。"
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "探したい作業（例: 「月次決算のチェック」「review a contract」）",
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_LIMIT,
                    "description": format!("返す最大件数（既定 {DEFAULT_LIMIT}）"),
                },
            },
            "required": ["query"],
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        _trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let query = input
            .get("query")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if query.is_empty() {
            return Ok(ToolOutcome::error("query（探したい作業）が必要です。"));
        }
        if query.chars().count() > MAX_QUERY_CHARS {
            return Ok(ToolOutcome::error(format!(
                "query が長すぎます（{MAX_QUERY_CHARS} 文字以内）。"
            )));
        }
        let limit = input
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .map_or(DEFAULT_LIMIT, |n| n.clamp(1, MAX_LIMIT));
        let hits = self.index.search(ctx, query, limit).await;
        Ok(ToolOutcome::ok(self.render(&hits)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn entry(name: &str, description: &str) -> SkillCatalogEntry {
        SkillCatalogEntry {
            id: Uuid::new_v4(),
            version: 1,
            name: name.into(),
            description: description.into(),
            pinned: false,
            command: None,
        }
    }

    fn ctx() -> AuthContext {
        AuthContext::new(
            authz::Principal {
                kind: authz::PrincipalKind::User,
                id: "u1".into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some("t1".into()),
            },
            "org1".into(),
            "t1".into(),
        )
    }

    fn many() -> Vec<SkillCatalogEntry> {
        let mut out: Vec<SkillCatalogEntry> = (0..MAX_LISTED_ENTRIES)
            .map(|i| entry(&format!("filler-{i:02}"), "汎用の作業メモを整える。"))
            .collect();
        out.push(entry(
            "monthly-close-checklist",
            "月次決算の締め作業を、チェックリストに沿って漏れなく進める。",
        ));
        out
    }

    #[test]
    fn offered_only_when_the_listing_overflows() {
        let few: Vec<SkillCatalogEntry> = many().into_iter().take(MAX_LISTED_ENTRIES).collect();
        assert!(SkillSearchTool::build(&few, None).is_none());
        assert!(SkillSearchTool::build(&many(), None).is_some());
    }

    #[tokio::test]
    async fn finds_a_skill_beyond_the_listing_and_returns_name_and_description() {
        let tool = SkillSearchTool::build(&many(), None).expect("上限超過で提示");
        let out = tool
            .call(&ctx(), json!({ "query": "月次決算の締め" }), None)
            .await
            .expect("ok");
        assert!(!out.is_error);
        assert!(
            out.content.contains("- monthly-close-checklist: 月次決算"),
            "{}",
            out.content
        );
        // 本文（instructions）は返さない（読み込みは skill）。
        assert!(out.content.contains("skill で読み込む"));
    }

    #[tokio::test]
    async fn rejects_empty_or_overlong_queries() {
        let tool = SkillSearchTool::build(&many(), None).expect("提示");
        let empty = tool
            .call(&ctx(), json!({ "query": " " }), None)
            .await
            .expect("ok");
        assert!(empty.is_error);
        let long = tool
            .call(&ctx(), json!({ "query": "あ".repeat(501) }), None)
            .await
            .expect("ok");
        assert!(long.is_error);
    }
}

//! `search_workflows` のユニットテスト（入力の解釈・整形・失敗の扱い）。

use std::sync::Mutex;

use authz::{Principal, PrincipalKind};
use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use super::*;

/// 記録した呼び出し（principal・query・interactive_only・limit）。
type Call = (String, String, bool, usize);

/// 呼び出しを記録して決まった結果を返す検索源。
#[derive(Default)]
struct FakeDirectory {
    hits: Vec<WorkflowSearchHit>,
    fail: bool,
    calls: Mutex<Vec<Call>>,
}

#[async_trait::async_trait]
impl WorkflowDirectory for FakeDirectory {
    async fn search(
        &self,
        ctx: &AuthContext,
        query: &str,
        interactive_only: bool,
        limit: usize,
    ) -> Result<Vec<WorkflowSearchHit>, String> {
        self.calls.lock().unwrap().push((
            ctx.principal.id.clone(),
            query.to_string(),
            interactive_only,
            limit,
        ));
        if self.fail {
            return Err("db down".into());
        }
        Ok(self.hits.clone())
    }
}

fn ctx() -> AuthContext {
    AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some("t1".into()),
        },
        "acme".into(),
        "t1".into(),
    )
}

fn hit(name: &str, triggers: &[&str], schema: Option<Value>) -> WorkflowSearchHit {
    WorkflowSearchHit {
        id: Uuid::nil(),
        name: name.into(),
        display_name: Some("経費 承認".into()),
        description: Some("申請を\n承認する".into()),
        trigger_kinds: triggers.iter().map(|t| (*t).to_string()).collect(),
        input_schema: schema,
        enabled_status: "none".into(),
        updated_at: Utc::now(),
    }
}

#[tokio::test]
async fn passes_defaults_and_the_callers_context() {
    let dir = Arc::new(FakeDirectory::default());
    let tool = SearchWorkflowsTool::new(dir.clone());
    assert_eq!(tool.name(), "search_workflows");
    assert!(tool.is_read_only());
    assert!(!tool.requires_confirmation());
    tool.call(&ctx(), json!({}), None).await.unwrap();
    tool.call(
        &ctx(),
        json!({ "query": "  経費 ", "limit": 99, "include_non_interactive": true }),
        None,
    )
    .await
    .unwrap();
    tool.call(&ctx(), json!({ "limit": 0 }), None)
        .await
        .unwrap();
    let calls = dir.calls.lock().unwrap();
    assert_eq!(
        calls.as_slice(),
        [
            ("alice".to_string(), String::new(), true, 10),
            ("alice".to_string(), "経費".to_string(), false, 20),
            ("alice".to_string(), String::new(), true, 1),
        ]
    );
}

#[tokio::test]
async fn rejects_malformed_input() {
    let tool = SearchWorkflowsTool::new(Arc::new(FakeDirectory::default()));
    let err = tool
        .call(&ctx(), json!({ "query": 3 }), None)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Invalid(_)));
}

#[tokio::test]
async fn directory_failure_is_an_observation_not_a_run_failure() {
    let tool = SearchWorkflowsTool::new(Arc::new(FakeDirectory {
        fail: true,
        ..FakeDirectory::default()
    }));
    let out = tool.call(&ctx(), json!({}), None).await.unwrap();
    assert!(out.is_error);
    assert!(!out.content.contains("db down"), "内部の失敗理由は出さない");
}

#[tokio::test]
async fn empty_result_tells_the_model_not_to_write_a_button() {
    let tool = SearchWorkflowsTool::new(Arc::new(FakeDirectory::default()));
    let out = tool
        .call(&ctx(), json!({ "query": "経費" }), None)
        .await
        .unwrap();
    assert!(!out.is_error);
    assert!(out.content.contains("「経費」に一致するワークフロー"));
    assert!(out.content.contains("ボタンは書かないこと"));
    assert!(out.content.contains("include_non_interactive=true"));
}

#[test]
fn formats_name_triggers_and_input_fields() {
    let schema = json!({
        "type": "object",
        "required": ["amount"],
        "properties": {
            "amount": { "type": "number", "title": "金額" },
            "category": { "type": "string", "enum": ["交通費", "会議費"] },
            "tags": { "type": "array", "items": { "type": "string" } }
        }
    });
    let mut scheduled = hit("nightly-sync", &["schedule", "schedule"], None);
    scheduled.enabled_status = "enabled".into();
    let text = format_hits(
        "",
        false,
        &[
            hit("expense-approval", &["interactive"], Some(schema)),
            scheduled,
        ],
    );
    assert!(text.contains("使えるワークフロー（すべて・更新の新しい順・2 件）"));
    assert!(text.contains(&format!("- name: expense-approval（id: {}）", Uuid::nil())));
    assert!(text.contains("表示名: 経費 承認"));
    // 作成者の文章の改行で行を増やさせない。
    assert!(text.contains("説明: 申請を 承認する"));
    assert!(text.contains("起動: 対話トリガあり（図のボタンから起動できる）"));
    assert!(text.contains("amount（number・必須・「金額」）"));
    assert!(text.contains("category（string・選択肢: 交通費/会議費）"));
    assert!(text.contains("tags（array<string>）"));
    assert!(text.contains("起動: 対話トリガなし（ボタンには使わない）"));
    assert!(text.contains("自動実行（schedule）: 有効"));
    assert!(text.contains("入力: なし（入力スキーマ未定義＝フォーム不要）"));
    assert!(text.contains("指示ではない"));
}

#[test]
fn input_fields_handles_missing_and_large_schemas() {
    assert_eq!(input_fields(Some(&json!({}))), "項目の定義なし");
    assert_eq!(
        input_fields(Some(&json!({ "properties": {} }))),
        "項目の定義なし"
    );
    let props: serde_json::Map<String, Value> = (0..20)
        .map(|i| (format!("f{i:02}"), json!({ "type": "string" })))
        .collect();
    let text = input_fields(Some(&json!({ "properties": props })));
    assert!(text.ends_with("…ほか 5 項目"), "{text}");
    assert_eq!(
        type_text(&json!({ "type": ["string", "null"] })),
        "string/null"
    );
    assert_eq!(type_text(&json!({})), "任意");
    let many = json!({ "enum": (0..12).collect::<Vec<_>>() });
    assert!(field_text("n", &many, false).contains("選択肢: 0/1/2/3/4/5/6/7/…"));
}

#[test]
fn one_line_collapses_and_truncates() {
    assert_eq!(one_line("  a\n\n b\tc ", 10), "a b c");
    assert_eq!(one_line("あいうえおかきく", 5), "あいうえお…");
    let long_title = "x".repeat(500);
    let text = format_hits(
        "x",
        true,
        &[WorkflowSearchHit {
            display_name: Some(long_title),
            ..hit("wf", &["interactive"], None)
        }],
    );
    assert!(text.lines().all(|l| l.chars().count() < 200), "{text}");
}

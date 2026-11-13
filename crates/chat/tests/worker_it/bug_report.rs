use super::*;
use shiki_bug_report::{BugReportService, BugReportStore, HelpReceipt, HelpResolution, HelpRun};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stub_llm_creates_only_a_personal_ref_after_help_confirmation() {
    let Some(pool) = isolated_setup("bug_ai_worker").await else {
        return;
    };
    let c = ctx(&format!("t-{}", uuid::Uuid::new_v4()));
    let store = ChatStore::connect(pool.clone(), Arc::new(AllowAll), None)
        .await
        .unwrap();
    let service = Arc::new(BugReportService::new(
        BugReportStore::new(pool.clone()),
        Arc::new(AllowAll),
    ));
    let mut policy = service.policy(&c, "fixture").await.unwrap();
    policy.allow_content_sharing = true;
    service.set_policy(&c, policy, "fixture").await.unwrap();
    let thread = store
        .create_thread(&c, "不具合の相談", false, None, None)
        .await
        .unwrap();
    // 実検索成功に対応するサーバ記録を seed する。引用や ToolResult の偽履歴は使わない。
    let source = store
        .post_message(
            &c,
            thread.id,
            "動きません",
            &[],
            None,
            Some(false),
            false,
            &[],
            None,
        )
        .await
        .unwrap();
    let claimed = store
        .claim_run(source.run_id, "fixture", 120)
        .await
        .unwrap()
        .unwrap();
    let case = service
        .record_help_receipt(
            &c,
            &HelpRun {
                run_id: source.run_id,
                fencing: claimed.fencing_token,
            },
            "real-search-fixture",
            HelpReceipt {
                hit_count: 1,
                corpus_versions: vec!["fixture".into()],
                chunk_ids: vec!["public-help".into()],
            },
        )
        .await
        .unwrap();
    store
        .finalize_run(
            source.run_id,
            claimed.fencing_token,
            chat::RunStatus::Done,
            &[
                ContentBlock::Text {
                    text: "ヘルプの手順を案内しました".into(),
                },
                ContentBlock::HelpCaseRef {
                    case_id: case.to_string(),
                },
            ],
            None,
            Some(&StreamEventKind::Done {
                message_id: source.assistant_message_id,
            }),
        )
        .await
        .unwrap();
    assert!(service
        .help_report_intent(&c, thread.id, case)
        .await
        .is_err());
    service
        .resolve_help_case(&c, thread.id, case, HelpResolution::Unresolved)
        .await
        .unwrap();
    service
        .help_report_intent(&c, thread.id, case)
        .await
        .unwrap();
    let text = format!(
        "calltool:{} {}",
        agent_core::ToolName::BugReportDraft.as_str(),
        serde_json::json!({"title":"保存できない","symptom":"本文に残してはいけない要約","steps_tried":"再読み込み","expected_behavior":"保存される"})
    );
    let post = store
        .post_message(
            &c,
            thread.id,
            &text,
            &[],
            None,
            Some(false),
            false,
            &[],
            None,
        )
        .await
        .unwrap();
    let tasks = ChatWorker::new(
        pool.clone(),
        store.clone(),
        test_deps(stub_gateway(pool.clone())),
        WorkerConfig {
            model: Some("m".into()),
            max_steps: 4,
            lease_secs: 120,
            ..Default::default()
        },
    )
    .with_bug_report(Some(service.clone()))
    .spawn(1);
    let mut events = store.event_stream(c.tenant_id(), post.run_id, 0);
    let mut report = None;
    while let Some(event) = tokio::time::timeout(Duration::from_secs(30), events.next())
        .await
        .unwrap()
    {
        match event.event {
            StreamEventKind::ToolCall { name, input, .. }
                if name == agent_core::ToolName::BugReportDraft.as_str() =>
            {
                assert_eq!(input, serde_json::json!({}))
            }
            StreamEventKind::BugReportDraft { report_id, .. } => {
                report = Some(uuid::Uuid::parse_str(&report_id).unwrap())
            }
            StreamEventKind::Error { message } => panic!("{message}"),
            StreamEventKind::Done { .. } => break,
            _ => {}
        }
    }
    let report = service
        .get(&c, report.expect("本人用のref"), "test")
        .await
        .unwrap();
    assert_eq!(report.status, shiki_bug_report::BugReportStatus::Draft);
    assert_eq!(report.content.symptom, "本文に残してはいけない要約");
    let messages = store.get_messages(&c, thread.id, None).await.unwrap();
    let answer = messages
        .iter()
        .find(|m| m.id == post.assistant_message_id)
        .unwrap();
    let json = serde_json::to_string(&answer.content).unwrap();
    assert!(json.contains("bug_report_draft"));
    assert!(!json.contains("本文に残してはいけない要約"));
    for task in tasks {
        task.abort();
    }
}

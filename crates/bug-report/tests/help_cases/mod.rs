use super::*;
use shiki_bug_report::{HelpReceipt, HelpResolution, HelpRun};

async fn run(
    db: &PgPool,
    ctx: &AuthContext,
    thread: Uuid,
    parent: Option<Uuid>,
) -> (HelpRun, Uuid) {
    let user = Uuid::new_v4();
    let answer = Uuid::new_v4();
    let run = Uuid::new_v4();
    sqlx::query("INSERT INTO message (tenant_id,org,id,thread_id,role,parent_id,content) VALUES ($1,$2,$3,$4,'user',$5,'[{\"type\":\"text\",\"text\":\"動かない\"}]')")
        .bind(ctx.tenant_id()).bind(ctx.org()).bind(user).bind(thread).bind(parent).execute(db).await.unwrap();
    sqlx::query("INSERT INTO message (tenant_id,org,id,thread_id,role,parent_id) VALUES ($1,$2,$3,$4,'assistant',$5)")
        .bind(ctx.tenant_id()).bind(ctx.org()).bind(answer).bind(thread).bind(user).execute(db).await.unwrap();
    sqlx::query("INSERT INTO generation_run (tenant_id,org,run_id,thread_id,message_id,actor,status,fencing_token,lease_until) VALUES ($1,$2,$3,$4,$5,$6,'running',1,now()+interval '1 hour')")
        .bind(ctx.tenant_id()).bind(ctx.org()).bind(run).bind(thread).bind(answer).bind(&ctx.principal().id).execute(db).await.unwrap();
    sqlx::query("UPDATE thread SET active_message_id=$4 WHERE tenant_id=$1 AND org=$2 AND id=$3")
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(thread)
        .bind(answer)
        .execute(db)
        .await
        .unwrap();
    (
        HelpRun {
            run_id: run,
            fencing: 1,
        },
        answer,
    )
}
async fn finish(db: &PgPool, ctx: &AuthContext, run: &HelpRun, answer: Uuid) {
    sqlx::query(
        "UPDATE generation_run SET status='done' WHERE tenant_id=$1 AND org=$2 AND run_id=$3",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(run.run_id)
    .execute(db)
    .await
    .unwrap();
    sqlx::query("UPDATE message SET content='[{\"type\":\"text\",\"text\":\"手順を試してください\"}]' WHERE tenant_id=$1 AND org=$2 AND id=$3")
        .bind(ctx.tenant_id()).bind(ctx.org()).bind(answer).execute(db).await.unwrap();
}
#[tokio::test]
async fn help_first_requires_durable_search_guidance_and_personal_buttons() {
    let Some((db, fga, service, ctx)) = setup().await else {
        return;
    };
    for hits in [0, 1] {
        let thread = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO thread (id,tenant_id,org,owner,title) VALUES ($1,$2,$3,$4,'ダミー相談')",
        )
        .bind(thread)
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(&ctx.principal().id)
        .execute(&db)
        .await
        .unwrap();
        fga.write_tuple(
            &ctx.subject(),
            Relation::Owner,
            &ctx.ns().thread(&thread.to_string()),
        )
        .await
        .unwrap();
        let (source, guidance) = run(&db, &ctx, thread, None).await;
        assert!(service.available_help_case(&ctx, &source).await.is_err());
        assert!(service
            .create_ai_draft(&ctx, &source, Uuid::new_v4(), content(), "test")
            .await
            .is_err());
        let case = service
            .record_help_receipt(
                &ctx,
                &source,
                "search",
                HelpReceipt {
                    hit_count: hits,
                    corpus_versions: vec!["fixture".into()],
                    chunk_ids: vec![],
                },
            )
            .await
            .unwrap();
        assert_eq!(
            case,
            service
                .record_help_receipt(
                    &ctx,
                    &source,
                    "search",
                    HelpReceipt {
                        hit_count: hits,
                        corpus_versions: vec![],
                        chunk_ids: vec![]
                    }
                )
                .await
                .unwrap()
        );
        // 同 batch・未確定の案内では本人確認も下書きも進めない。
        assert!(service
            .help_report_intent(&ctx, thread, case)
            .await
            .is_err());
        assert!(service
            .create_ai_draft(&ctx, &source, case, content(), "test")
            .await
            .is_err());
        finish(&db, &ctx, &source, guidance).await;
        if hits > 0 {
            assert!(service
                .help_report_intent(&ctx, thread, case)
                .await
                .is_err());
            service
                .resolve_help_case(&ctx, thread, case, HelpResolution::Unresolved)
                .await
                .unwrap();
        }
        let bob = context(ctx.tenant_id(), ctx.org(), "bob");
        fga.write_tuple(
            &bob.subject(),
            Relation::Member,
            &ctx.ns().organization(ctx.org()),
        )
        .await
        .unwrap();
        fga.write_tuple(
            &bob.subject(),
            Relation::Viewer,
            &ctx.ns().thread(&thread.to_string()),
        )
        .await
        .unwrap();
        assert!(service.help_case(&bob, thread, case).await.is_err());
        assert!(service
            .resolve_help_case(&bob, thread, case, HelpResolution::Unresolved)
            .await
            .is_err());
        assert!(service
            .help_report_intent(&bob, thread, case)
            .await
            .is_err());
        assert!(service
            .help_case(&context(ctx.tenant_id(), "other", "alice"), thread, case)
            .await
            .is_err());
        service
            .help_report_intent(&ctx, thread, case)
            .await
            .unwrap();
        let (draft_run, answer) = run(&db, &ctx, thread, Some(guidance)).await;
        assert_eq!(
            service.available_help_case(&ctx, &draft_run).await.unwrap(),
            case
        );
        // resolved の撤回・期限切れ・自律/skill run への持ち越しも DB で拒否する。
        service
            .resolve_help_case(&ctx, thread, case, HelpResolution::Resolved)
            .await
            .unwrap();
        assert!(service
            .create_ai_draft(&ctx, &draft_run, case, content(), "test")
            .await
            .is_err());
        service
            .resolve_help_case(&ctx, thread, case, HelpResolution::Unresolved)
            .await
            .unwrap();
        // 本人の再確認後も、案内直後以外から report-intent を再設定できない。
        assert!(service
            .help_report_intent(&ctx, thread, case)
            .await
            .is_err());
        // 以降の冪等検証用に、作成済み run より前の本人 intent を fixture として復元する。
        sqlx::query("UPDATE help_resolution_case SET report_intent_at=now()-interval '1 minute' WHERE tenant_id=$1 AND org=$2 AND id=$3")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(case).execute(&db).await.unwrap();
        for (autonomous, pins) in [
            (true, serde_json::json!([])),
            (
                false,
                serde_json::json!([{"skill_id":"x","skill_version":1}]),
            ),
        ] {
            sqlx::query("UPDATE generation_run SET autonomous=$4,skill_pins=$5 WHERE tenant_id=$1 AND org=$2 AND run_id=$3")
                .bind(ctx.tenant_id()).bind(ctx.org()).bind(draft_run.run_id).bind(autonomous).bind(pins).execute(&db).await.unwrap();
            assert!(service
                .create_ai_draft(&ctx, &draft_run, case, content(), "test")
                .await
                .is_err());
        }
        sqlx::query("UPDATE generation_run SET autonomous=false,skill_pins='[]'::jsonb WHERE tenant_id=$1 AND org=$2 AND run_id=$3")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(draft_run.run_id).execute(&db).await.unwrap();
        sqlx::query("UPDATE help_resolution_case SET expires_at=now()-interval '1 minute' WHERE tenant_id=$1 AND org=$2 AND id=$3")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(case).execute(&db).await.unwrap();
        assert!(service
            .create_ai_draft(&ctx, &draft_run, case, content(), "test")
            .await
            .is_err());
        sqlx::query("UPDATE help_resolution_case SET expires_at=now()+interval '1 hour' WHERE tenant_id=$1 AND org=$2 AND id=$3")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(case).execute(&db).await.unwrap();
        let report = service
            .create_ai_draft(&ctx, &draft_run, case, content(), "test")
            .await
            .unwrap();
        assert!(service.get(&bob, report.id, "test").await.is_err());
        assert_eq!(
            report.id,
            service
                .create_ai_draft(&ctx, &draft_run, case, content(), "test")
                .await
                .unwrap()
                .id
        );
        assert!(service
            .create_ai_draft(
                &ctx,
                &HelpRun {
                    run_id: draft_run.run_id,
                    fencing: 2
                },
                case,
                content(),
                "test"
            )
            .await
            .is_err());
        finish(&db, &ctx, &draft_run, answer).await;
        let (later, _) = run(&db, &ctx, thread, Some(answer)).await;
        assert!(service.available_help_case(&ctx, &later).await.is_err());
        assert!(service
            .create_ai_draft(&ctx, &later, case, content(), "test")
            .await
            .is_err());
        // sibling に切替後は同じ thread でも流用不可。
        let (sibling, _) = run(&db, &ctx, thread, None).await;
        assert!(service.help_case(&ctx, thread, case).await.is_err());
        assert!(service
            .create_ai_draft(&ctx, &sibling, case, content(), "test")
            .await
            .is_err());
    }
}

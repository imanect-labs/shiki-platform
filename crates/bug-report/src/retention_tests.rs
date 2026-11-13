use super::*;
use crate::retention::Retention;

#[tokio::test]
async fn purge_removes_all_body_copies_and_fences_inflight_delivery() {
    let Some((db, fga, svc, ctx)) = setup().await else {
        return;
    };
    let draft = svc.create(&ctx, content(), "draft").await.unwrap();
    let submitted = queued(&svc, &ctx).await;
    let fresh = svc.create(&ctx, content(), "fresh").await.unwrap();
    let (sink, _) = stub(vec![]).await;
    let worker = worker(&db, fga, sink);
    let claim = worker
        .claim(ctx.tenant(), ctx.org())
        .await
        .unwrap()
        .unwrap();
    sqlx::query("UPDATE bug_report SET updated_at=now()-interval '31 days' WHERE tenant_id=$1 AND org=$2 AND id=$3")
        .bind(ctx.tenant_id()).bind(ctx.org()).bind(draft.id).execute(&db).await.unwrap();
    sqlx::query("UPDATE bug_report_submission SET consent_at=now()-interval '91 days' WHERE tenant_id=$1 AND org=$2 AND report_id=$3")
        .bind(ctx.tenant_id()).bind(ctx.org()).bind(submitted.id).execute(&db).await.unwrap();
    let retention = Retention::new(db.clone());
    // 本文を読む runtime role で実行する。
    let url = shiki_testenv::var("STORAGE_TEST_DATABASE_URL").unwrap();
    let runtime = PgPoolOptions::new()
        .after_connect(|c, _| {
            Box::pin(async move {
                sqlx::query("SET ROLE shiki_runtime").execute(c).await?;
                Ok(())
            })
        })
        .connect(&url)
        .await
        .unwrap();
    assert!(Retention::new(runtime).run_batch().await.unwrap() > 0);
    assert!(matches!(
        svc.get(&ctx, draft.id, "gone").await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        svc.get(&ctx, submitted.id, "gone").await,
        Err(Error::NotFound)
    ));
    assert!(svc.get(&ctx, fresh.id, "fresh").await.is_ok());
    assert!(svc
        .list(&ctx, false, None, "list")
        .await
        .unwrap()
        .items
        .iter()
        .all(|r| r.id != submitted.id && r.id != draft.id));
    assert!(matches!(
        worker.finish(&claim, Dispatch::Unknown).await,
        Err(Error::Conflict)
    ));
    assert!(worker
        .claim(ctx.tenant(), ctx.org())
        .await
        .unwrap()
        .is_none());
    let (payload, principal, reason, hash): (serde_json::Value,Option<serde_json::Value>,Option<String>,String) = sqlx::query_as("SELECT payload,principal,rejection_reason,payload_hash FROM bug_report_submission WHERE tenant_id=$1 AND org=$2 AND report_id=$3")
        .bind(ctx.tenant_id()).bind(ctx.org()).bind(submitted.id).fetch_one(&db).await.unwrap();
    assert_eq!(payload, serde_json::json!({}));
    assert!(principal.is_none());
    assert!(reason.is_none());
    assert!(!hash.is_empty());
    let commands: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM bug_report_command WHERE tenant_id=$1 AND org=$2 AND report_id=$3",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(submitted.id)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(commands, 0);
    assert_eq!(retention.sweep(ctx.tenant(), ctx.org()).await.unwrap(), 0);
}

#[tokio::test]
async fn tenant_stopping_blocks_queue_and_approval_off_does_not_release_pending() {
    let Some((db, fga, svc, ctx)) = setup().await else {
        return;
    };
    let queued = queued(&svc, &ctx).await;
    let (sink, _) = stub(vec![]).await;
    let w = worker(&db, fga, sink);
    sqlx::query(
        "INSERT INTO tenant (tenant_id,org,display_name,status) VALUES ($1,$2,'dummy','deleting')",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .execute(&db)
    .await
    .unwrap();
    for status in ["deleting", "deleted"] {
        sqlx::query("UPDATE tenant SET status=$2 WHERE tenant_id=$1")
            .bind(ctx.tenant_id())
            .bind(status)
            .execute(&db)
            .await
            .unwrap();
        assert!(w.claim(ctx.tenant(), ctx.org()).await.unwrap().is_none());
        // 専用DB内の他テストのscopeもあるため run_batch の件数は固定しない。
        assert_eq!(
            svc.get(&ctx, queued.id, "get").await.unwrap().status,
            Status::Queued
        );
    }
    let mut p = svc.policy(&ctx, "p").await.unwrap();
    p.require_approval = true;
    svc.set_policy(&ctx, p, "p").await.unwrap();
    let d = svc.create(&ctx, content(), "draft").await.unwrap();
    let p = svc.policy(&ctx, "p").await.unwrap();
    let pending = svc
        .command(&ctx, submit(d.id, d.rev, p.rev, true, "pending"), "s")
        .await
        .unwrap();
    let mut p = svc.policy(&ctx, "p").await.unwrap();
    p.require_approval = false;
    svc.set_policy(&ctx, p, "p").await.unwrap();
    assert_eq!(
        svc.get(&ctx, pending.id, "get").await.unwrap().status,
        Status::PendingApproval
    );
}

#[tokio::test]
async fn rolling_daily_submission_limit_and_cancellation_race() {
    let Some((db, fga, svc, ctx)) = setup().await else {
        return;
    };
    // 作成枠の上限とは独立して提出枠を検証する。
    let extra = svc.create(&ctx, content(), "extra").await.unwrap();
    for i in 0..20 {
        if i % 5 == 0 {
            sqlx::query("UPDATE bug_report_submission SET consent_at=now()-interval '2 hours' WHERE tenant_id=$1 AND org=$2")
                .bind(ctx.tenant_id()).bind(ctx.org()).execute(&db).await.unwrap();
        }
        if i == 19 {
            sqlx::query("UPDATE bug_report SET created_at=now()-interval '2 days' WHERE tenant_id=$1 AND org=$2")
                .bind(ctx.tenant_id()).bind(ctx.org()).execute(&db).await.unwrap();
        }
        queued(&svc, &ctx).await;
    }
    sqlx::query("UPDATE bug_report_submission SET consent_at=now()-interval '2 hours' WHERE tenant_id=$1 AND org=$2")
        .bind(ctx.tenant_id()).bind(ctx.org()).execute(&db).await.unwrap();
    let p = svc.policy(&ctx, "p").await.unwrap();
    assert!(matches!(
        svc.command(
            &ctx,
            submit(extra.id, extra.rev, p.rev, true, "21st"),
            "limit"
        )
        .await,
        Err(Error::RateLimited)
    ));
    // 別 org で取り下げと claim を実際に競合させる。どちらかだけが先に進める。
    let race_ctx = context(ctx.tenant_id(), "race", "alice");
    for relation in [Relation::Member, Relation::Admin] {
        fga.write_tuple(
            &race_ctx.subject(),
            relation,
            &race_ctx.ns().organization(race_ctx.org()),
        )
        .await
        .unwrap();
    }
    svc.set_policy(
        &race_ctx,
        crate::BugReportPolicy {
            require_approval: false,
            allow_content_sharing: true,
            ..Default::default()
        },
        "p",
    )
    .await
    .unwrap();
    let r = queued(&svc, &race_ctx).await;
    let (sink, _) = stub(vec![]).await;
    let w = worker(&db, fga, sink);
    let (cancel, claim) = tokio::join!(
        svc.command(
            &race_ctx,
            CommandRequest {
                id: r.id,
                expected_rev: r.rev,
                key: "cancel".into(),
                command: Command::Cancel
            },
            "race"
        ),
        w.claim(race_ctx.tenant(), race_ctx.org())
    );
    match claim.unwrap() {
        Some(claim) => {
            assert!(cancel.is_err());
            w.finish(&claim, Dispatch::Unknown).await.unwrap();
        }
        None => {
            assert_eq!(cancel.unwrap().status, Status::Cancelled);
        }
    }
}

#[tokio::test]
async fn admin_detail_rejects_report_whose_submission_body_was_purged() {
    let Some((db, _fga, svc, ctx)) = setup().await else {
        return;
    };
    // 提出 snapshot だけが保持期限で消え、報告本体（再下書きの本文）は残った状態。
    let report = queued(&svc, &ctx).await;
    assert!(svc.detail(&ctx, report.id, true, "before").await.is_ok());
    sqlx::query("UPDATE bug_report_submission SET payload='{}'::jsonb, body_purged_at=now() WHERE tenant_id=$1 AND org=$2 AND report_id=$3")
        .bind(ctx.tenant_id()).bind(ctx.org()).bind(report.id).execute(&db).await.unwrap();
    sqlx::query("UPDATE bug_report SET status='cancelled' WHERE tenant_id=$1 AND org=$2 AND id=$3")
        .bind(ctx.tenant_id()).bind(ctx.org()).bind(report.id).execute(&db).await.unwrap();
    // 未共有の本文を管理者へ返さない。
    assert!(matches!(
        svc.detail(&ctx, report.id, true, "after").await,
        Err(Error::NotFound)
    ));
}

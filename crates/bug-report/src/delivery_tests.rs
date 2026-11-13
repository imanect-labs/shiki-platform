#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::pedantic,
    clippy::too_many_lines
)]
use crate::{
    BugReportService, BugReportStatus as Status, BugReportStore, Category, Command, CommandRequest,
    Error, ReportContent,
};
use authz::{
    AuthContext, AuthzClient, OpenFgaClient, OpenFgaConfig, Principal, PrincipalKind, Relation,
};
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::sync::Arc;
use uuid::Uuid;

fn context(tenant: &str, org: &str, user: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: user.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: None,
        },
        org.into(),
        authz::TenantId::parse(tenant).unwrap(),
    )
}
fn content() -> ReportContent {
    ReportContent {
        category: Category::Bug,
        title: "表示の不具合".into(),
        symptom: "秘密の本文".into(),
        steps_tried: "再読込".into(),
        expected_behavior: "表示される".into(),
    }
}
async fn setup() -> Option<(PgPool, Arc<OpenFgaClient>, BugReportService, AuthContext)> {
    let Ok(url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        return None;
    };
    let Ok(base_url) = shiki_testenv::var("OPENFGA_TEST_URL") else {
        return None;
    };
    setup_at(&url, &base_url).await
}
async fn setup_at(
    url: &str,
    base_url: &str,
) -> Option<(PgPool, Arc<OpenFgaClient>, BugReportService, AuthContext)> {
    let db = PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .unwrap();
    sqlx::migrate!("../../migrations").run(&db).await.unwrap();
    let fga = Arc::new(
        OpenFgaClient::connect(
            reqwest::Client::new(),
            &OpenFgaConfig {
                base_url: base_url.to_string(),
                store_name: format!("bug-report-{}", Uuid::new_v4()),
            },
            &serde_json::from_str(include_str!("../../authz/model/authorization-model.json"))
                .unwrap(),
        )
        .await
        .unwrap(),
    );
    let ctx = context(&format!("t{}", Uuid::new_v4().simple()), "org", "alice");
    fga.write_tuple(
        &ctx.subject(),
        Relation::Member,
        &ctx.ns().organization(ctx.org()),
    )
    .await
    .unwrap();
    fga.write_tuple(
        &ctx.subject(),
        Relation::Admin,
        &ctx.ns().organization(ctx.org()),
    )
    .await
    .unwrap();
    let runtime = PgPoolOptions::new()
        .max_connections(5)
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("SET ROLE shiki_runtime").execute(conn).await?;
                Ok(())
            })
        })
        .connect(url)
        .await
        .unwrap();
    let service =
        BugReportService::new(BugReportStore::new(runtime), fga.clone()).with_github_destination(1);
    service
        .set_policy(
            &ctx,
            crate::BugReportPolicy {
                require_approval: false,
                allow_content_sharing: true,
                ..Default::default()
            },
            "policy",
        )
        .await
        .unwrap();
    Some((db, fga, service, ctx))
}
fn submit(id: Uuid, rev: i64, policy_rev: i64, consent: bool, key: &str) -> CommandRequest {
    CommandRequest {
        id,
        expected_rev: rev,
        key: key.into(),
        command: Command::Submit {
            policy_rev,
            consent,
            notice_version: "bug-report-v1".into(),
        },
    }
}

use super::delivery::*;
use crate::github::{
    tests::{issue, response, stub},
    Dispatch,
};
async fn queued(svc: &BugReportService, ctx: &AuthContext) -> crate::BugReport {
    let r = svc.create(ctx, content(), "create").await.unwrap();
    let p = svc.policy(ctx, "policy").await.unwrap();
    let r = svc
        .command(
            ctx,
            submit(r.id, r.rev, p.rev, true, &Uuid::new_v4().to_string()),
            "submit",
        )
        .await
        .unwrap();
    assert_eq!(r.status, Status::Queued);
    r
}
fn worker(
    db: &PgPool,
    fga: Arc<OpenFgaClient>,
    sink: crate::github::GitHubIssueSink,
) -> DeliveryWorker {
    DeliveryWorker::new(db.clone(), fga, Arc::new(sink))
}
#[tokio::test]
async fn transactional_dispatch_and_reconcile_after_crash() {
    let Some((db, fga, svc, ctx)) = setup().await else {
        return;
    };
    let r = queued(&svc, &ctx).await;
    let (sink, mut requests) = stub(vec![response(201, "", &issue(10, ""))]).await;
    let w = worker(&db, fga.clone(), sink);
    let claim = w.claim(ctx.tenant(), ctx.org()).await.unwrap().unwrap();
    assert!(!claim.reconcile);
    assert!(matches!(
        w.sink.dispatch(&claim.report, &claim.identity).await,
        Dispatch::Sent { .. }
    ));
    assert!(requests.recv().await.unwrap().starts_with("POST "));
    // GitHub 成功後、finish 前にクラッシュ。別 worker が lease を取得する。
    sqlx::query("UPDATE bug_report_delivery SET lease_until=now()-interval '1 second' WHERE tenant_id=$1 AND org=$2 AND id=$3").bind(ctx.tenant_id()).bind(ctx.org()).bind(claim.identity.id).execute(&db).await.unwrap();
    let marker = format!("<!-- shiki-delivery:{} -->", claim.identity.id);
    let (sink, mut requests) = stub(vec![response(
        200,
        "",
        &format!("[{}]", issue(10, &marker)),
    )])
    .await;
    let next = worker(&db, fga, sink);
    let recovered = next.claim(ctx.tenant(), ctx.org()).await.unwrap().unwrap();
    assert!(recovered.reconcile);
    assert!(recovered.fence > claim.fence);
    assert!(matches!(
        w.finish(&claim, Dispatch::Unknown).await,
        Err(Error::Conflict)
    ));
    next.finish(&recovered, next.sink.lookup(&recovered.identity).await)
        .await
        .unwrap();
    assert!(requests.recv().await.unwrap().starts_with("GET "));
    assert_eq!(
        svc.get(&ctx, r.id, "get").await.unwrap().status,
        Status::Sent
    );
    let (outcome, url): (String, String) = sqlx::query_as(
        "SELECT outcome,url FROM bug_report_delivery WHERE tenant_id=$1 AND org=$2 AND id=$3",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(claim.identity.id)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(outcome, "sent");
    assert!(url.ends_with("/issues/10"));
}
#[tokio::test]
async fn policy_changes_and_tenant_org_isolation_stop_post() {
    let Some((db, fga, svc, ctx)) = setup().await else {
        return;
    };
    let (sink, _requests) = stub(vec![]).await;
    let w = worker(&db, fga, sink);
    for approval in [false, true] {
        let r = queued(&svc, &ctx).await;
        assert!(w.claim(ctx.tenant(), "other-org").await.unwrap().is_none());
        assert!(w
            .claim(&authz::TenantId::parse("other-tenant").unwrap(), ctx.org())
            .await
            .unwrap()
            .is_none());
        let mut p = svc.policy(&ctx, "p").await.unwrap();
        if approval {
            p.require_approval = true;
        } else {
            p.allow_content_sharing = false;
        }
        svc.set_policy(&ctx, p, "p").await.unwrap();
        assert!(w.claim(ctx.tenant(), ctx.org()).await.unwrap().is_none());
        assert_eq!(
            svc.get(&ctx, r.id, "get").await.unwrap().status,
            Status::Held
        );
        let mut p = svc.policy(&ctx, "p").await.unwrap();
        p.require_approval = false;
        p.allow_content_sharing = true;
        svc.set_policy(&ctx, p, "p").await.unwrap();
    }
}
#[tokio::test]
async fn retry_limit_unknown_and_database_failure_preserve_lease() {
    let Some((db, fga, svc, ctx)) = setup().await else {
        return;
    };
    let r = queued(&svc, &ctx).await;
    let (sink, _requests) = stub(vec![]).await;
    let w = worker(&db, fga, sink);
    let claim = w.claim(ctx.tenant(), ctx.org()).await.unwrap().unwrap();
    w.finish(
        &claim,
        Dispatch::Retry {
            after: 600,
            code: "rate_limit",
        },
    )
    .await
    .unwrap();
    let delay:bool=sqlx::query_scalar("SELECT next_attempt_at>=now()+interval '590 seconds' FROM bug_report_delivery WHERE tenant_id=$1 AND org=$2 AND id=$3").bind(ctx.tenant_id()).bind(ctx.org()).bind(claim.identity.id).fetch_one(&db).await.unwrap();
    assert!(delay);
    assert!(w.claim(ctx.tenant(), ctx.org()).await.unwrap().is_none());
    sqlx::query("UPDATE bug_report_delivery SET next_attempt_at=now() WHERE tenant_id=$1 AND org=$2 AND id=$3").bind(ctx.tenant_id()).bind(ctx.org()).bind(claim.identity.id).execute(&db).await.unwrap();
    let claim = w.claim(ctx.tenant(), ctx.org()).await.unwrap().unwrap();
    // GitHub成功後、監査のDB障害により finish 全体がrollbackする。
    let function = format!("bug_delivery_fail_{}", Uuid::new_v4().simple());
    let ddl=format!("CREATE FUNCTION {function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.tenant_id='{}' AND NEW.action='bug_report.sent' THEN RAISE EXCEPTION 'audit failure'; END IF; RETURN NEW; END $$",ctx.tenant_id());
    sqlx::query(&ddl).execute(&db).await.unwrap();
    sqlx::query(&format!("CREATE TRIGGER {function} BEFORE INSERT ON audit_log FOR EACH ROW EXECUTE FUNCTION {function}()" )).execute(&db).await.unwrap();
    let failed = w
        .finish(
            &claim,
            Dispatch::Sent {
                id: "42".into(),
                url: "https://github.com/imanect-labs/shiki-test/issues/42".into(),
            },
        )
        .await;
    sqlx::query(&format!("DROP TRIGGER {function} ON audit_log"))
        .execute(&db)
        .await
        .unwrap();
    sqlx::query(&format!("DROP FUNCTION {function}()"))
        .execute(&db)
        .await
        .unwrap();
    assert!(matches!(failed, Err(Error::Audit(_))));
    assert_eq!(
        svc.get(&ctx, r.id, "get").await.unwrap().status,
        Status::Sending
    );
    w.finish(&claim, Dispatch::Unknown).await.unwrap();
    assert_eq!(
        svc.get(&ctx, r.id, "get").await.unwrap().status,
        Status::DeliveryUnknown
    );
    sqlx::query("UPDATE bug_report_delivery SET next_attempt_at=now() WHERE tenant_id=$1 AND org=$2 AND id=$3").bind(ctx.tenant_id()).bind(ctx.org()).bind(claim.identity.id).execute(&db).await.unwrap();
    let claim = w.claim(ctx.tenant(), ctx.org()).await.unwrap().unwrap();
    assert!(claim.reconcile);
    w.finish(&claim, Dispatch::Unknown).await.unwrap();
    assert_eq!(
        svc.get(&ctx, r.id, "get").await.unwrap().status,
        Status::DeliveryUnknown
    );
}

#[tokio::test]
async fn bounded_batch_runs_as_runtime_role_and_stops_at_retry_limit() {
    let Ok(url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        return;
    };
    let Ok(fga_url) = shiki_testenv::var("OPENFGA_TEST_URL") else {
        return;
    };
    let admin = PgPoolOptions::new().connect(&url).await.unwrap();
    let database = format!("bug_delivery_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {database}"))
        .execute(&admin)
        .await
        .unwrap();
    let mut target = reqwest::Url::parse(&url).unwrap();
    target.set_path(&database);
    let (db, fga, svc, ctx) = setup_at(target.as_str(), &fga_url).await.unwrap();
    let r = queued(&svc, &ctx).await;
    let runtime = PgPoolOptions::new()
        .max_connections(5)
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("SET ROLE shiki_runtime").execute(conn).await?;
                Ok(())
            })
        })
        .connect(target.as_str())
        .await
        .unwrap();
    let (sink, mut requests) = stub(vec![response(201, "", &issue(20, ""))]).await;
    let w = worker(&runtime, fga.clone(), sink);
    assert_eq!(w.run_batch().await.unwrap(), 1);
    assert!(requests.recv().await.unwrap().starts_with("POST "));
    assert_eq!(
        svc.get(&ctx, r.id, "get").await.unwrap().status,
        Status::Sent
    );
    assert_eq!(w.run_batch().await.unwrap(), 0);
    let r = queued(&svc, &ctx).await;
    let claim = w.claim(ctx.tenant(), ctx.org()).await.unwrap().unwrap();
    sqlx::query(
        "UPDATE bug_report_delivery SET attempt_count=5 WHERE tenant_id=$1 AND org=$2 AND id=$3",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(claim.identity.id)
    .execute(&db)
    .await
    .unwrap();
    w.finish(
        &claim,
        Dispatch::Retry {
            after: 60,
            code: "connect",
        },
    )
    .await
    .unwrap();
    assert_eq!(
        svc.get(&ctx, r.id, "get").await.unwrap().status,
        Status::Failed
    );
    let r = queued(&svc, &ctx).await;
    let mut p = svc.policy(&ctx, "p").await.unwrap();
    p.require_approval = true;
    svc.set_policy(&ctx, p, "p").await.unwrap();
    assert_eq!(
        svc.get(&ctx, r.id, "get").await.unwrap().status,
        Status::Held
    );
    // 承認ONの新規提出は approve と同じtransactionで初めてdeliveryを作る。
    let draft = svc.create(&ctx, content(), "new").await.unwrap();
    let p = svc.policy(&ctx, "p").await.unwrap();
    let pending = svc
        .command(
            &ctx,
            submit(draft.id, draft.rev, p.rev, true, "approved"),
            "submit",
        )
        .await
        .unwrap();
    assert_eq!(pending.status, Status::PendingApproval);
    let approved = svc
        .command(
            &ctx,
            CommandRequest {
                id: pending.id,
                expected_rev: pending.rev,
                key: "approve".into(),
                command: Command::Approve,
            },
            "approve",
        )
        .await
        .unwrap();
    assert_eq!(approved.status, Status::Queued);
    let claim = w.claim(ctx.tenant(), ctx.org()).await.unwrap().unwrap();
    w.finish(&claim, Dispatch::Failed("forbidden"))
        .await
        .unwrap();
    assert_eq!(
        svc.get(&ctx, approved.id, "get").await.unwrap().status,
        Status::Failed
    );
    runtime.close().await;
    db.close().await;
    drop(svc);
    drop(w);
    sqlx::query(&format!("DROP DATABASE {database} WITH (FORCE)"))
        .execute(&admin)
        .await
        .unwrap();
}

#[tokio::test]
async fn revoked_membership_and_changed_destination_stop_unstarted_delivery() {
    let Some((db, fga, svc, ctx)) = setup().await else {
        return;
    };
    let r = queued(&svc, &ctx).await;
    let (sink, _requests) = stub(vec![]).await;
    let w = worker(&db, fga.clone(), sink);
    fga.delete_tuple(
        &ctx.subject(),
        Relation::Member,
        &ctx.ns().organization(ctx.org()),
    )
    .await
    .unwrap();
    assert!(w.claim(ctx.tenant(), ctx.org()).await.unwrap().is_none());
    fga.write_tuple(
        &ctx.subject(),
        Relation::Member,
        &ctx.ns().organization(ctx.org()),
    )
    .await
    .unwrap();
    assert_eq!(
        svc.get(&ctx, r.id, "get").await.unwrap().status,
        Status::Held
    );
    let r = queued(&svc, &ctx).await;
    sqlx::query("UPDATE bug_report_submission SET sink_config_version=2 WHERE tenant_id=$1 AND org=$2 AND report_id=$3").bind(ctx.tenant_id()).bind(ctx.org()).bind(r.id).execute(&db).await.unwrap();
    assert!(w.claim(ctx.tenant(), ctx.org()).await.unwrap().is_none());
    assert_eq!(
        svc.get(&ctx, r.id, "get").await.unwrap().status,
        Status::Held
    );
}

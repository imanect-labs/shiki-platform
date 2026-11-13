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
            code: "connect",
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
        .max_connections(2)
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
    // 旧 submission の NULL principal は approve しても held、配送行を作らない。
    let draft = svc.create(&ctx, content(), "legacy").await.unwrap();
    let pending = svc
        .command(
            &ctx,
            submit(draft.id, draft.rev, p.rev, true, "legacy-submit"),
            "submit",
        )
        .await
        .unwrap();
    sqlx::query("UPDATE bug_report_submission SET principal=NULL WHERE report_id=$1")
        .bind(pending.id)
        .execute(&db)
        .await
        .unwrap();
    let held = svc
        .command(
            &ctx,
            CommandRequest {
                id: pending.id,
                expected_rev: pending.rev,
                key: "legacy-approve".into(),
                command: Command::Approve,
            },
            "approve",
        )
        .await
        .unwrap();
    assert_eq!(held.status, Status::Held);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM bug_report_delivery WHERE report_id=$1")
            .bind(held.id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(count, 0);

    let mut policy = svc.policy(&ctx, "p").await.unwrap();
    policy.require_approval = false;
    svc.set_policy(&ctx, policy, "p").await.unwrap();
    // ここからは配送の検証。先行シナリオの提出を時間上限の窓から外す。
    sqlx::query(
        "UPDATE bug_report_submission SET consent_at=now()-interval '2 hours' WHERE tenant_id=$1",
    )
    .bind(ctx.tenant_id())
    .execute(&db)
    .await
    .unwrap();
    // 上限8件を超える不正 scope が先行しても、正常な scope を列挙する。
    // SQL NULL と JSON null の両方を除外する。
    for i in 0..10 {
        let bad_ctx = context(ctx.tenant_id(), &format!("legacy-{i}"), "alice");
        for relation in [Relation::Member, Relation::Admin] {
            fga.write_tuple(
                &bad_ctx.subject(),
                relation,
                &bad_ctx.ns().organization(bad_ctx.org()),
            )
            .await
            .unwrap();
        }
        svc.set_policy(
            &bad_ctx,
            crate::BugReportPolicy {
                require_approval: false,
                allow_content_sharing: true,
                ..Default::default()
            },
            "p",
        )
        .await
        .unwrap();
        let bad = queued(&svc, &bad_ctx).await;
        sqlx::query("UPDATE bug_report_submission SET principal=$2 WHERE report_id=$1")
            .bind(bad.id)
            .bind(if i % 2 == 0 {
                None
            } else {
                Some(serde_json::Value::Null)
            })
            .execute(&db)
            .await
            .unwrap();
    }
    // 後続の別 tenant の正常行を送れる。
    let other = context(&format!("t{}", Uuid::new_v4().simple()), "org", "bob");
    for relation in [Relation::Member, Relation::Admin] {
        fga.write_tuple(
            &other.subject(),
            relation,
            &other.ns().organization(other.org()),
        )
        .await
        .unwrap();
    }
    svc.set_policy(
        &other,
        crate::BugReportPolicy {
            require_approval: false,
            allow_content_sharing: true,
            ..Default::default()
        },
        "p",
    )
    .await
    .unwrap();
    let healthy = queued(&svc, &other).await;
    let (sink, mut requests) = stub(vec![response(201, "", &issue(21, ""))]).await;
    let replica = worker(&runtime, fga.clone(), sink);
    assert_eq!(replica.run_batch().await.unwrap(), 1);
    assert!(requests.recv().await.unwrap().starts_with("POST "));
    assert_eq!(
        svc.get(&other, healthy.id, "get").await.unwrap().status,
        Status::Sent
    );

    let first = queued(&svc, &ctx).await;
    let claim = w.claim(ctx.tenant(), ctx.org()).await.unwrap().unwrap();
    assert_eq!(claim.identity.report_id, first.id);
    // 最終試行でも共有待機を保存する。
    sqlx::query("UPDATE bug_report_delivery SET attempt_count=5 WHERE id=$1")
        .bind(claim.identity.id)
        .execute(&db)
        .await
        .unwrap();
    w.finish(
        &claim,
        Dispatch::Retry {
            after: 600,
            code: "rate_limit",
        },
    )
    .await
    .unwrap();
    let waiting = queued(&svc, &other).await;
    assert!(replica
        .claim(other.tenant(), other.org())
        .await
        .unwrap()
        .is_none());
    assert_eq!(replica.run_batch().await.unwrap(), 0);
    sqlx::query(
        "UPDATE bug_report_destination_cooldown SET next_attempt_at=now()-interval '1 second'",
    )
    .execute(&db)
    .await
    .unwrap();
    let resumed = replica
        .claim(other.tenant(), other.org())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resumed.identity.report_id, waiting.id);
    replica.finish(&resumed, Dispatch::Unknown).await.unwrap();
    sqlx::query("UPDATE bug_report_delivery SET next_attempt_at=now() WHERE id=$1")
        .bind(resumed.identity.id)
        .execute(&db)
        .await
        .unwrap();
    let reconcile = replica
        .claim(other.tenant(), other.org())
        .await
        .unwrap()
        .unwrap();
    assert!(reconcile.reconcile);
    replica
        .finish(
            &reconcile,
            Dispatch::Retry {
                after: 600,
                code: "rate_limit",
            },
        )
        .await
        .unwrap();
    assert_eq!(
        svc.get(&other, waiting.id, "get").await.unwrap().status,
        Status::DeliveryUnknown
    );
    assert!(w.claim(ctx.tenant(), ctx.org()).await.unwrap().is_none());
    // 待機期限を独立 commit した後に監査が失敗しても、別 replica は送信しない。
    sqlx::query(
        "UPDATE bug_report_destination_cooldown SET next_attempt_at=now()-interval '1 second'",
    )
    .execute(&db)
    .await
    .unwrap();
    let failing = queued(&svc, &ctx).await;
    let waiting_after_failure = queued(&svc, &other).await;
    let function = format!("bug_delivery_fail_{}", Uuid::new_v4().simple());
    let ddl = format!("CREATE FUNCTION {function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.tenant_id='{}' AND NEW.action='bug_report.failed' THEN RAISE EXCEPTION 'audit failure'; END IF; RETURN NEW; END $$", ctx.tenant_id());
    sqlx::query(&ddl).execute(&db).await.unwrap();
    sqlx::query(&format!("CREATE TRIGGER {function} BEFORE INSERT ON audit_log FOR EACH ROW EXECUTE FUNCTION {function}()"))
        .execute(&db).await.unwrap();
    let (sink, mut rate_requests) = stub(vec![response(429, "Retry-After: 600\r\n", "{}")]).await;
    let rate_worker = worker(&runtime, fga.clone(), sink);
    // serial 接続を保持した実バッチを pool=2 で走らせ、接続の三重取得を検出。
    let failed =
        tokio::time::timeout(std::time::Duration::from_secs(10), rate_worker.run_batch()).await;
    sqlx::query(&format!("DROP TRIGGER {function} ON audit_log"))
        .execute(&db)
        .await
        .unwrap();
    sqlx::query(&format!("DROP FUNCTION {function}()"))
        .execute(&db)
        .await
        .unwrap();
    assert!(matches!(failed.unwrap(), Err(Error::Audit(_))));
    assert!(rate_requests.recv().await.unwrap().starts_with("POST "));
    assert_eq!(
        svc.get(&ctx, failing.id, "get").await.unwrap().status,
        Status::Sending
    );
    let cooling: bool = sqlx::query_scalar("SELECT next_attempt_at>now()+interval '590 seconds' FROM bug_report_destination_cooldown WHERE installation_id='2'")
        .fetch_one(&db).await.unwrap();
    assert!(cooling);
    assert_eq!(replica.run_batch().await.unwrap(), 0);
    assert!(replica
        .claim(other.tenant(), other.org())
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        svc.get(&other, waiting_after_failure.id, "get")
            .await
            .unwrap()
            .status,
        Status::Queued
    );
    // 待機保存自体が失敗した場合、そのバッチの以降の配送へ進まない。
    sqlx::query(
        "UPDATE bug_report_destination_cooldown SET next_attempt_at=now()-interval '1 second'",
    )
    .execute(&db)
    .await
    .unwrap();
    let later_ctx = context(ctx.tenant_id(), "after-cooldown-failure", "alice");
    for relation in [Relation::Member, Relation::Admin] {
        fga.write_tuple(
            &later_ctx.subject(),
            relation,
            &later_ctx.ns().organization(later_ctx.org()),
        )
        .await
        .unwrap();
    }
    svc.set_policy(
        &later_ctx,
        crate::BugReportPolicy {
            require_approval: false,
            allow_content_sharing: true,
            ..Default::default()
        },
        "p",
    )
    .await
    .unwrap();
    let later = queued(&svc, &later_ctx).await;
    sqlx::query(&format!("CREATE FUNCTION {function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'cooldown failure'; END $$"))
        .execute(&db).await.unwrap();
    sqlx::query(&format!("CREATE TRIGGER {function} BEFORE INSERT OR UPDATE ON bug_report_destination_cooldown FOR EACH ROW EXECUTE FUNCTION {function}()"))
        .execute(&db).await.unwrap();
    let (sink, mut blocked_requests) =
        stub(vec![response(429, "Retry-After: 600\r\n", "{}")]).await;
    let blocked_worker = worker(&runtime, fga.clone(), sink);
    let failed = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        blocked_worker.run_batch(),
    )
    .await;
    sqlx::query(&format!(
        "DROP TRIGGER {function} ON bug_report_destination_cooldown"
    ))
    .execute(&db)
    .await
    .unwrap();
    sqlx::query(&format!("DROP FUNCTION {function}()"))
        .execute(&db)
        .await
        .unwrap();
    assert!(matches!(failed.unwrap(), Err(Error::Database(_))));
    assert!(blocked_requests.recv().await.unwrap().starts_with("POST "));
    assert_eq!(
        svc.get(&later_ctx, later.id, "get").await.unwrap().status,
        Status::Queued
    );
    let attempts: i32 = sqlx::query_scalar("SELECT attempt_count FROM bug_report_delivery WHERE tenant_id=$1 AND org=$2 AND report_id=$3")
        .bind(later_ctx.tenant_id()).bind(later_ctx.org()).bind(later.id).fetch_one(&db).await.unwrap();
    assert_eq!(attempts, 0);
    drop(blocked_worker);
    drop(rate_worker);
    drop(replica);
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

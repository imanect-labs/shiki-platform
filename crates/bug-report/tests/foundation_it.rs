#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::pedantic,
    clippy::too_many_lines
)]
use authz::{
    AuthContext, AuthzClient, OpenFgaClient, OpenFgaConfig, Principal, PrincipalKind, Relation,
};
use shiki_bug_report::{
    BugReportService, BugReportStatus as Status, BugReportStore, Category, Command, CommandRequest,
    Error, ReportContent,
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
    let db = PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .unwrap();
    sqlx::migrate!("../../migrations").run(&db).await.unwrap();
    let fga = Arc::new(
        OpenFgaClient::connect(
            reqwest::Client::new(),
            &OpenFgaConfig {
                base_url,
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
        .connect(&url)
        .await
        .unwrap();
    let service = BugReportService::new(BugReportStore::new(runtime), fga.clone());
    // 自由記述を使う fixture org はサービスで明示許可する。
    let mut policy = service.policy(&ctx, "fixture").await.unwrap();
    policy.allow_content_sharing = true;
    service.set_policy(&ctx, policy, "fixture").await.unwrap();
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
#[tokio::test]
async fn authorization_revision_gates_and_idempotency() {
    let Some((db, fga, svc, ctx)) = setup().await else {
        return;
    };
    let r = svc.create(&ctx, content(), "create").await.unwrap();
    let bob = context(ctx.tenant_id(), ctx.org(), "bob");
    fga.write_tuple(
        &bob.subject(),
        Relation::Member,
        &bob.ns().organization(bob.org()),
    )
    .await
    .unwrap();
    let admin = context(ctx.tenant_id(), ctx.org(), "admin");
    fga.write_tuple(
        &admin.subject(),
        Relation::Member,
        &admin.ns().organization(admin.org()),
    )
    .await
    .unwrap();
    fga.write_tuple(
        &admin.subject(),
        Relation::Admin,
        &admin.ns().organization(admin.org()),
    )
    .await
    .unwrap();
    assert!(matches!(
        svc.get(&admin, r.id, "draft").await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        svc.get(&bob, r.id, "draft").await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        svc.update(&bob, r.id, 1, content(), "edit").await,
        Err(Error::NotFound)
    ));
    for other in [
        context(ctx.tenant_id(), "other", "alice"),
        context("other-tenant", ctx.org(), "alice"),
    ] {
        fga.write_tuple(
            &other.subject(),
            Relation::Member,
            &other.ns().organization(other.org()),
        )
        .await
        .unwrap();
        assert!(matches!(
            svc.get(&other, r.id, "cross").await,
            Err(Error::NotFound)
        ));
    }
    assert!(matches!(
        svc.command(&ctx, submit(r.id, 0, 1, true, "stale"), "submit")
            .await,
        Err(Error::Conflict)
    ));
    assert!(matches!(
        svc.command(&ctx, submit(r.id, 1, 9, true, "policy"), "submit")
            .await,
        Err(Error::Conflict)
    ));
    let r = svc
        .command(&ctx, submit(r.id, 1, 1, true, "once"), "submit")
        .await
        .unwrap();
    assert_eq!(r.status, Status::Held);
    assert_eq!(
        svc.command(&ctx, submit(r.id, 1, 1, true, "once"), "replay")
            .await
            .unwrap()
            .rev,
        r.rev
    );
    assert!(matches!(
        svc.command(&ctx, submit(r.id, 1, 1, false, "once"), "different")
            .await,
        Err(Error::Conflict)
    ));
    assert!(matches!(
        svc.command(
            &bob,
            CommandRequest {
                id: r.id,
                expected_rev: r.rev,
                key: "approve".into(),
                command: Command::Approve
            },
            "approve"
        )
        .await,
        Err(Error::Forbidden)
    ));
    assert_eq!(
        svc.get(&admin, r.id, "view").await.unwrap().content.symptom,
        "秘密の本文"
    );
    let mut policy = svc.policy(&ctx, "policy").await.unwrap();
    policy.allow_content_sharing = true;
    for approval in [true, false] {
        policy.require_approval = approval;
        policy = svc.set_policy(&ctx, policy, "policy").await.unwrap();
        for consent in [false, true] {
            let r = svc.create(&ctx, content(), "create").await.unwrap();
            let r = svc
                .command(
                    &ctx,
                    submit(r.id, 1, policy.rev, consent, &r.id.to_string()),
                    "submit",
                )
                .await
                .unwrap();
            assert_eq!(r.status, Status::Held);
            let shared = svc.get(&admin, r.id, "snapshot").await.unwrap();
            assert_eq!(!shared.content.symptom.is_empty(), consent);
        }
    }
    let r = svc.create(&ctx, content(), "create").await.unwrap();
    assert!(matches!(
        svc.command(&ctx, submit(r.id, 1, policy.rev, true, "sixth"), "limit")
            .await,
        Err(Error::RateLimited)
    ));
    fga.delete_tuple(
        &ctx.subject(),
        Relation::Member,
        &ctx.ns().organization(ctx.org()),
    )
    .await
    .unwrap();
    assert!(matches!(
        svc.get(&ctx, r.id, "left").await,
        Err(Error::Forbidden)
    ));
    // 監査 metadata と command 台帳に自由記述を複製しない。
    let leak: bool=sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM audit_log WHERE tenant_id=$1 AND org=$2 AND metadata::text LIKE '%秘密の本文%')").bind(ctx.tenant_id()).bind(ctx.org()).fetch_one(&db).await.unwrap();
    assert!(!leak);
}
#[tokio::test]
async fn draft_daily_limit_and_revision_race() {
    let Some((_db, _fga, svc, ctx)) = setup().await else {
        return;
    };
    let r = svc.create(&ctx, content(), "first").await.unwrap();
    let (a, b) = tokio::join!(
        svc.update(&ctx, r.id, 1, content(), "race1"),
        svc.update(&ctx, r.id, 1, content(), "race2")
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let r = svc.get(&ctx, r.id, "get").await.unwrap();
    let r = svc
        .command(&ctx, submit(r.id, r.rev, 1, true, "submit"), "submit")
        .await
        .unwrap();
    let r = svc
        .command(
            &ctx,
            CommandRequest {
                id: r.id,
                expected_rev: r.rev,
                key: "reopen".into(),
                command: Command::Reopen,
            },
            "reopen",
        )
        .await
        .unwrap();
    assert_eq!(r.status, Status::Draft);
    let r = svc
        .command(
            &ctx,
            CommandRequest {
                id: r.id,
                expected_rev: r.rev,
                key: "cancel".into(),
                command: Command::Cancel,
            },
            "cancel",
        )
        .await
        .unwrap();
    assert_eq!(r.status, Status::Cancelled);
    for _ in 1..20 {
        svc.create(&ctx, content(), "create").await.unwrap();
    }
    assert!(matches!(
        svc.create(&ctx, content(), "21st").await,
        Err(Error::RateLimited)
    ));
}
#[tokio::test]
async fn audit_failure_rolls_back_submission_and_runtime_rls_is_strict() {
    let Some((db, _fga, svc, ctx)) = setup().await else {
        return;
    };
    let r = svc.create(&ctx, content(), "create").await.unwrap();
    // runtime role へ切替えた接続で、文脈なし・他tenant INSERT を拒否する。
    let mut tx = db.begin().await.unwrap();
    sqlx::query("SET LOCAL ROLE shiki_runtime")
        .execute(&mut *tx)
        .await
        .unwrap();
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM bug_report WHERE tenant_id=$1 AND org=$2")
            .bind(ctx.tenant_id())
            .bind(ctx.org())
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(count, 0);
    storage::db_tenant::set_tenant(&mut tx, ctx.tenant())
        .await
        .unwrap();
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM bug_report WHERE tenant_id=$1 AND org=$2")
            .bind(ctx.tenant_id())
            .bind(ctx.org())
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(count, 1);
    let result=sqlx::query("INSERT INTO bug_report (tenant_id,org,id,reporter_id,status,rev,snapshot) VALUES ('wrong','org',$1,'alice','draft',1,'{}')").bind(Uuid::new_v4()).execute(&mut *tx).await;
    assert!(result.is_err());
    tx.rollback().await.unwrap();
    // この tenant だけ監査 insert を失敗させる。並行テストは影響しない。
    let function = format!("bug_report_fail_{}", Uuid::new_v4().simple());
    let ddl=format!("CREATE FUNCTION {function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.tenant_id = '{}' AND NEW.action='bug_report.submit' THEN RAISE EXCEPTION 'audit failure'; END IF; RETURN NEW; END $$",ctx.tenant_id());
    sqlx::query(&ddl).execute(&db).await.unwrap();
    sqlx::query(&format!("CREATE TRIGGER {function} BEFORE INSERT ON audit_log FOR EACH ROW EXECUTE FUNCTION {function}() ")).execute(&db).await.unwrap();
    let result = svc
        .command(&ctx, submit(r.id, 1, 1, true, "rollback"), "submit")
        .await;
    sqlx::query(&format!("DROP TRIGGER {function} ON audit_log"))
        .execute(&db)
        .await
        .unwrap();
    sqlx::query(&format!("DROP FUNCTION {function}()"))
        .execute(&db)
        .await
        .unwrap();
    assert!(matches!(result, Err(Error::Audit(_))));
    assert_eq!(svc.get(&ctx, r.id, "after").await.unwrap().rev, 1);
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM bug_report_submission WHERE tenant_id=$1 AND org=$2 AND report_id=$3",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(r.id)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(n, 0);
    assert_eq!(
        svc.command(&ctx, submit(r.id, 1, 1, true, "rollback"), "retry")
            .await
            .unwrap()
            .status,
        Status::Held
    );
}

#[tokio::test]
async fn daily_submission_limit_counts_all_revisions() {
    let Some((db, _fga, svc, ctx)) = setup().await else {
        return;
    };
    let mut r = svc.create(&ctx, content(), "draft").await.unwrap();
    for n in 0..20 {
        r = svc
            .command(
                &ctx,
                submit(r.id, r.rev, 1, true, &format!("submit{n}")),
                "submit",
            )
            .await
            .unwrap();
        r = svc
            .command(
                &ctx,
                CommandRequest {
                    id: r.id,
                    expected_rev: r.rev,
                    key: format!("reopen{n}"),
                    command: Command::Reopen,
                },
                "reopen",
            )
            .await
            .unwrap();
        // 時間窓から外すが24時間内に残し、日次上限だけを踏む。
        sqlx::query("UPDATE bug_report_submission SET consent_at=now()-interval '2 hours' WHERE tenant_id=$1 AND org=$2 AND report_id=$3").bind(ctx.tenant_id()).bind(ctx.org()).bind(r.id).execute(&db).await.unwrap();
    }
    assert!(matches!(
        svc.command(&ctx, submit(r.id, r.rev, 1, true, "twenty-first"), "limit")
            .await,
        Err(Error::RateLimited)
    ));
}

#[tokio::test]
async fn approval_rejection_policy_and_cancel_guards() {
    let Some((db, fga, svc, ctx)) = setup().await else {
        return;
    };
    let admin = context(ctx.tenant_id(), ctx.org(), "approver");
    fga.write_tuple(
        &admin.subject(),
        Relation::Member,
        &admin.ns().organization(admin.org()),
    )
    .await
    .unwrap();
    fga.write_tuple(
        &admin.subject(),
        Relation::Admin,
        &admin.ns().organization(admin.org()),
    )
    .await
    .unwrap();
    let mut policy = svc.policy(&ctx, "policy").await.unwrap();
    let original = policy.clone();
    policy = svc.set_policy(&ctx, policy, "update").await.unwrap();
    assert!(matches!(
        svc.set_policy(&ctx, original, "stale").await,
        Err(Error::Conflict)
    ));
    for approve in [false, true] {
        let r = svc.create(&ctx, content(), "draft").await.unwrap();
        let mut r = svc
            .command(
                &ctx,
                submit(r.id, 1, policy.rev, false, &r.id.to_string()),
                "submit",
            )
            .await
            .unwrap();
        // PR2 の宛先導入後に存在する pending snapshot を fixture で作る。
        r.status = Status::PendingApproval;
        sqlx::query("UPDATE bug_report SET status='pending_approval',snapshot=$4 WHERE tenant_id=$1 AND org=$2 AND id=$3").bind(ctx.tenant_id()).bind(ctx.org()).bind(r.id).bind(serde_json::to_value(&r).unwrap()).execute(&db).await.unwrap();
        let command = if approve {
            Command::Approve
        } else {
            Command::Reject {
                reason: "再現手順を追加してください".into(),
            }
        };
        let r = svc
            .command(
                &admin,
                CommandRequest {
                    id: r.id,
                    expected_rev: r.rev,
                    key: r.id.to_string(),
                    command,
                },
                "decision",
            )
            .await
            .unwrap();
        assert_eq!(
            r.status,
            if approve {
                Status::Held
            } else {
                Status::Rejected
            }
        );
        assert!(r.content.symptom.is_empty());
        assert!(matches!(
            svc.command(
                &admin,
                CommandRequest {
                    id: r.id,
                    expected_rev: r.rev - 1,
                    key: "stale".into(),
                    command: Command::Approve
                },
                "decision"
            )
            .await,
            Err(Error::Conflict)
        ));
    }
    let mut disabled = policy.clone();
    disabled.enabled = false;
    let disabled = svc.set_policy(&ctx, disabled, "disable").await.unwrap();
    assert!(matches!(
        svc.create(&ctx, content(), "disabled").await,
        Err(Error::Disabled)
    ));
    let mut enabled = disabled;
    enabled.enabled = true;
    let enabled = svc.set_policy(&ctx, enabled, "enable").await.unwrap();
    let r = svc.create(&ctx, content(), "draft").await.unwrap();
    assert!(matches!(
        svc.command(&ctx, submit(r.id, 1, enabled.rev, true, ""), "invalid")
            .await,
        Err(Error::Invalid)
    ));
    let r = svc
        .command(
            &ctx,
            CommandRequest {
                id: r.id,
                expected_rev: 1,
                key: "cancel".into(),
                command: Command::Cancel,
            },
            "cancel",
        )
        .await
        .unwrap();
    assert!(matches!(
        svc.get(&admin, r.id, "cancelled-draft").await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        svc.update(&ctx, r.id, r.rev, content(), "cancelled").await,
        Err(Error::Conflict)
    ));
    fga.delete_tuple(
        &admin.subject(),
        Relation::Admin,
        &admin.ns().organization(admin.org()),
    )
    .await
    .unwrap();
    assert!(matches!(
        svc.policy(&admin, "revoked").await,
        Err(Error::Forbidden)
    ));
}

#[tokio::test]
async fn list_detail_keep_drafts_private_and_recheck_admin() {
    let Some((_db, fga, service, ctx)) = setup().await else {
        return;
    };
    let bob = context(ctx.tenant_id(), ctx.org(), "bob");
    fga.write_tuple(
        &bob.subject(),
        Relation::Member,
        &bob.ns().organization(bob.org()),
    )
    .await
    .unwrap();
    let report = service.create(&ctx, content(), "trace").await.unwrap();
    assert_eq!(
        service
            .list(&ctx, false, None, "trace")
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    assert!(service
        .list(&bob, false, None, "trace")
        .await
        .unwrap()
        .items
        .is_empty());
    assert!(service
        .list(&ctx, true, None, "trace")
        .await
        .unwrap()
        .items
        .is_empty());
    assert!(matches!(
        service.detail(&bob, report.id, false, "trace").await,
        Err(Error::NotFound)
    ));
    let policy = service.reporter_policy(&ctx, "trace").await.unwrap();
    assert!(!policy.destination_available);
    let submitted = service
        .command(
            &ctx,
            submit(report.id, 1, policy.policy.rev, false, "submitted"),
            "trace",
        )
        .await
        .unwrap();
    assert_eq!(submitted.status, Status::Held);
    let admin = service.list(&ctx, true, None, "trace").await.unwrap();
    assert_eq!(admin.items[0].title, "不具合報告（カテゴリのみ）");
    assert!(service
        .list(&ctx, false, Some(report.id), "trace")
        .await
        .unwrap()
        .items
        .is_empty());
    let detail = service
        .detail(&ctx, report.id, true, "trace")
        .await
        .unwrap();
    assert!(!detail.submission.unwrap().content_consent);
    fga.delete_tuple(
        &ctx.subject(),
        Relation::Admin,
        &ctx.ns().organization(ctx.org()),
    )
    .await
    .unwrap();
    assert!(matches!(
        service.list(&ctx, true, None, "trace").await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        service.detail(&ctx, report.id, true, "trace").await,
        Err(Error::Forbidden)
    ));
    assert!(service
        .detail(&ctx, report.id, false, "trace")
        .await
        .is_ok());
}

#[tokio::test]
async fn content_sharing_prohibition_rejects_new_text_at_service_boundary() {
    let Some((_db, _fga, svc, ctx)) = setup().await else {
        return;
    };
    let report = svc
        .create(&ctx, content(), "before-policy-change")
        .await
        .unwrap();
    let mut policy = svc.policy(&ctx, "policy").await.unwrap();
    policy.allow_content_sharing = false;
    svc.set_policy(&ctx, policy, "policy").await.unwrap();
    assert!(matches!(
        svc.create(&ctx, content(), "forbidden").await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        svc.update(&ctx, report.id, report.rev, content(), "forbidden")
            .await,
        Err(Error::Forbidden)
    ));
    let metadata = ReportContent {
        category: Category::Bug,
        title: "不具合報告（カテゴリのみ）".into(),
        symptom: String::new(),
        steps_tried: String::new(),
        expected_behavior: String::new(),
    };
    assert!(svc.create(&ctx, metadata, "metadata-only").await.is_ok());
}

#[tokio::test]
async fn admin_cursor_survives_reopen_and_keeps_tenant_org_boundaries() {
    let Some((db, fga, svc, ctx)) = setup().await else {
        return;
    };
    // 同じ時刻の51件を用意し、idによるタイブレークも検証する。
    let template = svc.create(&ctx, content(), "template").await.unwrap();
    let template = svc
        .command(&ctx, submit(template.id, 1, 1, true, "submit"), "submit")
        .await
        .unwrap();
    sqlx::query("INSERT INTO bug_report (tenant_id,org,id,reporter_id,status,rev,snapshot,created_at) SELECT tenant_id,org,gen_random_uuid(),reporter_id,status,rev,snapshot,created_at FROM bug_report CROSS JOIN generate_series(1,50) WHERE tenant_id=$1 AND org=$2 AND id=$3")
        .bind(ctx.tenant_id()).bind(ctx.org()).bind(template.id).execute(&db).await.unwrap();
    sqlx::query("INSERT INTO bug_report_submission (tenant_id,org,report_id,submission_rev,payload,payload_hash,sink_profile_id,sink_config_version,policy_rev,consent_actor,content_consent,notice_version) SELECT r.tenant_id,r.org,r.id,s.submission_rev,s.payload,s.payload_hash,s.sink_profile_id,s.sink_config_version,s.policy_rev,s.consent_actor,s.content_consent,s.notice_version FROM bug_report r JOIN bug_report_submission s ON s.tenant_id=r.tenant_id AND s.org=r.org AND s.report_id=$3 WHERE r.tenant_id=$1 AND r.org=$2 AND r.id<>$3")
        .bind(ctx.tenant_id()).bind(ctx.org()).bind(template.id).execute(&db).await.unwrap();
    // snapshot 内のidも実際の行に一致させる。
    sqlx::query("UPDATE bug_report SET snapshot=jsonb_set(snapshot,'{id}',to_jsonb(id::text)) WHERE tenant_id=$1 AND org=$2")
        .bind(ctx.tenant_id()).bind(ctx.org()).execute(&db).await.unwrap();
    let first = svc.list(&ctx, true, None, "first").await.unwrap();
    assert_eq!(first.items.len(), 50);
    let cursor = first.next_cursor.unwrap();
    let before = svc.list(&ctx, true, Some(cursor), "before").await.unwrap();
    assert_eq!(before.items.len(), 1);
    svc.command(
        &ctx,
        CommandRequest {
            id: cursor,
            expected_rev: template.rev,
            key: "reopen-cursor".into(),
            command: Command::Reopen,
        },
        "reopen",
    )
    .await
    .unwrap();
    let after = svc.list(&ctx, true, Some(cursor), "after").await.unwrap();
    assert_eq!(after.items.len(), 1);
    assert_eq!(after.items[0].id, before.items[0].id);
    assert!(after.next_cursor.is_none());
    assert!(!svc
        .list(&ctx, true, None, "hidden")
        .await
        .unwrap()
        .items
        .iter()
        .any(|r| r.id == cursor));
    for other in [
        context(ctx.tenant_id(), "other-org", "alice"),
        context("other-tenant", ctx.org(), "alice"),
    ] {
        fga.write_tuple(
            &other.subject(),
            Relation::Member,
            &other.ns().organization(other.org()),
        )
        .await
        .unwrap();
        fga.write_tuple(
            &other.subject(),
            Relation::Admin,
            &other.ns().organization(other.org()),
        )
        .await
        .unwrap();
        sqlx::query("INSERT INTO bug_report (tenant_id,org,id,reporter_id,status,rev,snapshot,created_at) SELECT $4,$5,gen_random_uuid(),reporter_id,status,rev,snapshot,created_at-interval '1 day' FROM bug_report WHERE tenant_id=$1 AND org=$2 AND id=$3")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(before.items[0].id)
            .bind(other.tenant_id()).bind(other.org()).execute(&db).await.unwrap();
        sqlx::query("INSERT INTO bug_report_submission (tenant_id,org,report_id,submission_rev,payload,payload_hash,sink_profile_id,sink_config_version,policy_rev,consent_actor,content_consent,notice_version) SELECT r.tenant_id,r.org,r.id,s.submission_rev,s.payload,s.payload_hash,s.sink_profile_id,s.sink_config_version,s.policy_rev,s.consent_actor,s.content_consent,s.notice_version FROM bug_report r JOIN bug_report_submission s ON s.tenant_id=$1 AND s.org=$2 AND s.report_id=$3 WHERE r.tenant_id=$4 AND r.org=$5")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(template.id)
            .bind(other.tenant_id()).bind(other.org()).execute(&db).await.unwrap();
        assert_eq!(
            svc.list(&other, true, None, "visible")
                .await
                .unwrap()
                .items
                .len(),
            1
        );
        assert!(svc
            .list(&other, true, Some(cursor), "boundary")
            .await
            .unwrap()
            .items
            .is_empty());
    }
}

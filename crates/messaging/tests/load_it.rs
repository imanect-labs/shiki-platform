//! 同時接続 1,000 での表示遅延（Task 14.16・目安 3 秒）。
//!
//! 重いので `MESSAGING_LOAD=1` のときだけ走る（実 Postgres ＋ 実 OpenFGA）。購読者 1,000 人が同じ公開
//! チャンネルを購読し、1 件の投稿が全員に届くまでの時間を測る。購読側は SSE ハンドラと同じ手順
//! （許可を取る → 合図を受ける → 自分の権限で読み直す）を踏む。DB の接続数は本番の既定（10）にそろえる。
//! 接続数は `MESSAGING_LOAD_N` で変えられる。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::pedantic,
    clippy::print_stderr
)]

use authz::{
    AuthContext, AuthzClient, OpenFgaClient, OpenFgaConfig, Principal, PrincipalKind, Relation,
};
use chat::model::ContentBlock;
use messaging::pubsub::{Hub, Signal};
use messaging::{ChannelKind, CreateChannel, MessagingService, NewMessage};
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

fn context(tenant: &str, user: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: user.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "org".into(),
        authz::TenantId::parse(tenant).unwrap(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn thousand_subscribers_see_a_post_within_three_seconds() {
    if std::env::var("MESSAGING_LOAD").as_deref() != Ok("1") {
        return;
    }
    let n: usize = std::env::var("MESSAGING_LOAD_N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000);
    let url = shiki_testenv::var("STORAGE_TEST_DATABASE_URL").unwrap();
    let base_url = shiki_testenv::var("OPENFGA_TEST_URL").unwrap();
    let admin = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();
    sqlx::migrate!("../../migrations")
        .run(&admin)
        .await
        .unwrap();
    let fga = Arc::new(
        OpenFgaClient::connect(
            reqwest::Client::new(),
            &OpenFgaConfig {
                base_url,
                store_name: format!("messaging-load-{}", Uuid::new_v4()),
            },
            &serde_json::from_str(include_str!("../../authz/model/authorization-model.json"))
                .unwrap(),
        )
        .await
        .unwrap(),
    );
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("SET ROLE shiki_runtime").execute(conn).await?;
                Ok(())
            })
        })
        .connect(&url)
        .await
        .unwrap();
    let svc = Arc::new(MessagingService::new(
        pool,
        fga.clone(),
        Hub::local(),
        Arc::new(messaging::files::NoFiles),
    ));
    let tenant = format!("t{}", Uuid::new_v4().simple());
    let users: Vec<String> = (0..n).map(|i| format!("u{i:04}")).collect();
    // 組織メンバーのタプル（並列で書く）。
    let mut writes = tokio::task::JoinSet::new();
    for chunk in users
        .chunks(50)
        .map(<[String]>::to_vec)
        .chain(std::iter::once(vec!["poster".to_string()]))
    {
        let fga = fga.clone();
        let tenant = tenant.clone();
        writes.spawn(async move {
            for u in chunk {
                let ctx = context(&tenant, &u);
                fga.write_tuple(
                    &ctx.subject(),
                    Relation::Member,
                    &ctx.ns().organization(ctx.org()),
                )
                .await
                .unwrap();
            }
        });
    }
    while writes.join_next().await.is_some() {}
    let poster = context(&tenant, "poster");
    let ch = svc
        .create_channel(
            &poster,
            CreateChannel {
                name: "全社".into(),
                topic: String::new(),
                kind: ChannelKind::Public,
                announcement: false,
                members: vec![],
            },
            None,
        )
        .await
        .unwrap();

    // 購読者: 許可を取って購読し、合図を受けたら自分の権限で読み直す（SSE ハンドラと同じ手順）。
    let (ready_tx, mut ready_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let (seen_tx, mut seen_rx) = tokio::sync::mpsc::unbounded_channel::<Instant>();
    let since = chrono::Utc::now();
    for u in &users {
        let svc = svc.clone();
        let ctx = context(&tenant, u);
        let ready_tx = ready_tx.clone();
        let seen_tx = seen_tx.clone();
        let channel = ch.id;
        tokio::spawn(async move {
            let mut grant = svc.authorize_read(&ctx, channel).await.unwrap();
            let mut sub = svc.hub().subscribe(ctx.tenant_id(), channel);
            ready_tx.send(()).unwrap();
            while let Some(sig) = sub.recv().await {
                if sig != Signal::Changed {
                    continue;
                }
                let views = svc
                    .changes_with(
                        &ctx,
                        &mut grant,
                        since,
                        &mut std::collections::HashMap::new(),
                    )
                    .await
                    .unwrap();
                if !views.is_empty() {
                    seen_tx.send(Instant::now()).unwrap();
                    return;
                }
            }
        });
    }
    for _ in 0..n {
        ready_rx.recv().await.unwrap();
    }
    let started = Instant::now();
    svc.post(
        &poster,
        ch.id,
        NewMessage {
            parent_id: None,
            blocks: vec![ContentBlock::Text {
                text: "全員に届く発言".into(),
            }],
            agent_private: false,
        },
        None,
    )
    .await
    .unwrap();
    let mut latencies = Vec::with_capacity(n);
    for _ in 0..n {
        let t = tokio::time::timeout(Duration::from_secs(30), seen_rx.recv())
            .await
            .expect("30 秒以内に全員へ届くこと")
            .unwrap();
        latencies.push(t.duration_since(started));
    }
    latencies.sort();
    let p50 = latencies[n / 2];
    let p95 = latencies[n * 95 / 100];
    let max = *latencies.last().unwrap();
    eprintln!("messaging load: n={n} p50={p50:?} p95={p95:?} max={max:?}");
    assert!(
        max <= Duration::from_secs(3),
        "全員への表示が 3 秒を超えた: max={max:?}"
    );
}

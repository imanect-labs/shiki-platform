//! Redis でレプリカ間を中継する配信の結合テスト（実 Redis・PIT-66）。
//!
//! 別プロセスのハブ（別の `Hub::connect`）が出した合図は届き、同じテナント・チャンネルのトピック
//! だけが購読者に渡る。自分が出した合図は Redis から二重に戻らない。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use messaging::pubsub::{Hub, Signal};
use uuid::Uuid;

async fn next(sub: &mut messaging::pubsub::Subscription) -> Option<Signal> {
    tokio::time::timeout(Duration::from_secs(5), sub.recv())
        .await
        .ok()
        .flatten()
}

#[tokio::test]
async fn signals_cross_replicas_through_redis_and_stay_on_topic() {
    let Ok(url) = shiki_testenv::var("REDIS_TEST_URL") else {
        return;
    };
    let a = Hub::connect(&url).await;
    let b = Hub::connect(&url).await;
    let tenant = format!("t{}", Uuid::new_v4().simple());
    let channel = Uuid::new_v4();
    let mut on_b = b.subscribe(&tenant, channel);
    let mut on_a = a.subscribe(&tenant, channel);
    // psubscribe の開始を待つ（届くまで送り直す）。
    let mut got = None;
    for _ in 0..25 {
        a.publish(&tenant, channel, Signal::Changed).await;
        if let Ok(Some(s)) = tokio::time::timeout(Duration::from_millis(200), on_b.recv()).await {
            got = Some(s);
            break;
        }
    }
    assert!(matches!(got, Some(Signal::Changed)), "{got:?}");
    // 別テナントの同じ channel_id は届かない。
    let other = format!("t{}", Uuid::new_v4().simple());
    a.publish(&other, channel, Signal::Changed).await;
    a.publish(
        &tenant,
        channel,
        Signal::Leave {
            user_id: "bob".into(),
        },
    )
    .await;
    match next(&mut on_b).await {
        Some(Signal::Leave { user_id }) => assert_eq!(user_id, "bob"),
        other => panic!("同じトピックの合図だけが届く: {other:?}"),
    }
    // 送り手のハブには、プロセス内の合図だけが届く（Redis から二重に戻らない）。
    let mut seen = 0;
    while let Ok(Some(_)) = tokio::time::timeout(Duration::from_millis(500), on_a.recv()).await {
        seen += 1;
    }
    assert!(seen >= 2, "送り手もプロセス内で受け取る: {seen}");

    // URL が不正でも、プロセス内の配信は動く。
    let bad = Hub::connect("not a url").await;
    let mut sub = bad.subscribe(&tenant, channel);
    bad.publish(&tenant, channel, Signal::Changed).await;
    assert!(matches!(next(&mut sub).await, Some(Signal::Changed)));
}

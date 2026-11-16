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
    // psubscribe の開始を、検証とは別のチャンネルで待つ（再送の残りが検証の購読に混ざらない）。
    let warm = Uuid::new_v4();
    let mut warm_b = b.subscribe(&tenant, warm);
    let mut ready = false;
    for _ in 0..25 {
        a.publish(&tenant, warm, Signal::Changed).await;
        if let Ok(Some(_)) = tokio::time::timeout(Duration::from_millis(200), warm_b.recv()).await {
            ready = true;
            break;
        }
    }
    assert!(ready, "Redis の中継が 5 秒以内に始まる");

    let channel = Uuid::new_v4();
    let mut on_b = b.subscribe(&tenant, channel);
    let mut on_a = a.subscribe(&tenant, channel);
    // 別テナントの同じ channel_id は届かない。
    let other = format!("t{}", Uuid::new_v4().simple());
    a.publish(&other, channel, Signal::Changed).await;
    // 検証するトピックへは 2 回だけ送る。
    a.publish(&tenant, channel, Signal::Changed).await;
    a.publish(
        &tenant,
        channel,
        Signal::Leave {
            user_id: "bob".into(),
        },
    )
    .await;
    assert!(matches!(next(&mut on_b).await, Some(Signal::Changed)));
    match next(&mut on_b).await {
        Some(Signal::Leave { user_id }) => assert_eq!(user_id, "bob"),
        other => panic!("同じトピックの合図だけが順に届く: {other:?}"),
    }
    // 受け手にはちょうど 2 件（別テナントの合図・重複は無い）。
    let extra = tokio::time::timeout(Duration::from_millis(500), on_b.recv()).await;
    assert!(extra.is_err(), "余分な合図が届いた: {extra:?}");
    // 送り手のハブには、プロセス内の 2 件だけが届く（Redis から二重に戻らない）。
    let mut seen = 0;
    while let Ok(Some(_)) = tokio::time::timeout(Duration::from_millis(500), on_a.recv()).await {
        seen += 1;
    }
    assert_eq!(seen, 2, "送り手は自分の合図を 1 回ずつだけ受け取る");

    // URL が不正でも、プロセス内の配信は動く。
    let bad = Hub::connect("not a url").await;
    let mut sub = bad.subscribe(&tenant, channel);
    bad.publish(&tenant, channel, Signal::Changed).await;
    assert!(matches!(next(&mut sub).await, Some(Signal::Changed)));
}

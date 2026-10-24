//! Tantivy の常駐資源の上限（#816）の結合テスト。
//!
//! 外部サービス不要（tempdir）のため常時実行される。
//! 検証: writer / 索引の同時オープン数が上限を超えない・閉じて開き直しても commit 済みの内容が
//! 残る・アイドル解放・並行書込とクローズの競合でディレクトリロックが衝突しない・purge で
//! writer が閉じる・閉じ直した後もテナント境界が保たれる。

// テストコード: pedantic/安全系 lint は本番コードのみ厳格化する方針のため許容する。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::pedantic
)]

use std::time::Duration;

use authz::{AuthContext, Principal};
use rag::vector_store::PreFilter;
use rag::{FulltextDoc, FulltextIndex, TantivyFulltext, TantivyLimits};
use uuid::Uuid;

fn ctx(tenant: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        tenant.into(),
    )
}

fn put(ft: &TantivyFulltext, ctx: &AuthContext, node: Uuid, text: &str) {
    let tags = vec![ctx.ns().file(&node.to_string()).as_str().to_string()];
    ft.replace_node(
        ctx,
        node,
        &[FulltextDoc {
            chunk_id: Uuid::new_v5(&node, b"chunk-0"),
            node_id: node,
            version: 1,
            text,
            authz_tags: &tags,
        }],
    )
    .unwrap();
}

fn hits(ft: &TantivyFulltext, ctx: &AuthContext, q: &str) -> Vec<Uuid> {
    ft.search(ctx, q, 10, &PreFilter::TenantOnly, &[], &[])
        .unwrap()
        .into_iter()
        .map(|h| h.node_id)
        .collect()
}

fn limits(indexes: usize, writers: usize, idle: Duration) -> TantivyLimits {
    TantivyLimits {
        max_open_indexes: indexes,
        max_open_writers: writers,
        idle_timeout: idle,
    }
}

#[test]
fn open_writers_and_indexes_stay_within_limits_and_data_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let ft = TantivyFulltext::with_limits(dir.path(), limits(2, 1, Duration::from_hours(1)));
    let tenants = ["t-a", "t-b", "t-c"];
    let mut nodes = Vec::new();
    for t in tenants {
        let node = Uuid::new_v4();
        put(&ft, &ctx(t), node, &format!("{t} の四半期売上報告"));
        nodes.push(node);
        let (indexes, writers) = ft.open_counts();
        assert!(writers <= 1, "writer は上限 1 本まで（{t}: {writers}）");
        assert!(indexes <= 2, "索引は上限 2 個まで（{t}: {indexes}）");
    }
    // 閉じられた（追い出された）テナントも、開き直して commit 済みの内容が引ける。
    // 開き直した索引でもテナント境界は index 選択で保たれる（他テナントの文書は出ない）。
    for (t, node) in tenants.iter().zip(&nodes) {
        assert_eq!(
            hits(&ft, &ctx(t), "売上"),
            vec![*node],
            "{t} は自分の 1 件だけ"
        );
    }
    assert!(ft.open_counts().0 <= 2, "検索で開き直しても上限内");
}

#[test]
fn close_idle_releases_writers_and_indexes_then_reopens_on_demand() {
    let dir = tempfile::tempdir().unwrap();
    let ft = TantivyFulltext::with_limits(dir.path(), limits(8, 4, Duration::ZERO));
    let c = ctx("t-idle");
    let first = Uuid::new_v4();
    put(&ft, &c, first, "東京の売上");
    assert_eq!(ft.open_counts(), (1, 1));

    assert_eq!(ft.close_idle(), (1, 1), "アイドルの writer と索引を閉じる");
    assert_eq!(ft.open_counts(), (0, 0));
    assert_eq!(ft.close_idle(), (0, 0), "閉じる物が無ければ何もしない");

    // 検索だけなら writer は開かない。
    assert_eq!(hits(&ft, &c, "東京"), vec![first]);
    assert_eq!(ft.open_counts(), (1, 0), "検索は reader だけを開く");
    // 書込で writer を開き直し、前の commit と合わせて引ける。
    let second = Uuid::new_v4();
    put(&ft, &c, second, "大阪の売上");
    let mut got = hits(&ft, &c, "売上");
    got.sort();
    let mut want = vec![first, second];
    want.sort();
    assert_eq!(got, want);
}

#[test]
fn writes_stay_resident_while_not_idle() {
    let dir = tempfile::tempdir().unwrap();
    let ft = TantivyFulltext::with_limits(dir.path(), limits(8, 4, Duration::from_hours(1)));
    put(&ft, &ctx("t-busy"), Uuid::new_v4(), "名古屋");
    assert_eq!(
        ft.close_idle(),
        (0, 0),
        "アイドル時間に満たないものは閉じない"
    );
    assert_eq!(ft.open_counts(), (1, 1));
}

/// 並行書込と、上限による入れ替え・アイドル解放が同時に走っても、同じテナントの writer が
/// 二重に開かれて Tantivy のディレクトリロックで失敗することがない。全書込が検索に残る。
#[test]
fn concurrent_writes_and_closing_never_double_open_a_writer() {
    let dir = tempfile::tempdir().unwrap();
    let ft = TantivyFulltext::with_limits(dir.path(), limits(2, 1, Duration::ZERO));
    let tenants = ["c-a", "c-b", "c-c"];
    let written: Vec<(String, Uuid)> = std::thread::scope(|s| {
        let closer = s.spawn(|| {
            for _ in 0..20 {
                ft.close_idle();
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let writers: Vec<_> = tenants
            .iter()
            .map(|t| {
                let ft = &ft;
                s.spawn(move || {
                    let c = ctx(t);
                    let mut mine = Vec::new();
                    for i in 0..3 {
                        let node = Uuid::new_v4();
                        put(ft, &c, node, &format!("並行 書込 {i}"));
                        mine.push((t.to_string(), node));
                    }
                    mine
                })
            })
            .collect();
        let out = writers
            .into_iter()
            .flat_map(|h| h.join().expect("書込スレッドが失敗しない"))
            .collect();
        closer.join().unwrap();
        out
    });
    for t in tenants {
        let mut want: Vec<Uuid> = written
            .iter()
            .filter(|(tt, _)| tt == t)
            .map(|(_, n)| *n)
            .collect();
        want.sort();
        let mut got = hits(&ft, &ctx(t), "並行");
        got.sort();
        assert_eq!(
            got, want,
            "{t} の全書込が残り、他テナントの文書は混ざらない"
        );
    }
}

#[test]
fn purge_closes_the_writer_and_releases_the_slot() {
    let dir = tempfile::tempdir().unwrap();
    let ft = TantivyFulltext::with_limits(dir.path(), limits(4, 1, Duration::from_hours(1)));
    put(&ft, &ctx("p-a"), Uuid::new_v4(), "撤去対象");
    assert_eq!(ft.open_counts(), (1, 1));
    ft.purge_tenant("p-a").unwrap();
    assert_eq!(ft.open_counts(), (0, 0), "purge で writer と索引を手放す");
    assert!(!ft.tenant_index_exists("p-a"));
    // 空いた枠で別テナントが書ける（上限 1 本を purge 済みテナントが占有し続けない）。
    let node = Uuid::new_v4();
    put(&ft, &ctx("p-b"), node, "後続");
    assert_eq!(hits(&ft, &ctx("p-b"), "後続"), vec![node]);
    assert!(hits(&ft, &ctx("p-a"), "撤去対象").is_empty());
}

#[test]
fn zero_limits_are_raised_to_a_working_minimum() {
    let dir = tempfile::tempdir().unwrap();
    let ft = TantivyFulltext::with_limits(dir.path(), limits(0, 0, Duration::from_hours(1)));
    let node = Uuid::new_v4();
    put(&ft, &ctx("z-a"), node, "最小構成");
    assert_eq!(hits(&ft, &ctx("z-a"), "最小"), vec![node]);
    assert_eq!(ft.open_counts(), (1, 1));
}

/// 周期タスクがアイドルの writer・索引を閉じ、本体が捨てられたら自分も終わる。
#[tokio::test]
async fn idle_reaper_closes_idle_resources_and_stops_when_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let ft = std::sync::Arc::new(TantivyFulltext::with_limits(
        dir.path(),
        limits(4, 2, Duration::ZERO),
    ));
    put(&ft, &ctx("r-a"), Uuid::new_v4(), "周期解放");
    assert_eq!(ft.open_counts(), (1, 1));
    let handle = ft.spawn_idle_reaper();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while ft.open_counts() != (0, 0) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "周期タスクが閉じない"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    drop(ft);
    tokio::time::timeout(Duration::from_secs(10), handle)
        .await
        .expect("本体を捨てたら周期タスクは終わる")
        .unwrap();
}

//! `channel` 型（職員間メッセージ・Task 14.2・design §4.14）の正例・負例の結合テスト。
//!
//! 実 OpenFGA が必要。`OPENFGA_TEST_URL` 未設定ならスキップする（`SHIKI_REQUIRE_IT=1` なら失敗）。
//! relation schema は human 承認済みの形（公開＝参加は明示タプル＋org_reader／投稿は poster ∩ member／
//! admin は org 管理者から継承しない）をそのまま検証する。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::pedantic
)]

use authz::{
    client::{OpenFgaClient, OpenFgaConfig},
    vocab::Relation,
    AuthContext, AuthzClient, Consistency, FgaObject, Principal, Subject,
};

fn model_json() -> serde_json::Value {
    serde_json::from_str(include_str!("../model/authorization-model.json")).unwrap()
}

fn ctx(tenant: &str, user: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: user.to_string(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.to_string()),
        },
        "acme".to_string(),
        authz::TenantId::parse(tenant.to_string()).unwrap(),
    )
}

async fn connect() -> Option<OpenFgaClient> {
    let Ok(base_url) = shiki_testenv::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return None;
    };
    let config = OpenFgaConfig {
        base_url,
        store_name: format!("shiki-channel-{}", uuid::Uuid::new_v4()),
    };
    Some(
        OpenFgaClient::connect(reqwest::Client::new(), &config, &model_json())
            .await
            .expect("OpenFGA へ接続できること"),
    )
}

async fn can(c: &OpenFgaClient, s: &Subject, r: Relation, o: &FgaObject) -> bool {
    c.check(s, r, o, Consistency::HigherConsistency)
        .await
        .unwrap()
}

async fn write(c: &OpenFgaClient, s: &Subject, r: Relation, o: &FgaObject) {
    c.write_tuple(s, r, o).await.unwrap();
}

/// 組織 acme に alice（作成者）・bob（一般）・carol（org 管理者）を置き、
/// チャンネル作成時のタプル（parent・admin・poster@channel#member）を書く。
async fn seed(c: &OpenFgaClient, t: &AuthContext, channel: &str) -> FgaObject {
    let org = t.ns().organization("acme");
    for u in ["alice", "bob", "carol"] {
        write(c, &t.ns().user(u), Relation::Member, &org).await;
    }
    write(c, &t.ns().user("carol"), Relation::Admin, &org).await;
    let ch = t.ns().channel(channel);
    write(c, &Subject::object(&org), Relation::Parent, &ch).await;
    write(c, &t.ns().user("alice"), Relation::Admin, &ch).await;
    write(c, &t.ns().channel_member(channel), Relation::Poster, &ch).await;
    ch
}

#[tokio::test]
async fn private_channel_is_closed_to_non_members() {
    let Some(c) = connect().await else { return };
    let t = ctx("t1", "alice");
    let ch = seed(&c, &t, "priv").await;
    let (alice, bob, carol) = (
        t.ns().user("alice"),
        t.ns().user("bob"),
        t.ns().user("carol"),
    );

    // 作成者（admin ⊂ member）は読めて投稿できる。
    assert!(can(&c, &alice, Relation::CanRead, &ch).await);
    assert!(can(&c, &alice, Relation::CanPost, &ch).await);
    assert!(can(&c, &alice, Relation::CanManage, &ch).await);

    // 負例: 組織メンバーでも非メンバーは読めず・参加できず・投稿できない。
    for r in [Relation::CanRead, Relation::CanJoin, Relation::CanPost] {
        assert!(
            !can(&c, &bob, r, &ch).await,
            "bob は非公開チャンネルの {r} を持たない"
        );
    }
    // 負例: org 管理者でもチャンネルの中身・管理権を継承しない（PIT-65）。
    for r in [Relation::CanRead, Relation::CanPost, Relation::CanManage] {
        assert!(
            !can(&c, &carol, r, &ch).await,
            "org 管理者は {r} を継承しない"
        );
    }

    // 招待（member タプル）で読めて投稿できる。
    write(&c, &bob, Relation::Member, &ch).await;
    assert!(can(&c, &bob, Relation::CanRead, &ch).await);
    assert!(can(&c, &bob, Relation::CanPost, &ch).await);
    // 招待されても管理権は持たない。
    assert!(!can(&c, &bob, Relation::CanManage, &ch).await);
}

#[tokio::test]
async fn public_channel_is_readable_by_org_but_posting_requires_membership() {
    let Some(c) = connect().await else { return };
    let t = ctx("t1", "alice");
    let ch = seed(&c, &t, "pub").await;
    let org_members = t.ns().organization_member("acme");
    write(&c, &org_members, Relation::OrgReader, &ch).await;
    let bob = t.ns().user("bob");

    // 参加していない組織メンバーは読めて参加できるが、投稿はできない。
    assert!(can(&c, &bob, Relation::CanRead, &ch).await);
    assert!(can(&c, &bob, Relation::CanJoin, &ch).await);
    assert!(!can(&c, &bob, Relation::CanPost, &ch).await);
    assert!(!can(&c, &bob, Relation::CanManage, &ch).await);

    // 参加すると投稿できる。
    write(&c, &bob, Relation::Member, &ch).await;
    assert!(can(&c, &bob, Relation::CanPost, &ch).await);

    // 退出（member タプル削除）で投稿権を失う。公開なので読むことはできる。
    c.delete_tuple(&bob, Relation::Member, &ch).await.unwrap();
    assert!(!can(&c, &bob, Relation::CanPost, &ch).await);
    assert!(can(&c, &bob, Relation::CanRead, &ch).await);

    // 負例: 組織に属さない利用者は公開チャンネルも読めない。
    let outsider = t.ns().user("mallory");
    assert!(!can(&c, &outsider, Relation::CanRead, &ch).await);
    assert!(!can(&c, &outsider, Relation::CanJoin, &ch).await);
}

#[tokio::test]
async fn announcement_channel_limits_posting_to_named_posters() {
    let Some(c) = connect().await else { return };
    let t = ctx("t1", "alice");
    let org = t.ns().organization("acme");
    for u in ["alice", "bob", "dave"] {
        write(&c, &t.ns().user(u), Relation::Member, &org).await;
    }
    // アナウンス専用: poster@channel#member を書かず、個別の user だけを poster にする。
    let ch = t.ns().channel("news");
    write(&c, &Subject::object(&org), Relation::Parent, &ch).await;
    write(&c, &t.ns().user("alice"), Relation::Admin, &ch).await;
    write(&c, &t.ns().user("dave"), Relation::Poster, &ch).await;
    write(&c, &t.ns().user("bob"), Relation::Member, &ch).await;

    assert!(can(&c, &t.ns().user("bob"), Relation::CanRead, &ch).await);
    assert!(!can(&c, &t.ns().user("bob"), Relation::CanPost, &ch).await);
    // poster でも参加していなければ投稿できない（poster ∩ member）。
    assert!(!can(&c, &t.ns().user("dave"), Relation::CanPost, &ch).await);
    write(&c, &t.ns().user("dave"), Relation::Member, &ch).await;
    assert!(can(&c, &t.ns().user("dave"), Relation::CanPost, &ch).await);
    // admin でも poster でなければ投稿できない。
    assert!(!can(&c, &t.ns().user("alice"), Relation::CanPost, &ch).await);
}

#[tokio::test]
async fn public_channel_does_not_cross_tenants() {
    let Some(c) = connect().await else { return };
    let t1 = ctx("t1", "alice");
    let ch = seed(&c, &t1, "pub").await;
    write(
        &c,
        &t1.ns().organization_member("acme"),
        Relation::OrgReader,
        &ch,
    )
    .await;

    // 別テナントで同じ org 名・同じ user id を持つ利用者は、t1 の公開チャンネルを読めない。
    let t2 = ctx("t2", "bob");
    write(
        &c,
        &t2.ns().user("bob"),
        Relation::Member,
        &t2.ns().organization("acme"),
    )
    .await;
    assert!(!can(&c, &t2.ns().user("bob"), Relation::CanRead, &ch).await);
    assert!(!can(&c, &t2.ns().user("bob"), Relation::CanJoin, &ch).await);
    // 同じ local id のチャンネルでも t2 側のオブジェクトは別物。
    assert!(
        !can(
            &c,
            &t1.ns().user("alice"),
            Relation::CanRead,
            &t2.ns().channel("pub")
        )
        .await
    );
}

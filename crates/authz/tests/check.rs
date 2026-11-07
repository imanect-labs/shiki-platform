//! OpenFGA check の正例・負例の結合テスト。
//!
//! 実 OpenFGA が必要。`OPENFGA_TEST_URL`（例: `http://localhost:8080`）が
//! 設定されている時のみ実行し、未設定なら early-return でスキップする
//! （ローカルの素の `cargo test` を壊さないため）。CI の compose smoke で実走する。

// テストコード: pedantic/安全系 lint は本番コードのみ厳格化する方針のため許容する。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::cognitive_complexity
)]

use authz::{
    client::{OpenFgaClient, OpenFgaConfig},
    vocab::Relation,
    AuthContext, AuthzClient, Consistency, Principal,
};

fn model_json() -> serde_json::Value {
    let raw = include_str!("../model/authorization-model.json");
    serde_json::from_str(raw).expect("model JSON が不正")
}

/// テナント名前空間化（SAAS.1）された識別子を組むための最小 `AuthContext`。
/// アプリ同様 `ctx.ns()` 経由でしか FGA 識別子を構築できないため、テストも同経路を使う。
fn ctx_for(tenant_id: &str, user_id: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: user_id.to_string(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant_id.to_string()),
        },
        "acme".to_string(),
        authz::TenantId::parse(tenant_id.to_string()).unwrap(),
    )
}

#[tokio::test]
async fn check_allows_member_and_denies_other_org() {
    let Ok(base_url) = shiki_testenv::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return;
    };

    // store 名はテスト毎にユニークにして他テストと干渉させない。
    let store_name = format!("shiki-test-{}", uuid::Uuid::new_v4());
    let http = reqwest::Client::new();
    let config = OpenFgaConfig {
        base_url,
        store_name,
    };

    let client = OpenFgaClient::connect(http, &config, &model_json())
        .await
        .expect("OpenFGA へ接続できること");

    // tenant `t1` の名前空間で識別子を組む（`user:t1|alice` / `organization:t1|acme`）。
    let ctx = ctx_for("t1", "alice");
    let alice = ctx.subject();
    let acme = ctx.ns().organization("acme");
    let other = ctx.ns().organization("other");

    // alice を acme の member として付与（新規付与なので changed=true）。
    assert!(client
        .write_tuple(&alice, Relation::Member, &acme)
        .await
        .expect("tuple 書き込み成功"));

    // 正例: alice は acme の member（強整合で書込直後を確実に観測）。
    assert!(client
        .check(
            &alice,
            Relation::Member,
            &acme,
            Consistency::HigherConsistency
        )
        .await
        .unwrap());

    // 負例: alice は other org の member ではない。
    assert!(!client
        .check(
            &alice,
            Relation::Member,
            &other,
            Consistency::HigherConsistency
        )
        .await
        .unwrap());
}

#[tokio::test]
async fn write_and_delete_tuple_are_idempotent() {
    let Ok(base_url) = shiki_testenv::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return;
    };
    let store_name = format!("shiki-test-{}", uuid::Uuid::new_v4());
    let http = reqwest::Client::new();
    let config = OpenFgaConfig {
        base_url,
        store_name,
    };
    let client = OpenFgaClient::connect(http, &config, &model_json())
        .await
        .expect("OpenFGA へ接続できること");

    let ctx = ctx_for("t1", "bob");
    let bob = ctx.subject();
    let acme = ctx.ns().organization("acme");

    // 冪等な write: 1 回目は実書込（true）、2 回目は既存 no-op（false）。
    assert!(client
        .write_tuple(&bob, Relation::Member, &acme)
        .await
        .expect("1 回目の write 成功"));
    assert!(!client
        .write_tuple(&bob, Relation::Member, &acme)
        .await
        .expect("既存 tuple の再 write も成功扱い（冪等）"));
    assert!(client
        .check(
            &bob,
            Relation::Member,
            &acme,
            Consistency::HigherConsistency
        )
        .await
        .unwrap());

    // 冪等な delete: 1 回目は実削除（true）、2 回目は不在 no-op（false）。
    assert!(client
        .delete_tuple(&bob, Relation::Member, &acme)
        .await
        .expect("1 回目の delete 成功"));
    assert!(!client
        .delete_tuple(&bob, Relation::Member, &acme)
        .await
        .expect("不在 tuple の再 delete も成功扱い（冪等）"));
    assert!(!client
        .check(
            &bob,
            Relation::Member,
            &acme,
            Consistency::HigherConsistency
        )
        .await
        .unwrap());
}

/// BatchCheck（Task 15.9）: 入力順で判定が返り、50 問超は分割して全問を判定する。単発 check と一致する。
/// 分けた BatchCheck は 4 本ずつ並べて送る（BUG-163）ので、並べる組をまたいでも入力順のまま返ること。
#[tokio::test]
async fn batch_check_matches_single_checks_and_chunks() {
    let Ok(base_url) = shiki_testenv::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return;
    };
    let config = OpenFgaConfig {
        base_url: base_url.clone(),
        store_name: format!("shiki-test-{}", uuid::Uuid::new_v4()),
    };
    let client = OpenFgaClient::connect(reqwest::Client::new(), &config, &model_json())
        .await
        .expect("OpenFGA へ接続できること");
    let ctx = ctx_for("t1", "carol");
    let carol = ctx.subject();
    // 3 つおきの組織にだけ member を付与する（470 問 = 4 本並べる組 200 問 × 2 ＋ 70 問・50 問ずつ 10 往復）。
    const N: usize = 470;
    let orgs: Vec<_> = (0..N)
        .map(|i| ctx.ns().organization(&format!("org{i}")))
        .collect();
    for (i, o) in orgs.iter().enumerate() {
        if i % 3 == 0 {
            client
                .write_tuple(&carol, Relation::Member, o)
                .await
                .unwrap();
        }
    }
    let items: Vec<authz::CheckTuple> = orgs
        .iter()
        .map(|o| authz::CheckTuple {
            subject: carol.clone(),
            relation: Relation::Member,
            object: o.clone(),
        })
        .collect();
    let got = client
        .batch_check(&items, Consistency::HigherConsistency)
        .await
        .expect("batch_check");
    assert_eq!(got.len(), N);
    for (i, allowed) in got.iter().enumerate() {
        assert_eq!(*allowed, i % 3 == 0, "問い {i} の判定が入力順で返ること");
    }
    // 単発 check と一致する（抜き取り・分けた境目と並べる組の境目）。
    for i in [0usize, 1, 49, 50, 51, 199, 200, 201, 399, 400, N - 1] {
        let single = client
            .check(
                &carol,
                Relation::Member,
                &orgs[i],
                Consistency::HigherConsistency,
            )
            .await
            .unwrap();
        assert_eq!(single, got[i], "問い {i}");
    }
    // 空の入力は往復せず空を返す。
    assert!(client
        .batch_check(&[], Consistency::HigherConsistency)
        .await
        .unwrap()
        .is_empty());
    // このテストのストアを消す（書いたタプルも消える）。
    let resp = reqwest::Client::new()
        .delete(format!("{base_url}/stores/{}", client.store_id()))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "delete store: {}",
        resp.status()
    );
}

/// テーブルの「追加だけ」（data_table#submitter・D-113）: editor（owner）が含意するが、viewer とは
/// 互いに含意しない。部署（role#member）への付与も効き、ワークフローの主体へは付けられない（型で拒否）。
#[tokio::test]
async fn data_table_submitter_is_add_only_and_not_a_viewer() {
    let Ok(base_url) = std::env::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return;
    };
    let config = OpenFgaConfig {
        base_url,
        store_name: format!("shiki-test-{}", uuid::Uuid::new_v4()),
    };
    let client = OpenFgaClient::connect(reqwest::Client::new(), &config, &model_json())
        .await
        .expect("OpenFGA へ接続できること");
    let ns_ctx = ctx_for("t1", "alice");
    let ns = ns_ctx.ns();
    let table = ns.data_table("tbl");
    let staff = ns.role("staff");
    let who = |id: &str| ctx_for("t1", id).subject();
    let (alice, bob, carol, dave, erin) = (
        who("alice"),
        who("bob"),
        who("carol"),
        who("dave"),
        who("erin"),
    );
    let staff_members = authz::Subject::userset(&staff, Relation::Member);
    let writes = [
        (alice.clone(), Relation::Owner, table.clone()),
        (bob.clone(), Relation::Submitter, table.clone()),
        (carol.clone(), Relation::Viewer, table.clone()),
        (dave.clone(), Relation::Editor, table.clone()),
        (staff_members.clone(), Relation::Submitter, table.clone()),
        (erin.clone(), Relation::Member, staff.clone()),
    ];
    for (s, r, o) in &writes {
        client.write_tuple(s, *r, o).await.expect("write");
    }
    let check = |s: authz::Subject, r: Relation| {
        let client = &client;
        let table = table.clone();
        async move {
            client
                .check(&s, r, &table, Consistency::HigherConsistency)
                .await
                .unwrap()
        }
    };
    // 追加だけ（bob）は追加できるが、閲覧も編集もできない。
    assert!(check(bob.clone(), Relation::Submitter).await);
    assert!(
        !check(bob.clone(), Relation::Viewer).await,
        "追加だけは閲覧を含まない"
    );
    assert!(!check(bob.clone(), Relation::Editor).await);
    // 閲覧だけ（carol）は追加できない（viewer は submitter を含まない）。
    assert!(check(carol.clone(), Relation::Viewer).await);
    assert!(
        !check(carol.clone(), Relation::Submitter).await,
        "閲覧は追加を含まない"
    );
    // 編集（dave）・持ち主（alice）は追加もできる。
    assert!(check(dave.clone(), Relation::Submitter).await);
    assert!(check(alice.clone(), Relation::Submitter).await);
    // 部署への追加だけの共有は、部署のメンバー（erin）に効き、閲覧は含まない。
    assert!(check(erin.clone(), Relation::Submitter).await);
    assert!(!check(erin.clone(), Relation::Viewer).await);
    // ワークフローの主体へは付けられない（委譲は viewer / editor だけ・型の制約）。
    let wf = ns.workflow_principal("wf1");
    assert!(
        client
            .write_tuple(&wf, Relation::Submitter, &table)
            .await
            .is_err(),
        "workflow を submitter に付けられないこと"
    );
    // 片付け（このテストで書いたタプルを消す）。
    for (s, r, o) in &writes {
        client.delete_tuple(s, *r, o).await.expect("delete");
    }
    assert!(!check(bob, Relation::Submitter).await);
}

//! 利用者の id（OIDC `sub`）に FGA の区切りの文字が入ったとき、実の OpenFGA がどう扱うかの結合テスト
//! （D-205・GAP-77）。
//!
//! `OPENFGA_TEST_URL` 設定時のみ実行（未設定は skip・`SHIKI_REQUIRE_IT=1` なら失敗）。
//! [`authz::validate_user_id`] がログイン・セッションの更新・アプリのゲートウェイの入口で断る文字の根拠を、
//! 記憶ではなく実の挙動で残す:
//!
//! - `:`・`#`・ASCII の空白を含む user は、書き込みも check も 400 で断られる（member にならない）。
//! - `|` を含む user は書けて check も通り、読み出しにも出る。定期の照合はこの人を飛ばすので、入口で断らないと
//!   所属グループから外れても member が外れない（D-136 の窓）。
//! - 全角の空白（U+3000）・制御文字は OpenFGA が受け付ける（入口で断るのは shiki の決まり）。
//! - object 側（ロールの id）の ASCII の空白も 400 で断られる（空白を含むグループ名の部署は付かない）。

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
    AuthzClient, AuthzError, Consistency, Namespace, ObjectType, Relation,
};

const TENANT: &str = "t";

fn model_json() -> serde_json::Value {
    let raw = include_str!("../model/authorization-model.json");
    serde_json::from_str(raw).expect("model JSON が不正")
}

/// 試しごとに別のストアへつなぐ（ほかの試しと干渉させない）。未設定なら `None`。
async fn connect() -> Option<OpenFgaClient> {
    let Ok(base_url) = shiki_testenv::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return None;
    };
    let config = OpenFgaConfig {
        base_url,
        store_name: format!("shiki-test-user-id-{}", uuid::Uuid::new_v4()),
    };
    Some(
        OpenFgaClient::connect(reqwest::Client::new(), &config, &model_json())
            .await
            .expect("OpenFGA へ接続できること"),
    )
}

/// OpenFGA が 400（検証の誤り）で断ったか。
fn is_rejected(result: &Result<impl std::fmt::Debug, AuthzError>) -> bool {
    matches!(result, Err(AuthzError::Unexpected { status: 400, .. }))
}

#[tokio::test]
async fn separators_and_ascii_whitespace_in_the_user_are_rejected_by_openfga() {
    let Some(client) = connect().await else {
        return;
    };
    let ns = Namespace::for_tenant(TENANT);
    let org = ns.organization("acme");
    // フェデレーションで Import を切った人の sub（`f:<provider>:<username>`）・userset の区切り・空白・タブ。
    for id in ["f:ldap:bob", "a#b", "a b", "a\tb"] {
        let user = ns.user(id);
        let written = client.write_tuple(&user, Relation::Member, &org).await;
        assert!(
            is_rejected(&written),
            "{id:?} の書き込みは 400: {written:?}"
        );
        let checked = client
            .check(
                &user,
                Relation::Member,
                &org,
                Consistency::HigherConsistency,
            )
            .await;
        assert!(is_rejected(&checked), "{id:?} の check は 400: {checked:?}");
        // shiki の入口でも断る（OpenFGA が断るものは必ず断る）。
        assert!(authz::validate_user_id(id).is_err(), "{id:?} は入口で断る");
    }
}

#[tokio::test]
async fn a_pipe_in_the_user_is_accepted_by_openfga_so_the_entrance_must_reject_it() {
    let Some(client) = connect().await else {
        return;
    };
    let ns = Namespace::for_tenant(TENANT);
    let org = ns.organization("acme");
    let user = ns.user("a|b");
    assert_eq!(user.as_str(), "user:t|a|b");

    // 書けて、check も通る（ログインの同期がこの人を member にできてしまう）。
    assert!(client
        .write_tuple(&user, Relation::Member, &org)
        .await
        .expect("`|` を含む user は書ける"));
    assert!(client
        .check(
            &user,
            Relation::Member,
            &org,
            Consistency::HigherConsistency
        )
        .await
        .expect("check できる"));
    // 読み出しにもそのまま出る（object 側からも subject 側からも）。
    let tuples = client
        .read_tuples(&org, Some(Relation::Member))
        .await
        .expect("read");
    assert!(tuples.iter().any(|t| t.user == "user:t|a|b"), "{tuples:?}");
    let orgs = client
        .read_subject_objects(&user, ObjectType::Organization)
        .await
        .expect("read");
    assert_eq!(orgs, vec!["organization:t|acme".to_string()]);
    // `<tenant>|` の後ろ（`a|b`）は local id として通らない。定期の照合はこの人を飛ばすので（D-136）、
    // 入口で断らないと外れない。
    assert_eq!(ns.parse_user_subject("user:t|a|b"), Some("a|b"));
    assert!(authz::validate_local_id("a|b").is_err());
    assert!(authz::validate_user_id("a|b").is_err());

    client
        .delete_tuple(&user, Relation::Member, &org)
        .await
        .expect("片付け");
}

#[tokio::test]
async fn unicode_whitespace_and_controls_in_the_user_are_accepted_by_openfga() {
    let Some(client) = connect().await else {
        return;
    };
    let ns = Namespace::for_tenant(TENANT);
    let org = ns.organization("acme");
    // OpenFGA の user の形の決まりは `^[^\s]{2,512}$`（ASCII の空白だけ）なので、これらは通る。shiki は入口で断る。
    for id in ["a\u{3000}b", "a\u{a0}b", "a\u{7}b"] {
        let user = ns.user(id);
        assert!(
            client
                .write_tuple(&user, Relation::Member, &org)
                .await
                .is_ok(),
            "{id:?} は OpenFGA が受け付ける"
        );
        assert!(client
            .check(
                &user,
                Relation::Member,
                &org,
                Consistency::HigherConsistency
            )
            .await
            .expect("check できる"));
        assert!(authz::validate_user_id(id).is_err(), "{id:?} は入口で断る");
        client
            .delete_tuple(&user, Relation::Member, &org)
            .await
            .expect("片付け");
    }
}

#[tokio::test]
async fn ascii_whitespace_in_a_role_id_is_rejected_by_openfga() {
    let Some(client) = connect().await else {
        return;
    };
    let ns = Namespace::for_tenant(TENANT);
    let alice = ns.user("alice");
    // 空白を含むグループ名から作る部署の id（`acme/営業 1課`）。validate_local_id は通るが、OpenFGA は書き込みも
    // check も読み出しも 400 で断る（object の形の決まりは `^[^\s]{2,256}$`）。
    let id = "acme/営業 1課";
    assert_eq!(authz::validate_local_id(id), Ok(()));
    let role = ns.role(id);
    let written = client.write_tuple(&alice, Relation::Member, &role).await;
    assert!(is_rejected(&written), "書き込みは 400: {written:?}");
    let checked = client
        .check(
            &alice,
            Relation::Member,
            &role,
            Consistency::HigherConsistency,
        )
        .await;
    assert!(is_rejected(&checked), "check は 400: {checked:?}");
    let read = client.read_tuples(&role, None).await;
    assert!(is_rejected(&read), "読み出しは 400: {read:?}");
}

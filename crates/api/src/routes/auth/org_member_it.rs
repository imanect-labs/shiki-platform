//! 実 Keycloak・実 OpenFGA・実 Postgres で、アプリのゲートウェイが組織をインストールから決め、その組織の member
//! だけを通すことを確かめる（D-130）。
//!
//! `KEYCLOAK_TEST_URL`（realm `shiki` を import 済み）・`STORAGE_TEST_DATABASE_URL`・`OPENFGA_TEST_URL` のどれかが
//! 無ければスキップする。テナントは realm の試しの利用者に合わせて `a-corp`（multi）。作ったクライアント・行・
//! OpenFGA のストアは、テストの最後に消す（[`super::gateway_fixture`]）。
//!
//! 1. 同意インストールで登録した B1 のクライアントのトークンには、所属グループ（`groups`）が載らない。
//! 2. member のタプルが無い人（ログインしていない人）はゲートウェイで 403。
//! 3. ログインの同期（[`super::org_member::sync_org_member`]）を Keycloak の所属グループで通すと 200 になる。
//! 4. 同じテナントで同期していない人（bob）は 403。別のテナントの人（charlie）は、自分のテナントで同期しても 403。
//! 5. 所属グループから外れた（グループが無い）人を同期し直すと、member が外れて 403 に戻る。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::large_futures,
    clippy::too_many_lines
)]

use std::panic::AssertUnwindSafe;

use app_gateway::test_support::{claims, TestRealm};
use authz::{GroupLayout, Principal, PrincipalKind};
use futures::FutureExt as _;

use super::gateway_fixture::{GatewayFixture, SHELL_CALLBACK, TENANT};
use super::org_member::sync_org_member;

/// realm の試しの利用者（deploy/keycloak/shiki-realm.json）の sub。
const ALICE: &str = "00000000-0000-0000-0000-000000000001";
const CHARLIE: &str = "00000000-0000-0000-0000-000000000003";

/// Keycloak の所属グループを claims に持つ利用者（ログインのクライアントのトークンの groups と同じ値）。
pub(super) async fn principal(
    realm: &TestRealm,
    username: &str,
    sub: &str,
    tenant: &str,
) -> Principal {
    Principal {
        kind: PrincipalKind::User,
        id: sub.into(),
        email: None,
        groups: realm.user_group_paths(username).await,
        roles: vec![],
        tenant_id: Some(tenant.into()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gateway_admits_only_members_of_the_installing_org() {
    let Some((realm, fx)) = GatewayFixture::new("org-member-it").await else {
        return;
    };
    let authz = fx.authz.clone();

    let body = async {
        fx.install_and_serve(&realm).await;
        let token_of = |username: &'static str| {
            let realm = &realm;
            let b1 = fx.b1.clone();
            async move {
                realm
                    .user_token(&b1, SHELL_CALLBACK, username, "openid data.read")
                    .await
                    .expect("token")
            }
        };

        // 1. アプリのトークンに所属グループは載らない（テナントと sub は載る）。
        let alice = token_of("alice").await;
        let raw = claims(&alice);
        assert!(raw.get("groups").is_none(), "{raw}");
        assert_eq!(raw["tenant"], TENANT);
        assert_eq!(raw["sub"], ALICE);

        // 2. member のタプルが無ければ、同じ組織の人でも 403。
        let (s, body) = fx.whoami(&alice).await;
        assert_eq!(s, 403, "{body}");
        assert_eq!(
            body["error"],
            "権限がありません: このアプリを使えるのは、アプリをインストールした組織のメンバーだけです"
        );

        // 3. ログインの同期（Keycloak の所属グループ）で member が付き、通る。
        let synced = sync_org_member(
            &*authz,
            &principal(&realm, "alice", ALICE, TENANT).await,
            TENANT,
            GroupLayout::TenantScoped,
        )
        .await;
        assert_eq!(synced.desired.as_deref(), Some(TENANT));
        assert!(synced.granted);
        let (s, body) = fx.whoami(&alice).await;
        assert_eq!(s, 200, "{body}");
        assert_eq!(body["user_sub"], ALICE);
        assert_eq!(body["app_id"], fx.app_id.to_string());

        // 4. 同期していない bob は 403。別のテナントの charlie は、自分のテナントで同期しても 403
        //    （そのテナントにこのアプリのインストールが無い）。
        let (s, _) = fx.whoami(&token_of("bob").await).await;
        assert_eq!(s, 403, "bob（member でない）");
        let charlie = principal(&realm, "charlie", CHARLIE, "b-corp").await;
        assert!(
            sync_org_member(&*authz, &charlie, "b-corp", GroupLayout::TenantScoped)
                .await
                .granted
        );
        let charlie_token = token_of("charlie").await;
        assert_eq!(claims(&charlie_token)["tenant"], "b-corp");
        let (s, body) = fx.whoami(&charlie_token).await;
        assert_eq!(s, 403, "charlie（別のテナント）: {body}");

        // 5. 所属グループから外れた alice を同期し直すと、member が外れて 403 に戻る。
        let mut left = principal(&realm, "alice", ALICE, TENANT).await;
        left.groups.clear();
        let revoked = sync_org_member(&*authz, &left, TENANT, GroupLayout::TenantScoped).await;
        assert_eq!(revoked.revoked, [TENANT]);
        let (s, _) = fx.whoami(&alice).await;
        assert_eq!(s, 403, "組織を離れた人は通さない");
    };
    let outcome = AssertUnwindSafe(body).catch_unwind().await;
    fx.cleanup(&realm).await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

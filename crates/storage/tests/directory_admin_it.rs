//! PostgreSQLを使って版管理・再実行・所属解決・組織分離を検証する。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::pedantic
)]
use authz::{AuthContext, Principal, PrincipalKind, TenantId};
use sqlx::postgres::PgPoolOptions;
use storage::{directory::admin::*, DirectoryStore};
use uuid::Uuid;
fn context(tenant: &str, org: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            id: "admin".into(),
            kind: PrincipalKind::User,
            email: None,
            roles: vec![],
            groups: vec![],
            tenant_id: Some(tenant.into()),
        },
        org.to_string(),
        TenantId::parse(tenant).unwrap(),
    )
}
async fn setup() -> Option<(DirectoryStore, AuthContext)> {
    let Ok(url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(6)
        .connect(&url)
        .await
        .unwrap();
    sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
    let tenant = format!("admin-{}", Uuid::new_v4().simple());
    Some((DirectoryStore::new(pool), context(&tenant, "org")))
}
fn edit(ctx: &AuthContext, name: &str, parent: Option<&str>) -> RoleEdit {
    RoleEdit {
        id: format!("{}{name}", role_id_prefix(ctx)),
        label: name.into(),
        parent_id: parent.map(str::to_string),
        expected_version: 0,
    }
}
fn group(ctx: &AuthContext, id: &str, ou: &[&str]) -> AdGroup {
    AdGroup {
        id: id.into(),
        name: id.into(),
        claim_path: format!("/tenants/{}/{}/ad/{id}", ctx.tenant_id(), ctx.org()),
        distinguished_name: Some(format!("CN={id},OU=Sales,DC=example")),
        mapping_version: 0,
        role_ids: vec![],
        ou_path: ou.iter().map(|s| (*s).into()).collect(),
        parent_ids: vec![],
    }
}
async fn apply(
    store: &DirectoryAdminStore,
    ctx: &AuthContext,
    input: &DirectoryChangeSet,
) -> DirectoryOperation {
    let prepared = store.validate(ctx, input).await.unwrap();
    let op = store
        .enqueue(ctx, &prepared, &request_hash(input).unwrap(), None)
        .await
        .unwrap();
    let (request, done) = store.start_operation(ctx, op.id).await.unwrap().unwrap();
    assert_eq!(done, 0);
    store
        .reserve_new_roles(ctx, &request.roles, op.id)
        .await
        .unwrap();
    for (step, role) in request.roles.iter().enumerate() {
        store
            .save_role(ctx, role, "kc-id", op.id, step as i32)
            .await
            .unwrap();
    }
    for (step, mapping) in request.mappings.iter().enumerate() {
        store
            .save_mapping(ctx, mapping, op.id, (request.roles.len() + step) as i32)
            .await
            .unwrap();
    }
    store.operation(ctx, op.id).await.unwrap()
}
#[tokio::test]
async fn role_mapping_and_replay_are_scoped_and_versioned() {
    let Some((directory, ctx)) = setup().await else {
        return;
    };
    let store = directory.admin();
    let root = edit(&ctx, "Sales", None);
    let child = edit(&ctx, "Team", Some(&root.id));
    let g = group(&ctx, "g1", &["HQ", "Sales"]);
    store
        .replace_groups(&ctx, std::slice::from_ref(&g), None)
        .await
        .unwrap();
    let request = DirectoryChangeSet {
        idempotency_key: Uuid::new_v4(),
        roles: vec![child.clone(), root.clone()],
        mappings: vec![GroupRoleMapping {
            group_id: g.id.clone(),
            role_ids: vec![child.id.clone()],
            expected_version: 0,
        }],
    };
    let op = apply(&store, &ctx, &request).await;
    assert_eq!(op.state, "succeeded");
    assert_eq!(op.processed, 3);
    assert_eq!(
        store
            .existing_request(
                &ctx,
                request.idempotency_key,
                &request_hash(&request).unwrap()
            )
            .await
            .unwrap()
            .unwrap()
            .id,
        op.id
    );
    let mut changed = request.clone();
    changed.roles[0].label = "Other".into();
    assert!(store
        .existing_request(
            &ctx,
            request.idempotency_key,
            &request_hash(&changed).unwrap()
        )
        .await
        .is_err());
    assert!(store.validate(&ctx, &request).await.is_err());
    let found = store
        .roles(&ctx, "", Some(&root.id), None, 40)
        .await
        .unwrap();
    assert_eq!(found.items[0].path_labels, ["Sales", "Team"]);
    assert_eq!(
        store
            .roles(&ctx, "Sales", None, None, 40)
            .await
            .unwrap()
            .items[0]
            .child_count,
        1
    );
    assert_eq!(
        store
            .mapped_roles(&ctx, std::slice::from_ref(&g.claim_path))
            .await
            .unwrap(),
        vec![child.id.clone()]
    );
    for other in [
        context(ctx.tenant_id(), "other-org"),
        context("other-tenant", ctx.org()),
    ] {
        assert!(store
            .roles(&other, "", None, None, 40)
            .await
            .unwrap()
            .items
            .is_empty());
        assert!(store
            .mapped_roles(&other, std::slice::from_ref(&g.claim_path))
            .await
            .unwrap()
            .is_empty());
        assert!(store.operation(&other, op.id).await.is_err());
        assert!(store
            .roles_by_ids(&other, std::slice::from_ref(&child.id))
            .await
            .unwrap()
            .is_empty());
        assert!(store
            .existing_request(
                &other,
                request.idempotency_key,
                &request_hash(&request).unwrap()
            )
            .await
            .unwrap()
            .is_none());
    }
    // claim射影の更新は管理者が付けた表示名を上書きしない。
    directory
        .upsert_role(&child.id, ctx.tenant_id(), ctx.org(), "raw claim")
        .await
        .unwrap();
    assert_eq!(
        store.role(&ctx, &child.id).await.unwrap().unwrap().label,
        "Team"
    );
    let folders = store.folders(&ctx, &[], None).await.unwrap();
    assert_eq!(folders.items[0].name, "HQ");
    assert!(store
        .groups(&ctx, "", None, 40, &[])
        .await
        .unwrap()
        .items
        .is_empty());
    assert_eq!(
        store
            .groups(&ctx, "", None, 40, &["HQ".into(), "Sales".into()])
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    assert_eq!(
        store
            .groups(&ctx, "g1", None, 40, &[])
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    store.replace_groups(&ctx, &[], None).await.unwrap();
    assert!(store
        .mapped_roles(&ctx, &[g.claim_path])
        .await
        .unwrap()
        .is_empty());
}
#[tokio::test]
async fn validation_rejects_foreign_ids_cycles_collisions_and_stale_groups() {
    let Some((directory, ctx)) = setup().await else {
        return;
    };
    let store = directory.admin();
    let a = edit(&ctx, "a", None);
    let b = edit(&ctx, "b", Some(&a.id));
    let input = |roles| DirectoryChangeSet {
        idempotency_key: Uuid::new_v4(),
        roles,
        mappings: vec![],
    };
    apply(&store, &ctx, &input(vec![a.clone(), b.clone()])).await;
    let mut looped = a.clone();
    looped.expected_version = 1;
    looped.parent_id = Some(b.id.clone());
    assert!(store.validate(&ctx, &input(vec![looped])).await.is_err());
    let mut duplicate = edit(&ctx, "new", None);
    duplicate.label = "A".into();
    assert!(store.validate(&ctx, &input(vec![duplicate])).await.is_err());
    let mut foreign = edit(&ctx, "foreign", None);
    foreign.id = "unscoped-id".into();
    assert!(store.validate(&ctx, &input(vec![foreign])).await.is_err());
    let mut missing = edit(&ctx, "missing", Some("missing"));
    assert!(store
        .validate(&ctx, &input(vec![missing.clone()]))
        .await
        .is_err());
    missing.parent_id = Some(missing.id.clone());
    assert!(store.validate(&ctx, &input(vec![missing])).await.is_err());
    assert!(store.validate(&ctx, &input(vec![])).await.is_err());
    let mut invalid_label = edit(&ctx, "control", None);
    invalid_label.label = "Invalid\0label".into();
    assert!(store
        .validate(&ctx, &input(vec![invalid_label]))
        .await
        .is_err());
    let mut deep = Vec::new();
    for n in 0..130 {
        deep.push(edit(
            &ctx,
            &format!("depth-{n:03}"),
            deep.last().map(|r: &RoleEdit| r.id.as_str()),
        ));
    }
    assert!(store.validate(&ctx, &input(deep.clone())).await.is_err());
    deep.truncate(128);
    assert!(store.validate(&ctx, &input(deep.clone())).await.is_ok());
    deep.truncate(127);
    let mut moved_parent = a.clone();
    moved_parent.expected_version = 1;
    moved_parent.parent_id = Some(deep.last().unwrap().id.clone());
    deep.push(moved_parent);
    // Parent depth 128 is valid alone, but its existing child would become 129.
    assert!(store.validate(&ctx, &input(deep)).await.is_err());
    let g = group(&ctx, "g", &[]);
    store.cache_group(&ctx, &g).await.unwrap();
    let mut changed = g.clone();
    changed.name = "renamed".into();
    store.cache_group(&ctx, &changed).await.unwrap();
    let mut request = input(vec![]);
    request.mappings.push(GroupRoleMapping {
        group_id: "g".into(),
        role_ids: vec![a.id],
        expected_version: 0,
    });
    assert!(store.validate(&ctx, &request).await.is_err());
}
#[tokio::test]
async fn interrupted_operation_is_durable_and_org_lock_is_exclusive() {
    let Some((directory, ctx)) = setup().await else {
        return;
    };
    let store = directory.admin();
    let lease = store.lease(&ctx).await.unwrap().unwrap();
    assert!(store.lease(&ctx).await.unwrap().is_none());
    drop(lease);
    let request = DirectoryChangeSet {
        idempotency_key: Uuid::new_v4(),
        roles: vec![edit(&ctx, "one", None), edit(&ctx, "two", None)],
        mappings: vec![],
    };
    let prepared = store.validate(&ctx, &request).await.unwrap();
    let hash = request_hash(&request).unwrap();
    let op = store
        .enqueue(&ctx, &prepared, &hash, Some("trace"))
        .await
        .unwrap();
    let mut another = request.clone();
    another.idempotency_key = Uuid::new_v4();
    assert!(store
        .enqueue(&ctx, &another, &request_hash(&another).unwrap(), None)
        .await
        .is_err());
    let (changes, _) = store.start_operation(&ctx, op.id).await.unwrap().unwrap();
    store
        .reserve_new_roles(&ctx, &changes.roles, op.id)
        .await
        .unwrap();
    store
        .save_role(&ctx, &changes.roles[0], "kc", op.id, 0)
        .await
        .unwrap();
    store
        .fail_operation(&ctx, op.id, "synchronization_failed")
        .await
        .unwrap();
    assert_eq!(store.operation(&ctx, op.id).await.unwrap().processed, 1);
    assert_eq!(
        store
            .retry_operation(&ctx, op.id, None)
            .await
            .unwrap()
            .state,
        "pending"
    );
    let (changes, processed) = store.start_operation(&ctx, op.id).await.unwrap().unwrap();
    assert_eq!(processed, 1);
    store
        .save_role(&ctx, &changes.roles[1], "kc", op.id, 1)
        .await
        .unwrap();
    assert_eq!(
        store.operation(&ctx, op.id).await.unwrap().state,
        "succeeded"
    );
    assert!(store.start_operation(&ctx, op.id).await.unwrap().is_none());
    assert_eq!(store.operations(&ctx).await.unwrap().len(), 1);
}

#[tokio::test]
async fn recreated_groups_do_not_inherit_mappings_and_rename_order_is_valid() {
    let Some((directory, ctx)) = setup().await else {
        return;
    };
    let store = directory.admin();
    let a = edit(&ctx, "a", None);
    let b = edit(&ctx, "b", None);
    let g = group(&ctx, "g", &[]);
    store
        .replace_groups(&ctx, std::slice::from_ref(&g), None)
        .await
        .unwrap();
    apply(
        &store,
        &ctx,
        &DirectoryChangeSet {
            idempotency_key: Uuid::new_v4(),
            roles: vec![a.clone(), b.clone()],
            mappings: vec![GroupRoleMapping {
                group_id: g.id.clone(),
                role_ids: vec![a.id.clone()],
                expected_version: 0,
            }],
        },
    )
    .await;
    let mut recreated = g.clone();
    recreated.id = "new-id".into();
    store
        .replace_groups(&ctx, &[recreated], None)
        .await
        .unwrap();
    assert!(store
        .mapped_roles(&ctx, std::slice::from_ref(&g.claim_path))
        .await
        .unwrap()
        .is_empty());
    let mut renamed_a = a.clone();
    renamed_a.label = "b".into();
    renamed_a.expected_version = 1;
    let mut renamed_b = b;
    renamed_b.label = "c".into();
    renamed_b.expected_version = 1;
    let change = DirectoryChangeSet {
        idempotency_key: Uuid::new_v4(),
        roles: vec![renamed_a, renamed_b],
        mappings: vec![],
    };
    assert_eq!(
        store.validate(&ctx, &change).await.unwrap().roles[0].label,
        "c"
    );
    assert_eq!(apply(&store, &ctx, &change).await.state, "succeeded");
    let mut other = group(&ctx, "other", &[]);
    other.claim_path.push_str("-other");
    store
        .replace_groups(&ctx, &[g.clone(), other.clone()], None)
        .await
        .unwrap();
    let first_path = g.claim_path.clone();
    let mut swapped = g;
    swapped.claim_path = other.claim_path.clone();
    other.claim_path = first_path;
    store
        .replace_groups(&ctx, &[swapped, other], None)
        .await
        .unwrap();
    // 組織ロックはテナント撤去と相互排他。別組織も同じtenantの撤去を止める。
    let org_lease = store.lease(&ctx).await.unwrap().unwrap();
    assert!(store
        .tenant_deletion_lease(ctx.tenant())
        .await
        .unwrap()
        .is_none());
    drop(org_lease);
    let deletion = store
        .tenant_deletion_lease(ctx.tenant())
        .await
        .unwrap()
        .unwrap();
    assert!(store.lease(&ctx).await.unwrap().is_none());
    drop(deletion);
}

#[tokio::test]
async fn only_owned_role_definitions_can_be_edited_and_ids_are_reserved_before_writes() {
    let Some((directory, ctx)) = setup().await else {
        return;
    };
    let store = directory.admin();
    let role = edit(&ctx, "owned", None);
    let other = context(ctx.tenant_id(), "other");
    directory
        .upsert_role(&role.id, ctx.tenant_id(), other.org(), "IdP role")
        .await
        .unwrap();
    let input = DirectoryChangeSet {
        idempotency_key: Uuid::new_v4(),
        roles: vec![role.clone()],
        mappings: vec![],
    };
    assert!(store.validate(&ctx, &input).await.is_err());
    let own_legacy = edit(&ctx, "legacy", None);
    directory
        .upsert_role(&own_legacy.id, ctx.tenant_id(), ctx.org(), "Legacy")
        .await
        .unwrap();
    let mut legacy = input.clone();
    legacy.roles = vec![own_legacy];
    assert!(store.validate(&ctx, &legacy).await.is_err());
    let created = edit(&ctx, "fresh", None);
    let mut request = input;
    request.roles = vec![created.clone()];
    let prepared = store.validate(&ctx, &request).await.unwrap();
    let op = store
        .enqueue(&ctx, &prepared, &request_hash(&request).unwrap(), None)
        .await
        .unwrap();
    store.start_operation(&ctx, op.id).await.unwrap();
    store
        .reserve_new_roles(&ctx, &prepared.roles, op.id)
        .await
        .unwrap();
    store
        .reserve_new_roles(&ctx, &prepared.roles, op.id)
        .await
        .unwrap();
    assert!(store
        .roles(&ctx, "fresh", None, None, 40)
        .await
        .unwrap()
        .items
        .is_empty());
    assert!(store
        .roles_by_ids(&ctx, std::slice::from_ref(&created.id))
        .await
        .unwrap()
        .is_empty());
    assert!(directory
        .upsert_role(&created.id, ctx.tenant_id(), other.org(), "foreign claim")
        .await
        .is_err());
    store
        .save_role(&ctx, &created, "kc", op.id, 0)
        .await
        .unwrap();
    assert_eq!(
        store
            .roles(&ctx, "fresh", None, None, 40)
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    let mut invalid = edit(&ctx, "invalid", None);
    invalid.id.push(',');
    let bad = DirectoryChangeSet {
        idempotency_key: Uuid::new_v4(),
        roles: vec![invalid],
        mappings: vec![],
    };
    assert!(store.validate(&ctx, &bad).await.is_err());
}

use super::*;
use authz::{AuthContext, TenantId};
use axum::extract::{Extension, Path, State};
use uuid::Uuid;

#[tokio::test]
async fn backup_hold_prevents_all_tenant_delete_side_effects() {
    let Ok(url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        return;
    };
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .unwrap();
    sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
    let tenant = format!("backup-http-{}", Uuid::new_v4().simple());
    let typed = TenantId::parse(tenant.clone()).unwrap();
    let mut state = state_with(Arc::new(MemorySessionStore::new()), None);
    Arc::make_mut(&mut state.config).auth.tenancy = Tenancy::Multi;
    state.storage = Arc::new(storage::StorageService::new(
        pool.clone(),
        Arc::new(FakeStore),
        Arc::new(AllowAll),
        Duration::from_secs(60),
        Duration::from_secs(60),
        1024,
    ));
    state.tenants = Arc::new(storage::TenantStore::new(pool.clone()));
    state
        .tenants
        .upsert_active(&tenant, &tenant, "backup test")
        .await
        .unwrap();
    state.data = Arc::new(data::DataStore::new(
        pool.clone(),
        Arc::new(AllowAll),
        Arc::new(api::data_refs::ApiRefResolver {
            directory: Arc::new(storage::DirectoryStore::new(pool.clone())),
            storage: state.storage.clone(),
        }),
    ));
    let ctx = AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.clone()),
        },
        tenant.clone(),
        typed.clone(),
    );
    let table = state
        .data
        .create_table(
            &ctx,
            data::NewDataTable {
                name: "Protected rows".into(),
                schema: serde_json::from_value(
                    serde_json::json!({"fields":[{"key":"value","type":"text"}]}),
                )
                .unwrap(),
            },
            None,
        )
        .await
        .unwrap();
    let archive = storage::blob_storage::BlobStorage::new(pool.clone(), Arc::new(FakeStore));
    let hold = Uuid::new_v4();
    archive
        .hold_backup(&typed, &tenant, hold, Duration::from_secs(60))
        .await
        .unwrap();
    let result = api::routes::admin::delete_tenant(
        State(state.clone()),
        Extension(api::routes::admin::ProvisionerIdentity("test".into())),
        Path(tenant.clone()),
    )
    .await;
    assert!(
        matches!(result, Err(api::error::ApiError::BadRequest(ref message)) if message.contains("バックアップ"))
    );
    assert_eq!(
        state.tenants.get(&tenant).await.unwrap().unwrap().status,
        storage::TenantStatus::Active
    );
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM data_table WHERE tenant_id = $1 AND id = $2)",
    )
    .bind(&tenant)
    .bind(table.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(exists, "保護チェック前に構造化データを削除しない");
    archive.release_backup(&typed, &tenant, hold).await.unwrap();
    let guard = state.storage.begin_tenant_purge(&typed).await.unwrap();
    assert!(matches!(
        archive
            .hold_backup(&typed, &tenant, Uuid::new_v4(), Duration::from_secs(60))
            .await,
        Err(storage::StorageError::NotFound)
    ));
    state.tenants.mark_deleting(&tenant).await.unwrap();
    state.data.purge_tenant(&tenant).await.unwrap();
    tokio::time::timeout(
        Duration::from_secs(10),
        state
            .storage
            .purge_tenant_with_guard(&guard, &tenant, "test"),
    )
    .await
    .unwrap()
    .unwrap();
    state.tenants.mark_deleted(&tenant).await.unwrap();
}

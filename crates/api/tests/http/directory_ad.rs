use super::*;

#[tokio::test]
async fn duplicate_cn_groups_are_separate_and_memberof_restores_their_parent() {
    let Some((state, ctx, fixture)) = fixture().await else {
        return;
    };
    let app = build_router(state.clone());
    let base = "/org-admin/directory";
    let input = json!({"connection_url":"ldaps://ad.example.test:636","users_dn":"OU=Users,DC=example","groups_dn":"OU=Groups,DC=example","bind_dn":"CN=reader,DC=example","bind_password":"test-secret","preserve_group_inheritance":true});
    assert_eq!(
        call(&app, "PUT", &format!("{base}/connection"), input)
            .await
            .0,
        StatusCode::OK
    );
    let parent = "CN=Parent,OU=Groups,DC=example";
    let sources = {
        let mut kc = fixture.lock().unwrap();
        let mapper = kc
            .components
            .iter()
            .find(|c| c["providerId"] == "shiki-ad-guid-group-mapper")
            .unwrap();
        assert_eq!(
            mapper["config"]["group.name.ldap.attribute"][0],
            "objectGUID"
        );
        assert_eq!(mapper["config"]["preserve.group.inheritance"][0], "false");
        assert_eq!(
            mapper["config"]["mapped.group.attributes"][0],
            "cn,distinguishedName,memberOf"
        );
        assert_eq!(
            mapper["config"]["user.roles.retrieve.strategy"][0],
            "LOAD_GROUPS_BY_MEMBER_ATTRIBUTE_RECURSIVELY"
        );
        let root = kc
            .groups
            .iter()
            .find(|g| g["path"] == format!("/tenants/default/{}/ad-guid-v1", ctx.org()))
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let p = kc.add_group(
            Some(&root),
            &uuid::Uuid::new_v4().to_string(),
            json!({"cn":["Parent"],"distinguishedName":[parent]}),
        );
        let mut ids = vec![p];
        for ou in ["East", "West"] {
            let dn = format!("CN=Sales,OU={ou},OU=Groups,DC=example");
            ids.push(kc.add_group(Some(&root),&uuid::Uuid::new_v4().to_string(),json!({"cn":["Sales"],"distinguishedName":[dn],"memberOf":["cn=parent,ou=groups,dc=EXAMPLE"]})));
        }
        ids
    };
    let op = api::directory_admin::submit_sync(&state, &ctx, uuid::Uuid::new_v4(), None)
        .await
        .unwrap();
    api::directory_admin::process(&state, &ctx, op.id)
        .await
        .unwrap();
    let groups = state
        .directory
        .admin()
        .groups_for_plan(&ctx, None, false)
        .await
        .unwrap();
    assert_eq!(groups.len(), 3);
    let sales: Vec<_> = groups.iter().filter(|g| g.name == "Sales").collect();
    assert_eq!(sales.len(), 2);
    assert_ne!(sales[0].id, sales[1].id);
    assert_ne!(sales[0].claim_path, sales[1].claim_path);
    assert!(sales
        .iter()
        .all(|g| uuid::Uuid::parse_str(g.claim_path.rsplit('/').next().unwrap()).is_ok()));
    assert!(sales
        .iter()
        .all(|g| g.parent_ids == vec![sources[0].clone()]));
    let (_, plan) = call(
        &app,
        "POST",
        &format!("{base}/auto-plan"),
        json!({"hierarchy":"ou"}),
    )
    .await;
    assert_eq!(plan["group_count"], 3);
    assert_eq!(
        plan["sources"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|g| g["name"] == "Sales")
            .count(),
        2
    );
}

#[tokio::test]
async fn already_stopped_worker_does_not_claim_pending_operations() {
    let Some((state, ctx, fixture)) = fixture().await else {
        return;
    };
    let op = api::directory_admin::submit_sync(&state, &ctx, uuid::Uuid::new_v4(), None)
        .await
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(1),
        api::directory_admin::run(
            state.clone(),
            std::future::ready(()),
            Duration::from_millis(10),
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        state
            .directory
            .admin()
            .operation(&ctx, op.id)
            .await
            .unwrap()
            .state,
        "pending"
    );
    assert!(fixture.lock().unwrap().writes.is_empty());
}

#[tokio::test]
async fn old_identity_format_requires_confirmation_and_does_not_transfer_bindings_to_a_reused_id() {
    let Some((state, ctx, fixture)) = fixture().await else {
        return;
    };
    let app = build_router(state.clone());
    let base = "/org-admin/directory";
    let mut input = json!({"connection_url":"ldaps://ad.example.test:636","users_dn":"OU=Users,DC=example","groups_dn":"OU=Groups,DC=example","bind_dn":"CN=reader,DC=example","bind_password":"test-secret"});
    assert_eq!(
        call(&app, "PUT", &format!("{base}/connection"), input.clone())
            .await
            .0,
        StatusCode::OK
    );
    let identity = uuid::Uuid::new_v4().to_string();
    let path = format!("/tenants/default/{}/ad-guid-v1/{identity}", ctx.org());
    let group:storage::directory::admin::AdGroup = serde_json::from_value(json!({"id":"reused-id","name":identity,"claim_path":path,"distinguished_name":"CN=Old,OU=Groups,DC=example","ou_path":["Groups"],"parent_ids":[],"role_ids":[],"mapping_version":0})).unwrap();
    state
        .directory
        .admin()
        .cache_group(&ctx, &group)
        .await
        .unwrap();
    let role = format!("{}legacy", storage::directory::admin::role_id_prefix(&ctx));
    let request = serde_json::from_value(json!({"idempotency_key":uuid::Uuid::new_v4(),"roles":[{"id":role,"label":"Protected role","parent_id":null,"expected_version":0}],"mappings":[{"group_id":group.id,"role_ids":[role],"expected_version":0}]})).unwrap();
    let op = api::directory_admin::submit(&state, &ctx, &request, None)
        .await
        .unwrap();
    api::directory_admin::process(&state, &ctx, op.id)
        .await
        .unwrap();
    {
        let mut kc = fixture.lock().unwrap();
        let provider = kc
            .components
            .iter_mut()
            .find(|c| c["providerId"] == "ldap")
            .unwrap();
        provider["config"]["shiki.groupIdentity"] = json!(["cn-v0"]);
        let mapper = kc
            .components
            .iter_mut()
            .find(|c| c["providerId"] == "shiki-ad-guid-group-mapper")
            .unwrap();
        mapper["providerId"] = json!("group-ldap-mapper");
        mapper["config"]["group.name.ldap.attribute"] = json!(["cn"]);
        let root = kc
            .groups
            .iter()
            .find(|g| g["path"] == format!("/tenants/default/{}/ad-guid-v1", ctx.org()))
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        kc.add_group(Some(&root),&identity,json!({"cn":["Different group"],"distinguishedName":["CN=Different,OU=Groups,DC=example"]}));
        kc.groups.last_mut().unwrap()["id"] = json!("reused-id");
    }
    assert_eq!(
        call(&app, "PUT", &format!("{base}/connection"), input.clone())
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        state
            .directory
            .admin()
            .mapped_roles(&ctx, std::slice::from_ref(&path))
            .await
            .unwrap(),
        vec![role]
    );
    input["identity_reset_confirmed"] = json!(true);
    assert_eq!(
        call(&app, "PUT", &format!("{base}/connection"), input)
            .await
            .0,
        StatusCode::OK
    );
    {
        let mut kc = fixture.lock().unwrap();
        assert!(!kc.groups.iter().any(|g| g["id"] == "reused-id"));
        let root = kc
            .groups
            .iter()
            .find(|g| g["path"] == format!("/tenants/default/{}/ad-guid-v1", ctx.org()))
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        kc.add_group(Some(&root),&identity,json!({"cn":["Different group"],"distinguishedName":["CN=Different,OU=Groups,DC=example"]}));
    }
    let op = api::directory_admin::submit_sync(&state, &ctx, uuid::Uuid::new_v4(), None)
        .await
        .unwrap();
    api::directory_admin::process(&state, &ctx, op.id)
        .await
        .unwrap();
    let groups = state
        .directory
        .admin()
        .groups_for_plan(&ctx, None, false)
        .await
        .unwrap();
    assert_ne!(groups[0].id, "reused-id");
    assert_eq!(groups[0].name, "Different group");
    assert!(groups[0].role_ids.is_empty());
    assert!(state
        .directory
        .admin()
        .mapped_roles(&ctx, &[path])
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn role_container_rejects_ldap_mappers_below_a_provider() {
    let Some((state, ctx, fixture)) = fixture().await else {
        return;
    };
    let sdk =
        api::keycloak_admin::KeycloakAdmin::from_config(&state.http, &state.config.auth).unwrap();
    let path = format!("/__shiki_roles/{}/{}", ctx.tenant_id(), ctx.org());
    {
        let mut kc = fixture.lock().unwrap();
        kc.components.push(json!({"id":"external-ldap", "parentId":"realm-id", "providerId":"ldap", "providerType":"org.keycloak.storage.UserStorageProvider"}));
        kc.components.push(json!({"id":"external-groups", "parentId":"external-ldap", "providerId":"group-ldap-mapper", "providerType":"org.keycloak.storage.ldap.mappers.LDAPStorageMapper", "name":"external-groups", "config":{"groups.path":[path]}}));
    }
    assert!(sdk
        .directory_role_container(ctx.tenant_id(), ctx.org())
        .await
        .is_err());
    assert!(fixture.lock().unwrap().groups.is_empty());
    fixture.lock().unwrap().components.last_mut().unwrap()["config"]["groups.path"] =
        json!(["/unrelated"]);
    assert!(sdk
        .directory_role_container(ctx.tenant_id(), ctx.org())
        .await
        .is_ok());
}

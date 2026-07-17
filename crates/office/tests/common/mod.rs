//! office IT 共通モック（wopi_it / edit_it で共用）。
//!
//! - [`RoleAuthz`]: (subject, relation) 付与集合の authz モック（revoke 可能）。
//! - [`MemStore`]: バイト保持の in-memory ObjectStore。
//! - [`ctx_for`]: テスト用 AuthContext。

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};

/// (subject, relation) 付与集合を持つ authz モック（revoke 可能）。
pub struct RoleAuthz {
    grants: Mutex<HashSet<(String, Relation)>>,
}
impl RoleAuthz {
    pub fn new() -> Self {
        RoleAuthz {
            grants: Mutex::new(HashSet::new()),
        }
    }
    pub fn grant(&self, subject: &Subject, relation: Relation) {
        self.grants
            .lock()
            .unwrap()
            .insert((subject.as_str().to_string(), relation));
    }
    /// 共有解除（relation 剥奪）を模す。
    pub fn revoke(&self, subject: &Subject, relation: Relation) {
        self.grants
            .lock()
            .unwrap()
            .remove(&(subject.as_str().to_string(), relation));
    }
}
#[async_trait]
impl AuthzClient for RoleAuthz {
    async fn check(
        &self,
        subject: &Subject,
        relation: Relation,
        _o: &FgaObject,
        _c: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(self
            .grants
            .lock()
            .unwrap()
            .contains(&(subject.as_str().to_string(), relation)))
    }
    async fn write_tuple(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn delete_tuple(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn read_tuples(
        &self,
        _: &FgaObject,
        _: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        Ok(vec![])
    }
    async fn list_objects(
        &self,
        _: &Subject,
        _: Relation,
        _: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
    async fn delete_object_tuples(&self, _: &FgaObject) -> Result<u32, AuthzError> {
        Ok(0)
    }
    async fn read_subject_objects(
        &self,
        _: &Subject,
        _: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
}

/// バイト保持の in-memory ObjectStore。
#[derive(Default)]
pub struct MemStore {
    objects: Mutex<std::collections::HashMap<String, Vec<u8>>>,
}
#[async_trait]
impl storage::object_store::ObjectStore for MemStore {
    async fn ensure_bucket(&self) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn presign_get_internal(
        &self,
        _: &str,
        _: Duration,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://mem".into())
    }
    async fn presign_put(
        &self,
        _: &str,
        _: Duration,
        _: i64,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://mem".into())
    }
    async fn presign_get(
        &self,
        _: &str,
        _: Duration,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://mem".into())
    }
    async fn read_and_hash(&self, _: &str) -> Result<(String, u64), storage::ObjectStoreError> {
        Err(storage::ObjectStoreError::NotFound("mem".into()))
    }
    async fn put_object(
        &self,
        key: &str,
        bytes: Vec<u8>,
        _: &str,
    ) -> Result<(), storage::ObjectStoreError> {
        self.objects.lock().unwrap().insert(key.into(), bytes);
        Ok(())
    }
    async fn get_object(&self, key: &str) -> Result<Vec<u8>, storage::ObjectStoreError> {
        self.objects
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .ok_or_else(|| storage::ObjectStoreError::NotFound(key.into()))
    }
    async fn exists(&self, key: &str) -> Result<bool, storage::ObjectStoreError> {
        Ok(self.objects.lock().unwrap().contains_key(key))
    }
    async fn copy(&self, _: &str, _: &str) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn delete(&self, _: &str) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn list_prefix(
        &self,
        _: &str,
        _: Option<&str>,
    ) -> Result<(Vec<String>, Option<String>), storage::ObjectStoreError> {
        Ok((vec![], None))
    }
    async fn delete_batch(&self, _: &[String]) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
}

pub fn ctx_for(user: &str, tenant: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: user.into(),
            email: None,
            groups: vec!["/acme".into()],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        tenant.into(),
    )
}

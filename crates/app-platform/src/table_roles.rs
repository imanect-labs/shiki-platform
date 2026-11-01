//! アプリのテーブルを使える部署・グループ（GAP-68）。
//!
//! インストールの要求の `viewer_roles`・`editor_roles`・`submitter_roles` と同じ意味で、アプリが持つ（`app_id` に
//! 束縛した）すべてのテーブルに、部署・グループ（ロールのメンバー `role:<tenant>|<id>#member`）の viewer・editor・
//! submitter（追加だけ・D-113）を付ける。テーブルの共有（`/data/tables/{id}/shares`）はアプリのテーブルを断るので、
//! インストールの後から足す・外すのはここだけ。
//!
//! - **タプルの付け外しの 1 か所**: [`InstallService::write_table_roles`]。インストールの provision も同じ関数を通る。
//! - **変えられる人**: インストールとアンインストールができる人と同じ（アプリ〔`mini_app_code` のアーティファクト〕の
//!   owner・human 確定判断）。断ったら監査に deny で残す。
//! - **監査**: 足した・外した部署・グループを `app.table_roles.update` に残す。監査を書けなければタプルを戻して
//!   失敗にする（監査に残らない変更を作らない・テーブルの共有と同じ）。
//! - 足す部署・グループは、テーブルの共有と同じく id の形と同じテナントのディレクトリに居ることを確かめる。外すのは
//!   形だけ確かめる（残った不正なタプルも消せるように）。
//! - 付け外しの後は、テーブルごとに配信と材料化の前提を検査し直させる（`after_table_rebac_changed`）。

use authz::{AuthContext, FgaObject, Relation, Subject};
use serde::{Deserialize, Serialize};
use storage::audit::{AuditAction, AuditEntry, Decision};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{AppPlatformError, InstallService};

/// 1 回の変更で足す・外せる部署・グループの数の上限（3 つの役割の合計・タプルの書き込みを抑える）。
pub const MAX_TABLE_ROLE_CHANGES: usize = 100;

/// アプリのテーブルを使える部署・グループ（インストールの要求と同じ 3 つの役割）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AppTableRoles {
    /// 見られる部署・グループ（viewer）。
    #[serde(default)]
    pub viewer_roles: Vec<String>,
    /// 編集できる部署・グループ（editor）。
    #[serde(default)]
    pub editor_roles: Vec<String>,
    /// レコードを追加するだけの部署・グループ（submitter・D-113）。
    #[serde(default)]
    pub submitter_roles: Vec<String>,
}

impl AppTableRoles {
    /// （部署・グループ, 役割）の並び（インストールの要求と同じ対応）。
    fn entries(&self) -> impl Iterator<Item = (&str, Relation)> {
        [
            (&self.viewer_roles, Relation::Viewer),
            (&self.editor_roles, Relation::Editor),
            (&self.submitter_roles, Relation::Submitter),
        ]
        .into_iter()
        .flat_map(|(roles, rel)| roles.iter().map(move |r| (r.as_str(), rel)))
    }

    fn push(&mut self, role: String, relation: Relation) {
        let list = match relation {
            Relation::Viewer => &mut self.viewer_roles,
            Relation::Editor => &mut self.editor_roles,
            _ => &mut self.submitter_roles,
        };
        if !list.contains(&role) {
            list.push(role);
        }
    }

    fn sort(&mut self) {
        for list in [
            &mut self.viewer_roles,
            &mut self.editor_roles,
            &mut self.submitter_roles,
        ] {
            list.sort();
            list.dedup();
        }
    }

    fn is_empty(&self) -> bool {
        self.entries().next().is_none()
    }
}

/// 足す・外す部署・グループ（[`InstallService::update_table_roles`]）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct AppTableRolesChange {
    #[serde(default)]
    pub add: AppTableRoles,
    #[serde(default)]
    pub remove: AppTableRoles,
}

/// アプリのテーブル 1 つ（画面に名前を出す）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct AppTable {
    pub id: Uuid,
    pub name: String,
}

/// アプリのテーブルと、それを使える部署・グループ（どれかのテーブルに付いていれば出す）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct AppTableAccess {
    /// アプリが持つテーブル（作った順）。
    pub tables: Vec<AppTable>,
    pub roles: AppTableRoles,
}

/// [`InstallService::update_table_roles`] が 1 つのテーブルに実際に変えたもの（監査を書けなければ戻す）。
struct Applied {
    table_id: Uuid,
    changed: Vec<(String, Relation)>,
    /// 付けた（`true`）か外した（`false`）か。
    granted: bool,
}

/// 部署・グループの id の形をまとめて確かめる（インストールの要求は、テーブルを作る前にここで断る）。
pub(crate) fn check_role_ids(roles: &AppTableRoles) -> Result<(), AppPlatformError> {
    roles
        .entries()
        .try_for_each(|(role, _)| check_role_id(role))
}

/// 部署・グループの id の形（テーブルの共有の共有先と同じ文字の決まり）。
fn check_role_id(role: &str) -> Result<(), AppPlatformError> {
    if role != role.trim() {
        return Err(AppPlatformError::Invalid(
            "部署・グループの id の前後に空白は使えません".into(),
        ));
    }
    authz::validate_local_id(role).map_err(|v| {
        AppPlatformError::Invalid(format!("部署・グループの id が正しくありません: {v}"))
    })
}

impl InstallService {
    /// アプリのテーブル `object` に、部署・グループの役割のタプルを付ける（`grant`）か外す（GAP-68）。
    ///
    /// **アプリのテーブルの部署・グループのタプルを書き換えるのはこの関数だけ**（インストールの provision と、
    /// インストールの後の変更で共通）。id の形を確かめてから書く。実際に変えた（部署・グループ, 役割）を返す
    /// （冪等の書き込みで既にあった・無かったものは含めない。監査の失敗で戻すため）。途中で失敗したら、この呼び出しで
    /// 変えたものを戻してから失敗を返す（best-effort）。
    pub(crate) async fn write_table_roles(
        &self,
        ctx: &AuthContext,
        object: &FgaObject,
        roles: &AppTableRoles,
        grant: bool,
    ) -> Result<Vec<(String, Relation)>, AppPlatformError> {
        check_role_ids(roles)?;
        let ns = ctx.ns();
        let mut changed = Vec::new();
        for (role, relation) in roles.entries() {
            match self
                .set_role_tuple(&ns, role, relation, object, grant)
                .await
            {
                Ok(true) => changed.push((role.to_string(), relation)),
                Ok(false) => {}
                Err(e) => {
                    for (done, rel) in changed.iter().rev() {
                        if let Err(undo) =
                            self.set_role_tuple(&ns, done, *rel, object, !grant).await
                        {
                            tracing::error!(error = %undo, role = %done, "部署・グループのタプルを戻せません");
                        }
                    }
                    return Err(e);
                }
            }
        }
        Ok(changed)
    }

    /// 部署・グループ 1 つの役割のタプルを付ける・外す（変えたら `true`）。
    async fn set_role_tuple(
        &self,
        ns: &authz::Namespace<'_>,
        role: &str,
        relation: Relation,
        object: &FgaObject,
        grant: bool,
    ) -> Result<bool, AppPlatformError> {
        let subject = Subject::userset(&ns.role(role), Relation::Member);
        if grant {
            self.authz.write_tuple(&subject, relation, object).await
        } else {
            self.authz.delete_tuple(&subject, relation, object).await
        }
        .map_err(|e| AppPlatformError::Internal(format!("authz: {e}")))
    }

    /// アプリのテーブルと、それを使える部署・グループを返す（変えられる人だけ・GAP-68）。
    pub async fn table_roles(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
    ) -> Result<AppTableAccess, AppPlatformError> {
        self.require_artifact_owner(ctx, app_id).await?;
        self.require_installed(ctx, app_id).await?;
        self.read_table_access(ctx, app_id).await
    }

    /// アプリのテーブルを使える部署・グループを足す・外す（本文はモジュールの説明）。変えた後の状態を返す。
    pub async fn update_table_roles(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
        change: AppTableRolesChange,
        trace_id: Option<&str>,
    ) -> Result<AppTableAccess, AppPlatformError> {
        if let Err(e) = self.require_artifact_owner(ctx, app_id).await {
            if matches!(e, AppPlatformError::Forbidden) {
                self.audit_deny(ctx, app_id, AuditAction::AppTableRolesUpdate, trace_id)
                    .await;
            }
            return Err(e);
        }
        self.require_installed(ctx, app_id).await?;
        validate_change(&change)?;
        for (role, _) in change.add.entries() {
            self.data.check_role_share_target(ctx, role).await?;
        }
        let tables = self.data.tables_owned_by_app(ctx, app_id).await?;
        if tables.is_empty() {
            return Err(AppPlatformError::Invalid(
                "このアプリにはテーブルがありません".into(),
            ));
        }

        // 付け外し（途中で失敗したら、それまでに変えたものを戻す）。
        let mut applied: Vec<Applied> = Vec::new();
        let outcome = async {
            for (table_id, _) in &tables {
                let obj = ctx.ns().data_table(&table_id.to_string());
                let added = self.write_table_roles(ctx, &obj, &change.add, true).await?;
                applied.push(Applied {
                    table_id: *table_id,
                    changed: added,
                    granted: true,
                });
                let removed = self
                    .write_table_roles(ctx, &obj, &change.remove, false)
                    .await?;
                applied.push(Applied {
                    table_id: *table_id,
                    changed: removed,
                    granted: false,
                });
            }
            self.audit
                .record(
                    ctx,
                    AuditEntry {
                        action: AuditAction::AppTableRolesUpdate,
                        object_type: "miniapp",
                        object_id: &app_id.to_string(),
                        decision: Decision::Allow,
                        trace_id,
                        metadata: serde_json::json!({
                            "add": change.add,
                            "remove": change.remove,
                            "tables": tables.iter().map(|(id, _)| id).collect::<Vec<_>>(),
                        }),
                    },
                )
                .await
                .map_err(|e| AppPlatformError::Internal(format!("audit: {e}")))
        }
        .await;
        if let Err(e) = outcome {
            self.undo_table_roles(ctx, &applied).await;
            return Err(e);
        }
        for (table_id, _) in &tables {
            self.data.after_table_rebac_changed(ctx, *table_id).await;
        }
        self.read_table_access(ctx, app_id).await
    }

    /// [`Self::update_table_roles`] の途中までの変更を戻す（best-effort・戻せなければ error に残す）。
    async fn undo_table_roles(&self, ctx: &AuthContext, applied: &[Applied]) {
        for Applied {
            table_id,
            changed,
            granted,
        } in applied.iter().rev()
        {
            let mut roles = AppTableRoles::default();
            for (role, relation) in changed {
                roles.push(role.clone(), *relation);
            }
            if roles.is_empty() {
                continue;
            }
            let obj = ctx.ns().data_table(&table_id.to_string());
            if let Err(e) = self.write_table_roles(ctx, &obj, &roles, !granted).await {
                tracing::error!(error = %e, %table_id, "アプリのテーブルの部署・グループの変更を戻せません");
            }
            self.data.after_table_rebac_changed(ctx, *table_id).await;
        }
    }

    /// アプリが有効にインストールされているか（無ければ NotFound）。
    async fn require_installed(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
    ) -> Result<(), AppPlatformError> {
        self.installations
            .resolve_active_by_app(&ctx.tenant_id, app_id)
            .await
            .map_err(crate::install::map_gateway)?
            .map(|_| ())
            .ok_or(AppPlatformError::NotFound)
    }

    /// アプリのテーブルと、どれかのテーブルに付いている部署・グループの役割を読む。
    async fn read_table_access(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
    ) -> Result<AppTableAccess, AppPlatformError> {
        let tables = self.data.tables_owned_by_app(ctx, app_id).await?;
        let ns = ctx.ns();
        let mut roles = AppTableRoles::default();
        for (table_id, _) in &tables {
            let tuples = self
                .authz
                .read_tuples(&ns.data_table(&table_id.to_string()), None)
                .await
                .map_err(|e| AppPlatformError::Internal(format!("authz: {e}")))?;
            for t in tuples {
                let relation = Relation::parse(&t.relation).filter(|r| {
                    matches!(r, Relation::Viewer | Relation::Editor | Relation::Submitter)
                });
                if let (Some(relation), Some(role)) =
                    (relation, ns.parse_role_member_subject(&t.user))
                {
                    roles.push(role.to_string(), relation);
                }
            }
        }
        roles.sort();
        Ok(AppTableAccess {
            tables: tables
                .into_iter()
                .map(|(id, name)| AppTable { id, name })
                .collect(),
            roles,
        })
    }
}

/// 変更の形を確かめる（件数の上限・同じ役割で足して外す食い違い・id の形）。
fn validate_change(change: &AppTableRolesChange) -> Result<(), AppPlatformError> {
    let count = change.add.entries().count() + change.remove.entries().count();
    if count == 0 {
        return Err(AppPlatformError::Invalid(
            "足す部署・グループも外す部署・グループもありません".into(),
        ));
    }
    if count > MAX_TABLE_ROLE_CHANGES {
        return Err(AppPlatformError::Invalid(format!(
            "1 回に変えられる部署・グループは最大 {MAX_TABLE_ROLE_CHANGES} 件です"
        )));
    }
    for (role, relation) in change.add.entries() {
        check_role_id(role)?;
        if change
            .remove
            .entries()
            .any(|(r, rel)| r == role && rel == relation)
        {
            return Err(AppPlatformError::Invalid(
                "同じ部署・グループを足して外すことはできません".into(),
            ));
        }
    }
    for (role, _) in change.remove.entries() {
        check_role_id(role)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::{Arc, Mutex};

    use authz::{AuthzClient, AuthzError, Consistency, ObjectType, ReadTupleKey};

    use super::*;

    /// 書き込みと削除を記録し、部署 `boom` への書き込みだけ失敗する認可。
    #[derive(Default)]
    struct FailsOnBoom(Mutex<Vec<String>>);

    #[async_trait::async_trait]
    impl AuthzClient for FailsOnBoom {
        async fn check(
            &self,
            _: &Subject,
            _: Relation,
            _: &FgaObject,
            _: Consistency,
        ) -> Result<bool, AuthzError> {
            Ok(true)
        }
        async fn write_tuple(
            &self,
            s: &Subject,
            r: Relation,
            _: &FgaObject,
        ) -> Result<bool, AuthzError> {
            if s.as_str().contains("|boom#") {
                return Err(AuthzError::Unexpected {
                    status: 500,
                    body: "boom".into(),
                });
            }
            self.0
                .lock()
                .unwrap()
                .push(format!("write {} {}", s.as_str(), r.as_str()));
            Ok(true)
        }
        async fn delete_tuple(
            &self,
            s: &Subject,
            r: Relation,
            _: &FgaObject,
        ) -> Result<bool, AuthzError> {
            self.0
                .lock()
                .unwrap()
                .push(format!("delete {} {}", s.as_str(), r.as_str()));
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

    struct NoRefs;

    #[async_trait::async_trait]
    impl data::RefResolver for NoRefs {
        async fn user_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
            Ok(false)
        }
        async fn role_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
            Ok(false)
        }
        async fn file_readable(&self, _: &AuthContext, _: Uuid) -> Result<bool, String> {
            Ok(false)
        }
    }

    /// 途中で失敗したら、その呼び出しで付けたものを戻してから失敗を返す（DB には触らない）。
    #[tokio::test]
    async fn a_failed_write_undoes_what_the_call_already_wrote() {
        let db = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://localhost/none")
            .unwrap();
        let fga = Arc::new(FailsOnBoom::default());
        let authz: Arc<dyn AuthzClient> = fga.clone();
        let artifacts = Arc::new(artifact::ArtifactStore::new(db.clone(), authz.clone()));
        let code = Arc::new(crate::MiniAppCodeStore::new(
            artifacts,
            crate::Registry::new(db.clone()),
        ));
        let data = Arc::new(data::DataStore::new(
            db.clone(),
            authz.clone(),
            Arc::new(NoRefs),
        ));
        let installs = InstallService::new(
            db.clone(),
            crate::Registry::new(db),
            code,
            data,
            authz,
            None,
            vec![],
        );
        let ctx = AuthContext::new(
            authz::Principal {
                kind: authz::PrincipalKind::User,
                id: "owner".into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some("t".into()),
            },
            "acme".into(),
            "t".into(),
        );
        let obj = ctx.ns().data_table("tbl");
        let out = installs
            .write_table_roles(&ctx, &obj, &roles(&["sales", "boom"], &[], &[]), true)
            .await;
        assert!(matches!(out, Err(AppPlatformError::Internal(_))), "{out:?}");
        assert_eq!(
            *fga.0.lock().unwrap(),
            [
                "write role:t|sales#member viewer",
                "delete role:t|sales#member viewer"
            ]
        );
    }

    fn roles(viewer: &[&str], editor: &[&str], submitter: &[&str]) -> AppTableRoles {
        let v = |x: &[&str]| x.iter().map(|s| (*s).to_string()).collect();
        AppTableRoles {
            viewer_roles: v(viewer),
            editor_roles: v(editor),
            submitter_roles: v(submitter),
        }
    }

    #[test]
    fn entries_follow_the_install_request_meaning() {
        let r = roles(&["sales"], &["acct"], &["staff"]);
        let got: Vec<_> = r.entries().collect();
        assert_eq!(
            got,
            [
                ("sales", Relation::Viewer),
                ("acct", Relation::Editor),
                ("staff", Relation::Submitter)
            ]
        );
    }

    #[test]
    fn change_is_checked_before_any_tuple_is_written() {
        let ok = AppTableRolesChange {
            add: roles(&["sales"], &[], &[]),
            remove: roles(&[], &["sales"], &[]),
        };
        assert!(validate_change(&ok).is_ok(), "役割が違えば足して外せる");
        let empty = AppTableRolesChange::default();
        assert!(matches!(
            validate_change(&empty),
            Err(AppPlatformError::Invalid(_))
        ));
        let both = AppTableRolesChange {
            add: roles(&["sales"], &[], &[]),
            remove: roles(&["sales"], &[], &[]),
        };
        assert!(matches!(
            validate_change(&both),
            Err(AppPlatformError::Invalid(m)) if m.contains("足して外す")
        ));
        for bad in ["a#b", "a:b", "a|b", " sales", ""] {
            let change = AppTableRolesChange {
                add: AppTableRoles::default(),
                remove: roles(&[bad], &[], &[]),
            };
            assert!(validate_change(&change).is_err(), "{bad:?}");
        }
        let many: Vec<String> = (0..=MAX_TABLE_ROLE_CHANGES)
            .map(|i| format!("r{i}"))
            .collect();
        let too_many = AppTableRolesChange {
            add: AppTableRoles {
                viewer_roles: many,
                ..AppTableRoles::default()
            },
            remove: AppTableRoles::default(),
        };
        assert!(matches!(
            validate_change(&too_many),
            Err(AppPlatformError::Invalid(m)) if m.contains("最大 100 件")
        ));
    }
}

//! 共有相手（user / role）の表示用プロフィールの一括解決（#741）。
//!
//! 共有一覧 API（ノード・アーティファクト・スレッド）は OpenFGA タプルから `{type, id}` しか
//! 得られないため、`directory_user` / `directory_role` から表示名（user は email も）を引いて
//! 同じ応答に載せる。
//!
//! 設計上の不変条件:
//! - **テナント（＋ org）スコープ**: 呼び出し元 [`AuthContext`] の `tenant_id` と `org` で必ず絞る。
//!   別テナント（同 org 文字列でも）の名前・email は返さない＝該当なしは `None`。
//! - **N+1 の禁止**: 一覧の user / role id を重複除去して `= ANY($ids)` の **1 クエリ**
//!   （`UNION ALL`）で引く。必要な列（id・表示名・email）だけを射影する。

use std::collections::{BTreeSet, HashMap};

use authz::AuthContext;
use sqlx::PgPool;

use crate::{error::StorageError, model::ShareTarget};

/// 共有相手 1 件の表示用プロフィール。ディレクトリに無ければ両方 `None`
/// （UI は短縮 ID 等でフォールバック表示する）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrincipalProfile {
    pub display_name: Option<String>,
    /// user のみ（role には email が無いので常に `None`）。
    pub email: Option<String>,
}

/// 共有相手群の解決結果（[`resolve_share_profiles`] の戻り値）。
#[derive(Debug, Default)]
pub struct ShareProfiles {
    users: HashMap<String, (String, String)>,
    roles: HashMap<String, String>,
}

impl ShareProfiles {
    /// 共有相手のプロフィールを返す（未解決なら空）。
    pub fn get(&self, target: &ShareTarget) -> PrincipalProfile {
        match target {
            ShareTarget::User { id } => match self.users.get(id) {
                Some((name, email)) => PrincipalProfile {
                    display_name: Some(name.clone()),
                    email: Some(email.clone()),
                },
                None => PrincipalProfile::default(),
            },
            ShareTarget::Role { id } => PrincipalProfile {
                display_name: self.roles.get(id).cloned(),
                email: None,
            },
        }
    }
}

#[derive(sqlx::FromRow)]
struct ProfileRow {
    kind: String,
    id: String,
    display_name: String,
    email: Option<String>,
}

/// 共有相手群の表示名（user は email も）をテナント（＋ org）スコープで一括解決する。
///
/// 相手が 0 件ならクエリを撃たない。DB 失敗は呼び出し側で非致命に扱える（名前は表示補助）。
pub(crate) async fn resolve_share_profiles<'a>(
    db: &PgPool,
    ctx: &AuthContext,
    targets: impl IntoIterator<Item = &'a ShareTarget>,
) -> Result<ShareProfiles, StorageError> {
    // 同じ相手が viewer/editor の両方で現れても 1 回だけ引く（重複除去）。
    let mut user_ids = BTreeSet::new();
    let mut role_ids = BTreeSet::new();
    for t in targets {
        match t {
            ShareTarget::User { id } => user_ids.insert(id.as_str()),
            ShareTarget::Role { id } => role_ids.insert(id.as_str()),
        };
    }
    if user_ids.is_empty() && role_ids.is_empty() {
        return Ok(ShareProfiles::default());
    }
    let user_ids: Vec<&str> = user_ids.into_iter().collect();
    let role_ids: Vec<&str> = role_ids.into_iter().collect();
    // 両表とも PK (tenant_id, <id>) で引ける。org も隔離境界なので同一 org に限る（#371 と同方針）。
    let rows: Vec<ProfileRow> = sqlx::query_as(
        "SELECT 'user'::text AS kind, user_id AS id, display_name, email \
         FROM directory_user \
         WHERE tenant_id = $1 AND org = $2 AND user_id = ANY($3) \
         UNION ALL \
         SELECT 'role'::text AS kind, role_id AS id, display_name, NULL::text AS email \
         FROM directory_role \
         WHERE tenant_id = $1 AND org = $2 AND role_id = ANY($4)",
    )
    .bind(&ctx.tenant_id)
    .bind(&ctx.org)
    .bind(&user_ids)
    .bind(&role_ids)
    .fetch_all(db)
    .await
    .map_err(StorageError::Db)?;
    Ok(collect_profiles(rows))
}

fn collect_profiles(rows: Vec<ProfileRow>) -> ShareProfiles {
    let mut out = ShareProfiles::default();
    for r in rows {
        match (r.kind.as_str(), r.email) {
            ("user", Some(email)) => {
                out.users.insert(r.id, (r.display_name, email));
            }
            ("role", _) => {
                out.roles.insert(r.id, r.display_name);
            }
            // directory_user.email は NOT NULL なので user で email 無しは来ない（来ても載せない）。
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kind: &str, id: &str, name: &str, email: Option<&str>) -> ProfileRow {
        ProfileRow {
            kind: kind.into(),
            id: id.into(),
            display_name: name.into(),
            email: email.map(Into::into),
        }
    }

    #[test]
    fn profiles_resolve_by_kind_and_fall_back_to_none() {
        let p = collect_profiles(vec![
            row("user", "u1", "Alice", Some("alice@a.example")),
            row("role", "eng", "Engineering", None),
            // user と role で同じ id でも種別で区別される。
            row("role", "u1", "Role U1", None),
        ]);
        assert_eq!(
            p.get(&ShareTarget::User { id: "u1".into() }),
            PrincipalProfile {
                display_name: Some("Alice".into()),
                email: Some("alice@a.example".into()),
            }
        );
        assert_eq!(
            p.get(&ShareTarget::Role { id: "eng".into() }),
            PrincipalProfile {
                display_name: Some("Engineering".into()),
                email: None,
            }
        );
        assert_eq!(
            p.get(&ShareTarget::Role { id: "u1".into() }).display_name,
            Some("Role U1".into())
        );
        // 未解決（別テナント・未登録）は両方 None。
        assert_eq!(
            p.get(&ShareTarget::User { id: "eng".into() }),
            PrincipalProfile::default()
        );
        assert_eq!(
            p.get(&ShareTarget::Role {
                id: "missing".into()
            }),
            PrincipalProfile::default()
        );
    }
}

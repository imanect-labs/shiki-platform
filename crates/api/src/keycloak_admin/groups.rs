//! Keycloak の group ツリー操作（テナント修飾 group `/tenants/<tenant>/<org>`・#811）。
//!
//! 名前で辿る（`group-by-path` は使わない）。AD 由来の部署名は空白や記号を含み得るため、
//! パス文字列を組み立てて URL に埋めるより、親 id ＋ 子の名前で 1 段ずつ解決する方が壊れにくい。
//! 作成・メンバー追加/削除・削除はすべて冪等（既存/不在を成功に倒す）。

use authz::org_group::TENANTS_GROUP;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{ensure_ok, status_error, KcUser, KeycloakAdmin, KeycloakAdminError};

/// 一覧系 API のページサイズ。
const PAGE: u32 = 100;

/// Keycloak 上の group（必要フィールドのみ）。
#[derive(Debug, Clone, Deserialize)]
pub struct KcGroup {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub path: String,
    /// 子 group の数（一覧 API が返す。辿る必要があるかの判断に使う）。
    #[serde(default, rename = "subGroupCount")]
    pub sub_group_count: Option<u64>,
    #[serde(default)]
    pub attributes: std::collections::HashMap<String, Vec<String>>,
    #[serde(default, rename = "realmRoles")]
    pub realm_roles: Vec<String>,
    #[serde(default, rename = "clientRoles")]
    pub client_roles: std::collections::HashMap<String, Vec<String>>,
}

impl KcGroup {
    /// この group に直接付いた role 割り当て（realm ＋ client）の件数。
    pub fn role_mapping_count(&self) -> usize {
        self.realm_roles.len() + self.client_roles.values().map(Vec::len).sum::<usize>()
    }
}

/// 旧 group パスを参照している realm 設定（[`KeycloakAdmin::group_path_references`]）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupPathReferences {
    /// 参照している設定（`<種別> <名前>: <パス>`）。
    pub found: Vec<String>,
    /// 調べられなかった種別（権限不足など・手で確認する）。
    pub unchecked: Vec<String>,
}

/// `path` が `bases` のいずれか、またはその配下か（セグメント単位）。
fn references_any(path: &str, bases: &[String]) -> bool {
    bases.iter().any(|base| {
        path == base
            || path
                .strip_prefix(base.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// `/tenants/<from>` のリネーム結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameOutcome {
    /// リネームした。
    Renamed,
    /// `/tenants/<from>` が無い（未移行・リネーム済み）。
    Missing,
    /// `/tenants/<to>` が既にある（統合は手作業）。
    TargetExists,
}

impl KeycloakAdmin<'_> {
    /// 親（`None` はトップレベル）直下の名前一致 group を引く（不在は `None`）。
    pub async fn find_child_group(
        &self,
        parent_id: Option<&str>,
        name: &str,
    ) -> Result<Option<KcGroup>, KeycloakAdminError> {
        let token = self.admin_token().await?;
        let url = match parent_id {
            Some(pid) => format!("{}/groups/{pid}/children", self.base),
            None => format!("{}/groups", self.base),
        };
        // トップレベル検索は「子孫に一致を含む親」も返すため、名前で直下だけに絞る。
        let resp = self
            .http
            .get(url)
            .bearer_auth(&token)
            .query(&[
                ("search", name),
                ("exact", "true"),
                ("briefRepresentation", "false"),
            ])
            .send()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        let groups: Vec<KcGroup> = ensure_ok(resp)
            .await?
            .json()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        Ok(groups.into_iter().find(|g| g.name == name))
    }

    /// 親直下に `name` の group を冪等に用意し、その id を返す。
    pub async fn ensure_child_group(
        &self,
        parent_id: Option<&str>,
        name: &str,
    ) -> Result<String, KeycloakAdminError> {
        if let Some(existing) = self.find_child_group(parent_id, name).await? {
            return Ok(existing.id);
        }
        let token = self.admin_token().await?;
        let url = match parent_id {
            Some(pid) => format!("{}/groups/{pid}/children", self.base),
            None => format!("{}/groups", self.base),
        };
        let resp = self
            .http
            .post(url)
            .bearer_auth(&token)
            .json(&json!({ "name": name }))
            .send()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        // 409（並行作成で既存）は成功に倒し、検索で id を引く。
        if !resp.status().is_success() && resp.status().as_u16() != 409 {
            return Err(status_error(resp).await);
        }
        self.find_child_group(parent_id, name)
            .await?
            .map(|g| g.id)
            .ok_or_else(|| KeycloakAdminError::Status {
                status: 404,
                body: format!("group '{name}' が作成後も見つかりません"),
            })
    }

    /// パスのセグメント列（`["tenants", "acme", "acme"]`）を辿って group を引く（不在は `None`）。
    pub async fn find_group_path(
        &self,
        segments: &[&str],
    ) -> Result<Option<KcGroup>, KeycloakAdminError> {
        let mut current: Option<KcGroup> = None;
        for seg in segments {
            let parent = current.as_ref().map(|g| g.id.as_str());
            match self.find_child_group(parent, seg).await? {
                Some(g) => current = Some(g),
                None => return Ok(None),
            }
        }
        Ok(current)
    }

    /// パスのセグメント列を上から冪等に作り、末端の id を返す。
    pub async fn ensure_group_path(&self, segments: &[&str]) -> Result<String, KeycloakAdminError> {
        let mut parent: Option<String> = None;
        for seg in segments {
            parent = Some(self.ensure_child_group(parent.as_deref(), seg).await?);
        }
        parent.ok_or_else(|| KeycloakAdminError::Status {
            status: 400,
            body: "空の group パスは作れません".into(),
        })
    }

    /// テナントの org group（`/tenants/<tenant>/<org>`）を冪等に用意し、その id を返す。
    pub async fn ensure_org_group(
        &self,
        tenant_id: &str,
        org: &str,
    ) -> Result<String, KeycloakAdminError> {
        self.ensure_group_path(&[TENANTS_GROUP, tenant_id, org])
            .await
    }

    /// テナントの group subtree（`/tenants/<tenant>` 以下すべて）を削除する（不在は成功）。
    ///
    /// group はテナントで修飾されているので、他テナントの group を巻き込まない
    /// （同じ org slug を使う他テナントが居ても `/tenants/<他>/<org>` は別物）。
    pub async fn delete_tenant_groups(&self, tenant_id: &str) -> Result<bool, KeycloakAdminError> {
        match self.find_group_path(&[TENANTS_GROUP, tenant_id]).await? {
            Some(root) => {
                self.delete_group(&root.id).await?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// `/tenants/<from>` を `/tenants/<to>` にリネームする（retenant の IdP 追従）。
    ///
    /// メンバーシップは group id に付いているので、リネームだけで配下の org/部署ごと
    /// 新しいパスで claim に載る。`<to>` が既にあれば統合せず [`RenameOutcome::TargetExists`]。
    pub async fn rename_tenant_group(
        &self,
        from: &str,
        to: &str,
    ) -> Result<RenameOutcome, KeycloakAdminError> {
        let Some(root) = self.find_group_path(&[TENANTS_GROUP, from]).await? else {
            return Ok(RenameOutcome::Missing);
        };
        if self.find_group_path(&[TENANTS_GROUP, to]).await?.is_some() {
            return Ok(RenameOutcome::TargetExists);
        }
        let token = self.admin_token().await?;
        let url = format!("{}/groups/{}", self.base, root.id);
        // PUT は representation の置換なので、GET した全体の name だけ差し替えて書き戻す
        // （属性・role 割り当てを落とさない）。
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        let mut representation: Value = ensure_ok(resp)
            .await?
            .json()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        let obj = representation
            .as_object_mut()
            .ok_or_else(|| KeycloakAdminError::Status {
                status: 500,
                body: "group representation が想定外の形式です".into(),
            })?;
        obj.insert("name".into(), Value::String(to.to_string()));
        let resp = self
            .http
            .put(&url)
            .bearer_auth(&token)
            .json(&representation)
            .send()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        match resp.status().as_u16() {
            s if (200..300).contains(&s) => Ok(RenameOutcome::Renamed),
            // 確認と PUT の間に `<to>` が作られた。
            409 => Ok(RenameOutcome::TargetExists),
            _ => Err(status_error(resp).await),
        }
    }

    /// トップレベル group を全件列挙する（role 割り当て・属性込み）。
    pub async fn list_top_level_groups(&self) -> Result<Vec<KcGroup>, KeycloakAdminError> {
        self.paged(
            &format!("{}/groups", self.base),
            &[("briefRepresentation", "false")],
        )
        .await
    }

    /// 直下の子 group を全件列挙する（role 割り当て・属性込み）。
    pub async fn list_child_groups(
        &self,
        group_id: &str,
    ) -> Result<Vec<KcGroup>, KeycloakAdminError> {
        self.paged(
            &format!("{}/groups/{group_id}/children", self.base),
            &[("briefRepresentation", "false")],
        )
        .await
    }

    /// group の直接メンバーを全件列挙する（`tenant` 属性込み）。
    pub async fn group_members(&self, group_id: &str) -> Result<Vec<KcUser>, KeycloakAdminError> {
        self.paged(
            &format!("{}/groups/{group_id}/members", self.base),
            &[("briefRepresentation", "false")],
        )
        .await
    }

    /// ユーザーを group へ加える（既に所属していても成功）。
    pub async fn add_user_to_group(
        &self,
        user_id: &str,
        group_id: &str,
    ) -> Result<(), KeycloakAdminError> {
        let token = self.admin_token().await?;
        let resp = self
            .http
            .put(format!("{}/users/{user_id}/groups/{group_id}", self.base))
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        ensure_ok(resp).await.map(|_| ())
    }

    /// ユーザーを group から外す（所属していない・不在でも成功）。
    pub async fn remove_user_from_group(
        &self,
        user_id: &str,
        group_id: &str,
    ) -> Result<(), KeycloakAdminError> {
        let token = self.admin_token().await?;
        let resp = self
            .http
            .delete(format!("{}/users/{user_id}/groups/{group_id}", self.base))
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        if resp.status().is_success() || resp.status().as_u16() == 404 {
            return Ok(());
        }
        Err(status_error(resp).await)
    }

    /// group を削除する（子 group ごと消える・不在 404 は成功）。
    pub async fn delete_group(&self, group_id: &str) -> Result<(), KeycloakAdminError> {
        let token = self.admin_token().await?;
        let resp = self
            .http
            .delete(format!("{}/groups/{group_id}", self.base))
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        if resp.status().is_success() || resp.status().as_u16() == 404 {
            return Ok(());
        }
        Err(status_error(resp).await)
    }

    /// group パスを参照している realm 設定を列挙する（移行後に手で付け替える対象の報告用）。
    ///
    /// IdP マッパー（`config.group`）・LDAP group マッパー（`groups.path`）・既定 group を、種別ごとに
    /// 独立に調べる。`paths` のいずれか、またはその配下を指すものを `found` に、権限不足などで
    /// 調べられなかった種別を `unchecked` に入れる（1 種別の失敗で他の報告を落とさない）。
    pub async fn group_path_references(&self, paths: &[String]) -> GroupPathReferences {
        let mut refs = GroupPathReferences::default();
        let sections = [
            ("IdP マッパー", self.idp_mapper_groups().await),
            ("LDAP マッパー", self.ldap_mapper_groups().await),
            ("既定 group", self.default_group_paths().await),
        ];
        for (kind, result) in sections {
            match result {
                Ok(entries) => refs.found.extend(
                    entries
                        .into_iter()
                        .filter(|(_, group)| references_any(group, paths))
                        .map(|(name, group)| format!("{kind} {name}: {group}")),
                ),
                Err(e) => refs.unchecked.push(format!("{kind}（{e}）")),
            }
        }
        refs
    }

    /// IdP マッパーの（`<alias>/<name>`, `config.group`）。
    async fn idp_mapper_groups(&self) -> Result<Vec<(String, String)>, KeycloakAdminError> {
        let idps: Vec<Value> = self
            .get_json(&format!("{}/identity-provider/instances", self.base))
            .await?;
        let mut out = Vec::new();
        for alias in idps
            .iter()
            .filter_map(|i| i.get("alias").and_then(Value::as_str))
        {
            let mappers: Vec<Value> = self
                .get_json(&format!(
                    "{}/identity-provider/instances/{alias}/mappers",
                    self.base
                ))
                .await?;
            for m in &mappers {
                if let Some(group) = m.pointer("/config/group").and_then(Value::as_str) {
                    let name = m.get("name").and_then(Value::as_str).unwrap_or("?");
                    out.push((format!("{alias}/{name}"), group.to_string()));
                }
            }
        }
        Ok(out)
    }

    /// LDAP group マッパーの（名前, `groups.path`）。
    async fn ldap_mapper_groups(&self) -> Result<Vec<(String, String)>, KeycloakAdminError> {
        let mappers: Vec<Value> = self
            .get_json(&format!(
                "{}/components?type=org.keycloak.storage.ldap.mappers.LDAPStorageMapper",
                self.base
            ))
            .await?;
        let mut out = Vec::new();
        for m in &mappers {
            let name = m.get("name").and_then(Value::as_str).unwrap_or("?");
            let values = m
                .pointer("/config/groups.path")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str);
            out.extend(values.map(|g| (name.to_string(), g.to_string())));
        }
        Ok(out)
    }

    /// 既定 group の（名前, パス）。
    async fn default_group_paths(&self) -> Result<Vec<(String, String)>, KeycloakAdminError> {
        let groups: Vec<KcGroup> = self
            .get_json(&format!("{}/default-groups", self.base))
            .await?;
        Ok(groups.into_iter().map(|g| (g.name, g.path)).collect())
    }

    /// GET して JSON を返す。
    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
    ) -> Result<T, KeycloakAdminError> {
        let token = self.admin_token().await?;
        let resp = self
            .http
            .get(url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        ensure_ok(resp)
            .await?
            .json()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))
    }

    /// `first`/`max` でページングして全件集める。
    async fn paged<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        extra: &[(&str, &str)],
    ) -> Result<Vec<T>, KeycloakAdminError> {
        let token = self.admin_token().await?;
        let mut out = Vec::new();
        let mut first: u32 = 0;
        loop {
            let (first_s, max_s) = (first.to_string(), PAGE.to_string());
            let mut query: Vec<(&str, &str)> = vec![("first", &first_s), ("max", &max_s)];
            query.extend_from_slice(extra);
            let resp = self
                .http
                .get(url)
                .bearer_auth(&token)
                .query(&query)
                .send()
                .await
                .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
            let page: Vec<T> = ensure_ok(resp)
                .await?
                .json()
                .await
                .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
            let n = page.len();
            out.extend(page);
            if (n as u32) < PAGE {
                return Ok(out);
            }
            first += PAGE;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::references_any;

    #[test]
    fn references_match_by_segment() {
        let bases = vec!["/acme".to_string()];
        assert!(references_any("/acme", &bases));
        assert!(references_any("/acme/eng", &bases));
        // 前方一致では拾わない（`/acme2` は `/acme` の配下ではない）。
        assert!(!references_any("/acme2", &bases));
        assert!(!references_any("/tenants/t/acme", &bases));
    }
}

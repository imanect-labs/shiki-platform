//! StorageService: 共有リンク（#342）— broad タプルの reconcile（参照カウントの中核）。
//!
//! FGA の broad タプル集合を「**active な全リンクの (subject,relation) 和集合**」の射影として
//! 合わせる。複数リンクが同一タプルを共有しても、active な要求が 1 本でも残っていればタプルは
//! 残る（参照カウント）。owner CRUD（[`super::share_link`]）・失効（[`super::share_link_expiry`]）が呼ぶ。

#[allow(clippy::wildcard_imports)]
use super::*;

use std::collections::HashSet;

use super::share_link_util::broad_subject;
use crate::model::GeneralAccessLevel;

/// reconcile 用の active リンク 1 行。
#[derive(sqlx::FromRow)]
struct ActiveLink {
    audience: String,
    role: String,
    password_hash: Option<String>,
}

impl StorageService {
    /// broad タプル集合を「active な全リンクの (subject,relation) 和集合」に合わせる（#342 の中核）。
    ///
    /// desired からは **password 付きリンク**（broad タプルを張らない）と **restricted**（付与ゼロ）を
    /// 除外する。current は FGA 上の general-access 由来 broad タプル（`user:*` / `organization#member`）
    /// **だけ**を射影し、`user:X`・`role#member` の直接共有は絶対に触らない。add は不足権限側で benign、
    /// del は失敗時 `?` 伝播で tx 未コミット（＝再試行に委ねる／fail-open 防止）。付与したタプルは
    /// コミット失敗時の補償用に返す。
    ///
    /// active 判定の基準時刻 `now` は呼び出し側が渡す（owner 操作は `Utc::now()`、イベント駆動
    /// タイマは論理時刻を渡す＝失効の決定性・テスト容易性）。
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn reconcile_broad(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        ns: &Namespace<'_>,
        obj: &FgaObject,
        node_id: Uuid,
        tenant_id: &str,
        org: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<(Subject, Relation)>, StorageError> {
        let links: Vec<ActiveLink> = sqlx::query_as(
            "SELECT audience, role, password_hash FROM node_share_link \
             WHERE node_id = $1 AND tenant_id = $2 \
               AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > $3) \
             FOR UPDATE",
        )
        .bind(node_id)
        .bind(tenant_id)
        .bind(now)
        .fetch_all(&mut **tx)
        .await?;

        // desired: active・非パスワード・broad なリンクの (subject, relation) 和集合。
        let mut desired: Vec<(Subject, Relation)> = Vec::new();
        let mut desired_keys: HashSet<(String, Relation)> = HashSet::new();
        for l in &links {
            if l.password_hash.is_some() {
                continue; // パスワード付きは broad タプルを張らない（redeem 経由）。
            }
            let (Some(level), Some(role)) = (
                GeneralAccessLevel::parse(&l.audience),
                ShareRole::parse(&l.role),
            ) else {
                continue; // 破損行は無視（reconcile で消し込みも足しもしない）。
            };
            if let Some(subject) = broad_subject(ns, level, org) {
                let rel = role.relation();
                if desired_keys.insert((subject.as_str().to_string(), rel)) {
                    desired.push((subject, rel));
                }
            }
        }

        // current: FGA 上の general-access 由来 broad タプルだけを射影（直接共有は載せない）。
        let public = Subject::public();
        let org_member = ns.organization_member(org);
        let tuples = self.authz.read_tuples(obj, None).await?;
        let mut current: HashSet<(String, Relation)> = HashSet::new();
        for t in &tuples {
            let Some(rel) = Relation::parse(&t.relation) else {
                continue;
            };
            if !matches!(rel, Relation::Viewer | Relation::Editor) {
                continue;
            }
            if t.user == public.as_str() || t.user == org_member.as_str() {
                current.insert((t.user.clone(), rel));
            }
        }

        // add（desired − current）を先に付与。補償用に控える。
        let mut added: Vec<(Subject, Relation)> = Vec::new();
        for (subject, rel) in desired {
            if current.contains(&(subject.as_str().to_string(), rel)) {
                continue;
            }
            match self.authz.write_tuple(&subject, rel, obj).await {
                Ok(_) => added.push((subject, rel)),
                Err(e) => {
                    self.compensate_broad(obj, &added).await;
                    return Err(e.into());
                }
            }
        }
        // del（current − desired）を剥奪。失敗は補償＋Err（tx 未コミット→再試行）。
        for (sub_str, rel) in current {
            if desired_keys.contains(&(sub_str.clone(), rel)) {
                continue;
            }
            // current は public / org#member のみ→ subject を安全に再構成できる。
            let subject = if sub_str == public.as_str() {
                Subject::public()
            } else {
                ns.organization_member(org)
            };
            if let Err(e) = self.authz.delete_tuple(&subject, rel, obj).await {
                self.compensate_broad(obj, &added).await;
                return Err(e.into());
            }
        }
        Ok(added)
    }

    /// reconcile 中に付与した broad タプルを best-effort で剥奪する（コミット失敗時の補償）。
    pub(super) async fn compensate_broad(&self, obj: &FgaObject, added: &[(Subject, Relation)]) {
        for (subject, rel) in added {
            let _ = self.authz.delete_tuple(subject, *rel, obj).await;
        }
    }
}

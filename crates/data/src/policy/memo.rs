//! リクエストスコープの認可メモ（Task 15.9・data-platform §16.4・PIT-18）。
//!
//! グリッド 1 画面の読取（ページ＋件数＋lookup / rollup の解決）は、素朴に書くと同じ問い
//! （テーブル viewer・ロール・個別共有集合）を OpenFGA へ何度も投げる。1 リクエストの処理中に
//! 権限が変わる前提を置く必要はないので、**同一リクエスト内に限って**答えを覚える。
//!
//! # 生存範囲は型で閉じる（リクエストを跨がない）
//!
//! - [`AuthzMemo<'r>`] は `DataStore` が持つ authz クライアントを**借用**して作る
//!   （[`crate::store::DataStore::authz_memo`]）。`'static` ではないので `DataStore`（`Clone + 'static`）・
//!   静的領域・`Arc` で持ち回る長命のタスクへは置けない。`Clone` も持たない。型名は crate 外へ出ない。
//! - `DataStore` の公開メソッドは入口で新しいメモを作り、戻るときに捨てる。**プロセス内・リクエスト
//!   跨ぎの保持は無い**（権限の剥奪は次のリクエストで必ず OpenFGA から引き直される）。
//!
//! # キー（緩めると PIT-18 を破る）
//!
//! - 主体に依存する問い（check / list_objects / read_subject_objects / 材料）は、[`PrincipalKey`]
//!   （`PrincipalKind`・principal id・`org`・`tenant_id`）を**漏れなく**含める。
//! - 材料（[`crate::policy::material`]）は加えて**解決対象の式集合**と「共有集合を引くか」をキーにする
//!   （式が違えば要る材料も違う。部分的な材料を別の式へ流用しない）。
//! - 主体に依存しない問い（object の直接タプル `read_tuples`）は tenant 名前空間化済みの object 識別子と
//!   relation がキー。
//! - check はすべて `HigherConsistency`（data の既存の check と同じ・剥奪の即時反映）。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use authz::{
    AuthContext, AuthzClient, CheckTuple, Consistency, FgaObject, ObjectType, PrincipalKind,
    ReadTupleKey, Relation,
};

use crate::policy::ast::PolicyExpr;
use crate::policy::material::PolicyMaterial;
use crate::DataError;

/// 認可の問いの主体側のキー（`AuthContext` の関連部分を漏れなく）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PrincipalKey {
    kind: &'static str,
    id: String,
    org: String,
    tenant_id: String,
}

impl PrincipalKey {
    pub(crate) fn of(ctx: &AuthContext) -> Self {
        let kind = match ctx.principal.kind {
            PrincipalKind::User => "user",
            PrincipalKind::Workflow => "workflow",
            PrincipalKind::MiniApp => "miniapp",
        };
        PrincipalKey {
            kind,
            id: ctx.principal.id.clone(),
            org: ctx.org.clone(),
            tenant_id: ctx.tenant_id.clone(),
        }
    }
}

/// 材料のキー（主体＋式集合＋共有集合の要否）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct MaterialKey {
    who: PrincipalKey,
    /// 式の正規形（`Debug` 表現・文字列は引用符つきで曖昧さが無い）を整列・重複除去した集合。
    exprs: Vec<String>,
    shares: bool,
}

impl MaterialKey {
    pub(crate) fn new(ctx: &AuthContext, exprs: &[&PolicyExpr], shares: bool) -> Self {
        let mut v: Vec<String> = exprs.iter().map(|e| format!("{e:?}")).collect();
        v.sort_unstable();
        v.dedup();
        MaterialKey {
            who: PrincipalKey::of(ctx),
            exprs: v,
            shares,
        }
    }
}

/// 主体の集合問い合わせ（list_objects / 直接タプル）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ListKey {
    Objects(Relation, ObjectType),
    Direct(ObjectType),
}

/// object の直接タプルの問い（object・relation 指定）。
type TupleKey = (FgaObject, Option<Relation>);

#[derive(Default)]
struct State {
    checks: HashMap<(PrincipalKey, Relation, FgaObject), bool>,
    lists: HashMap<(PrincipalKey, ListKey), Arc<Vec<String>>>,
    tuples: HashMap<TupleKey, Arc<Vec<ReadTupleKey>>>,
    materials: HashMap<MaterialKey, Arc<PolicyMaterial>>,
}

/// リクエストスコープの認可メモ（[モジュール文書](self)）。
pub(crate) struct AuthzMemo<'r> {
    authz: &'r dyn AuthzClient,
    state: Mutex<State>,
}

#[allow(clippy::needless_pass_by_value)] // map_err へそのまま渡すため。
fn authz_err(e: authz::AuthzError) -> DataError {
    DataError::Internal(format!("authz: {e}"))
}

impl<'r> AuthzMemo<'r> {
    pub(crate) fn new(authz: &'r dyn AuthzClient) -> Self {
        AuthzMemo {
            authz,
            state: Mutex::new(State::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // 保持中に await しない・panic で中断した更新は単なる未記録なので、毒は無視してよい。
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `relation` を持つか（1 問）。覚えていなければ OpenFGA へ 1 往復。
    pub(crate) async fn check(
        &self,
        ctx: &AuthContext,
        relation: Relation,
        object: &FgaObject,
    ) -> Result<bool, DataError> {
        let got = self.check_all(ctx, &[(relation, object.clone())]).await?;
        got.first()
            .copied()
            .ok_or_else(|| DataError::Internal("authz: check の応答が空です".into()))
    }

    /// 複数の check。覚えていない問いだけを **BatchCheck 1 往復**（50 問ごと）で引く。結果は入力順。
    pub(crate) async fn check_all(
        &self,
        ctx: &AuthContext,
        items: &[(Relation, FgaObject)],
    ) -> Result<Vec<bool>, DataError> {
        let who = PrincipalKey::of(ctx);
        let missing: Vec<(Relation, FgaObject)> = {
            let st = self.lock();
            let mut seen = HashSet::new();
            items
                .iter()
                .filter(|(r, o)| !st.checks.contains_key(&(who.clone(), *r, o.clone())))
                .filter(|it| seen.insert((*it).clone()))
                .cloned()
                .collect()
        };
        if !missing.is_empty() {
            let subject = ctx.subject();
            let got = if let [(relation, object)] = missing.as_slice() {
                vec![self
                    .authz
                    .check(&subject, *relation, object, Consistency::HigherConsistency)
                    .await
                    .map_err(authz_err)?]
            } else {
                let tuples: Vec<CheckTuple> = missing
                    .iter()
                    .map(|(relation, object)| CheckTuple {
                        subject: subject.clone(),
                        relation: *relation,
                        object: object.clone(),
                    })
                    .collect();
                self.authz
                    .batch_check(&tuples, Consistency::HigherConsistency)
                    .await
                    .map_err(authz_err)?
            };
            if got.len() != missing.len() {
                return Err(DataError::Internal(
                    "authz: batch_check の応答数が問い数と一致しません".into(),
                ));
            }
            let mut st = self.lock();
            for ((relation, object), allowed) in missing.into_iter().zip(got) {
                st.checks.insert((who.clone(), relation, object), allowed);
            }
        }
        let st = self.lock();
        items
            .iter()
            .map(|(r, o)| {
                st.checks
                    .get(&(who.clone(), *r, o.clone()))
                    .copied()
                    .ok_or_else(|| DataError::Internal("authz: check の答えがありません".into()))
            })
            .collect()
    }

    async fn list(&self, ctx: &AuthContext, key: ListKey) -> Result<Arc<Vec<String>>, DataError> {
        let who = PrincipalKey::of(ctx);
        if let Some(v) = self.lock().lists.get(&(who.clone(), key)) {
            return Ok(Arc::clone(v));
        }
        let subject = ctx.subject();
        let raw = match key {
            ListKey::Objects(relation, ty) => self.authz.list_objects(&subject, relation, ty).await,
            ListKey::Direct(ty) => self.authz.read_subject_objects(&subject, ty).await,
        }
        .map_err(authz_err)?;
        let v = Arc::new(raw);
        self.lock().lists.insert((who, key), Arc::clone(&v));
        Ok(v)
    }

    /// `list_objects`（継承展開済みの実効集合）。
    pub(crate) async fn list_objects(
        &self,
        ctx: &AuthContext,
        relation: Relation,
        object_type: ObjectType,
    ) -> Result<Arc<Vec<String>>, DataError> {
        self.list(ctx, ListKey::Objects(relation, object_type))
            .await
    }

    /// 主体の直接タプルを持つ object 一覧（継承を展開しない）。
    pub(crate) async fn read_subject_objects(
        &self,
        ctx: &AuthContext,
        object_type: ObjectType,
    ) -> Result<Arc<Vec<String>>, DataError> {
        self.list(ctx, ListKey::Direct(object_type)).await
    }

    /// object の直接タプル（主体に依存しない）。
    pub(crate) async fn read_tuples(
        &self,
        object: &FgaObject,
        relation: Option<Relation>,
    ) -> Result<Arc<Vec<ReadTupleKey>>, DataError> {
        let key = (object.clone(), relation);
        if let Some(v) = self.lock().tuples.get(&key) {
            return Ok(Arc::clone(v));
        }
        let v = Arc::new(
            self.authz
                .read_tuples(object, relation)
                .await
                .map_err(authz_err)?,
        );
        self.lock().tuples.insert(key, Arc::clone(&v));
        Ok(v)
    }

    pub(crate) fn material(&self, key: &MaterialKey) -> Option<Arc<PolicyMaterial>> {
        self.lock().materials.get(key).map(Arc::clone)
    }

    pub(crate) fn put_material(&self, key: MaterialKey, m: Arc<PolicyMaterial>) {
        self.lock().materials.insert(key, m);
    }
}

#[cfg(test)]
#[path = "memo_tests.rs"]
mod tests;

//! 一覧・パンくずの認可 post-filter（PIT-13）を BatchCheck にまとめる。
//!
//! 子ごと・祖先ごとに `check` すると、OpenFGA への往復と `authz.check` span が行数ぶん増える。
//! ここでは判定を BatchCheck に寄せつつ、**判定に要らない行の失敗で結果全体を落とさない**よう、
//! 失敗時は必要な行だけで判定し直す。判定は単発と同じく強整合（即時剥奪の反映）・fail-closed。

use authz::{AuthContext, AuthzClient, CheckTuple, Consistency, Relation};
use uuid::Uuid;

use super::{node_fga_object, NodeKind, NodeRow};
use crate::error::StorageError;

/// 1 回の BatchCheck で判定する最小の行数。OpenFGA は 50 問を 1 往復で判定するため、
/// ページの残り枠が少なくてもこの数まではまとめて判定する（拒否行ごとに往復と span が増えない）。
const MIN_CHUNK: usize = authz::fga_http::MAX_CHECKS_PER_BATCH;

/// `targets`（ノード種別, ID）の `relation` を 1 回の `batch_check` で判定する。結果は同じ順。
async fn batch_decisions(
    authz: &dyn AuthzClient,
    ctx: &AuthContext,
    relation: Relation,
    targets: &[(NodeKind, Uuid)],
) -> Result<Vec<bool>, StorageError> {
    let subject = ctx.subject();
    let ns = ctx.ns();
    let items: Vec<CheckTuple> = targets
        .iter()
        .map(|(kind, id)| CheckTuple {
            subject: subject.clone(),
            relation,
            object: node_fga_object(&ns, *kind, *id),
        })
        .collect();
    let allowed = authz
        .batch_check(&items, Consistency::HigherConsistency)
        .await?;
    if allowed.len() != targets.len() {
        return Err(StorageError::Integrity(
            "batch_check の応答数が問い数と一致しません".into(),
        ));
    }
    Ok(allowed)
}

fn row_target(row: &NodeRow) -> (NodeKind, Uuid) {
    (NodeKind::parse(&row.kind).unwrap_or(NodeKind::File), row.id)
}

/// 一覧の post-filter: `rows` の**先頭から** `relation` を判定し、判定結果（先頭 k 行ぶん・k ≥ 1）を返す。
///
/// `need` はページの残り枠。残り枠が小さくても [`MIN_CHUNK`] 行まではまとめて判定する。ページに
/// 入らないかもしれない行まで含めた判定が失敗したら、ページに入り得る `need` 行だけで判定し直す
/// （不要な行の判定失敗で一覧全体を落とさない）。`need` 行の判定が失敗したらエラー（fail-closed）。
pub(super) async fn post_filter_front(
    authz: &dyn AuthzClient,
    ctx: &AuthContext,
    relation: Relation,
    rows: &[NodeRow],
    need: usize,
) -> Result<Vec<bool>, StorageError> {
    let need = need.clamp(1, rows.len().max(1)).min(rows.len());
    let wide = rows.len().min(need.max(MIN_CHUNK));
    let targets: Vec<(NodeKind, Uuid)> = rows[..wide].iter().map(row_target).collect();
    match batch_decisions(authz, ctx, relation, &targets).await {
        Ok(decisions) => Ok(decisions),
        Err(_) if wide > need => {
            tracing::warn!("ページ外の行を含む一括判定に失敗。ページに入り得る行だけで判定し直す");
            batch_decisions(authz, ctx, relation, &targets[..need]).await
        }
        Err(e) => Err(e),
    }
}

/// パンくずの祖先（自身を除く・自身に近い順）の viewer を判定する。
///
/// 祖先は 1 回の BatchCheck でまとめて判定する。失敗したら変更前と同じく自身に近い順に 1 件ずつ
/// 判定し、最初に読めない祖先で打ち切る（それより上の祖先の判定失敗でパンくず全体を落とさない）。
/// 返り値は `ancestors` と同じ長さで、打ち切った以降は `false`。
pub(super) async fn ancestor_decisions(
    authz: &dyn AuthzClient,
    ctx: &AuthContext,
    ancestors: &[(NodeKind, Uuid)],
) -> Result<Vec<bool>, StorageError> {
    if ancestors.is_empty() {
        return Ok(Vec::new());
    }
    if let Ok(decisions) = batch_decisions(authz, ctx, Relation::Viewer, ancestors).await {
        return Ok(decisions);
    }
    tracing::warn!("祖先の一括判定に失敗。自身に近い順の 1 件ずつの判定に切り替える");
    let mut out = vec![false; ancestors.len()];
    for (i, (kind, id)) in ancestors.iter().enumerate() {
        let allowed = authz
            .check(
                &ctx.subject(),
                Relation::Viewer,
                &node_fga_object(&ctx.ns(), *kind, *id),
                Consistency::HigherConsistency,
            )
            .await?;
        if !allowed {
            break;
        }
        out[i] = true;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::sync::Mutex;

    use async_trait::async_trait;
    use authz::{AuthzError, FgaObject, ObjectType, Principal, ReadTupleKey, Subject, TenantId};
    use chrono::Utc;

    use super::*;

    /// 問い数が `fail_over` を超える一括判定を失敗させ、呼ばれた問い数を記録するテストダブル。
    /// `deny` に入った ID は拒否、`fail_on` の ID は単発 check が失敗する（それ以外は許可）。
    struct Fake {
        fail_over: usize,
        fail_batch: bool,
        fail_on: Option<String>,
        calls: Mutex<Vec<usize>>,
        single: Mutex<usize>,
        deny: Vec<String>,
    }

    impl Fake {
        fn new(fail_over: usize) -> Self {
            Fake {
                fail_over,
                fail_batch: false,
                fail_on: None,
                calls: Mutex::new(Vec::new()),
                single: Mutex::new(0),
                deny: Vec::new(),
            }
        }
        fn allowed(&self, object: &FgaObject) -> bool {
            !self
                .deny
                .iter()
                .any(|d| object.as_str().ends_with(d.as_str()))
        }
    }

    fn err() -> AuthzError {
        AuthzError::Unexpected {
            status: 500,
            body: String::new(),
        }
    }

    #[async_trait]
    impl AuthzClient for Fake {
        async fn check(
            &self,
            _subject: &Subject,
            _relation: Relation,
            object: &FgaObject,
            _consistency: Consistency,
        ) -> Result<bool, AuthzError> {
            *self.single.lock().unwrap() += 1;
            if let Some(f) = &self.fail_on {
                if object.as_str().ends_with(f.as_str()) {
                    return Err(err());
                }
            }
            Ok(self.allowed(object))
        }
        async fn batch_check(
            &self,
            items: &[CheckTuple],
            _consistency: Consistency,
        ) -> Result<Vec<bool>, AuthzError> {
            self.calls.lock().unwrap().push(items.len());
            if self.fail_batch || items.len() > self.fail_over {
                return Err(err());
            }
            Ok(items.iter().map(|i| self.allowed(&i.object)).collect())
        }
        async fn write_tuple(
            &self,
            _: &Subject,
            _: Relation,
            _: &FgaObject,
        ) -> Result<bool, AuthzError> {
            unreachable!()
        }
        async fn delete_tuple(
            &self,
            _: &Subject,
            _: Relation,
            _: &FgaObject,
        ) -> Result<bool, AuthzError> {
            unreachable!()
        }
        async fn read_tuples(
            &self,
            _: &FgaObject,
            _: Option<Relation>,
        ) -> Result<Vec<ReadTupleKey>, AuthzError> {
            unreachable!()
        }
        async fn list_objects(
            &self,
            _: &Subject,
            _: Relation,
            _: ObjectType,
        ) -> Result<Vec<String>, AuthzError> {
            unreachable!()
        }
        async fn delete_object_tuples(&self, _: &FgaObject) -> Result<u32, AuthzError> {
            unreachable!()
        }
        async fn read_subject_objects(
            &self,
            _: &Subject,
            _: ObjectType,
        ) -> Result<Vec<String>, AuthzError> {
            unreachable!()
        }
    }

    fn ctx() -> AuthContext {
        AuthContext::new(
            Principal {
                kind: authz::PrincipalKind::User,
                id: "u".into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: None,
            },
            "org".into(),
            TenantId::parse("default").unwrap(),
        )
    }

    fn rows(n: usize) -> Vec<NodeRow> {
        (0..n)
            .map(|_| NodeRow {
                id: Uuid::new_v4(),
                org: "org".into(),
                tenant_id: "default".into(),
                kind: "file".into(),
                name: "n".into(),
                parent_id: None,
                blob_sha256: None,
                size_bytes: None,
                content_type: None,
                version: 1,
                deleted_at: None,
                created_by: "u".into(),
                updated_by: "u".into(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            })
            .collect()
    }

    #[tokio::test]
    async fn small_remaining_still_checks_one_round_trip_worth() {
        // 残り枠 1 でも 1 行ずつにせず、最大 50 行を 1 回で判定する（拒否行ごとに span を増やさない）。
        let fake = Fake::new(usize::MAX);
        let rows = rows(80);
        let d = post_filter_front(&fake, &ctx(), Relation::Viewer, &rows, 1)
            .await
            .unwrap();
        assert_eq!(d.len(), MIN_CHUNK);
        assert_eq!(*fake.calls.lock().unwrap(), vec![MIN_CHUNK]);
    }

    #[tokio::test]
    async fn large_remaining_checks_only_rows_that_fit() {
        let fake = Fake::new(usize::MAX);
        let rows = rows(100);
        let d = post_filter_front(&fake, &ctx(), Relation::Viewer, &rows, 70)
            .await
            .unwrap();
        assert_eq!(d.len(), 70);
    }

    #[tokio::test]
    async fn wide_failure_retries_with_rows_that_fit() {
        // ページ外を含む判定が失敗しても、ページに入り得る行だけで判定し直して一覧を返す。
        let fake = Fake::new(3);
        let rows = rows(10);
        let d = post_filter_front(&fake, &ctx(), Relation::Viewer, &rows, 2)
            .await
            .unwrap();
        assert_eq!(d, vec![true, true]);
        assert_eq!(*fake.calls.lock().unwrap(), vec![10, 2]);
    }

    #[tokio::test]
    async fn failure_of_needed_rows_is_an_error() {
        // ページに入り得る行の判定が失敗したら fail-closed でエラー。
        let fake = Fake::new(1);
        let rows = rows(10);
        assert!(post_filter_front(&fake, &ctx(), Relation::Viewer, &rows, 5)
            .await
            .is_err());
        let fake = Fake::new(0);
        assert!(
            post_filter_front(&fake, &ctx(), Relation::Viewer, &rows[..1], 1)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn mismatched_answer_count_is_an_error() {
        struct Short(Fake);
        #[async_trait]
        impl AuthzClient for Short {
            async fn check(
                &self,
                s: &Subject,
                r: Relation,
                o: &FgaObject,
                c: Consistency,
            ) -> Result<bool, AuthzError> {
                self.0.check(s, r, o, c).await
            }
            async fn batch_check(
                &self,
                _items: &[CheckTuple],
                _c: Consistency,
            ) -> Result<Vec<bool>, AuthzError> {
                Ok(vec![true])
            }
            async fn write_tuple(
                &self,
                s: &Subject,
                r: Relation,
                o: &FgaObject,
            ) -> Result<bool, AuthzError> {
                self.0.write_tuple(s, r, o).await
            }
            async fn delete_tuple(
                &self,
                s: &Subject,
                r: Relation,
                o: &FgaObject,
            ) -> Result<bool, AuthzError> {
                self.0.delete_tuple(s, r, o).await
            }
            async fn read_tuples(
                &self,
                o: &FgaObject,
                r: Option<Relation>,
            ) -> Result<Vec<ReadTupleKey>, AuthzError> {
                self.0.read_tuples(o, r).await
            }
            async fn list_objects(
                &self,
                s: &Subject,
                r: Relation,
                t: ObjectType,
            ) -> Result<Vec<String>, AuthzError> {
                self.0.list_objects(s, r, t).await
            }
            async fn delete_object_tuples(&self, o: &FgaObject) -> Result<u32, AuthzError> {
                self.0.delete_object_tuples(o).await
            }
            async fn read_subject_objects(
                &self,
                s: &Subject,
                t: ObjectType,
            ) -> Result<Vec<String>, AuthzError> {
                self.0.read_subject_objects(s, t).await
            }
        }
        let rows = rows(3);
        assert!(matches!(
            post_filter_front(
                &Short(Fake::new(usize::MAX)),
                &ctx(),
                Relation::Viewer,
                &rows,
                3
            )
            .await,
            Err(StorageError::Integrity(_))
        ));
    }

    fn ancestors(n: usize) -> Vec<(NodeKind, Uuid)> {
        (0..n).map(|_| (NodeKind::Folder, Uuid::new_v4())).collect()
    }

    #[tokio::test]
    async fn ancestors_are_checked_in_one_batch() {
        let fake = Fake::new(usize::MAX);
        let a = ancestors(4);
        let d = ancestor_decisions(&fake, &ctx(), &a).await.unwrap();
        assert_eq!(d, vec![true; 4]);
        assert_eq!(*fake.calls.lock().unwrap(), vec![4]);
        assert_eq!(*fake.single.lock().unwrap(), 0);
        assert!(ancestor_decisions(&fake, &ctx(), &[])
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn ancestor_batch_failure_falls_back_and_stops_at_first_denied() {
        // 一括判定が失敗しても、自身に近い順に判定して読めない祖先で打ち切る。
        // その上の祖先（判定すると失敗する）は判定しない。
        let a = ancestors(4);
        let mut fake = Fake::new(usize::MAX);
        fake.fail_batch = true;
        fake.deny = vec![a[1].1.to_string()];
        fake.fail_on = Some(a[2].1.to_string());
        let d = ancestor_decisions(&fake, &ctx(), &a).await.unwrap();
        assert_eq!(d, vec![true, false, false, false]);
        assert_eq!(*fake.single.lock().unwrap(), 2);
        // 必要な祖先の判定が失敗したらエラー（fail-closed）。
        let mut fake = Fake::new(usize::MAX);
        fake.fail_batch = true;
        fake.fail_on = Some(a[0].1.to_string());
        assert!(ancestor_decisions(&fake, &ctx(), &a).await.is_err());
    }
}

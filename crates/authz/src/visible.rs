//! 一覧を OpenFGA の ListObjects の上限で黙って切らないための部品（BUG-149・PIT-1 と同じ考え方）。
//!
//! ListObjects は応答の件数に上限がある（OpenFGA の `OPENFGA_LIST_OBJECTS_MAX_RESULTS`・既定 1,000）。
//! 上限に届いた応答は「見られるもの全部」ではないので、そのまま一覧の正に使うと、上限の外のものが
//! 一覧にも検索にも出ない（漏洩ではなく取りこぼし）。上限に届いたら、DB の候補（テナント・組織で絞った
//! もの）を並びの順にページで引き、[`AuthzClient::batch_check`] で絞る（二段の認可は DB の候補が pre-filter、
//! OpenFGA の判定が post-filter）。

use crate::client::{AuthzClient, CheckTuple, Consistency};
use crate::error::AuthzError;
use crate::fga_http::MAX_CHECKS_PER_BATCH;
use crate::object::FgaObject;
use crate::subject::Subject;
use crate::vocab::{ObjectType, Relation};

/// ListObjects の応答の上限（OpenFGA の `OPENFGA_LIST_OBJECTS_MAX_RESULTS` の既定）。
///
/// 応答がこの件数に届いたら、上限で切られたものとして扱う（[`list_may_be_cut`]）。配備で上限を
/// **下げる**ときは、この値も下げること（上げるのは安全側で、上限の内でも DB の候補から引き直すだけ）。
pub const LIST_OBJECTS_MAX_RESULTS: usize = 1_000;

/// ListObjects の応答が上限で切られたかもしれないか（`n` は応答の件数）。
///
/// 上限より少ない応答は全部とみなす。ただし OpenFGA は応答の期限（`OPENFGA_LIST_OBJECTS_DEADLINE`・
/// 既定 3 秒）を過ぎると、そこまでに見つけた分を誤りにせず返すので、期限切れの取りこぼしはここでは
/// 見分けられない。
#[must_use]
pub const fn list_may_be_cut(n: usize) -> bool {
    n >= LIST_OBJECTS_MAX_RESULTS
}

/// [`holds_any`] が同時に送る BatchCheck の本数（1 本は 50 問）。
///
/// OpenFGA の BatchCheck は 1 本ずつだと 50 問に 100 ミリ秒ほどかかる（v1.8・手元の計測。単発の check は 1 問
/// 2 ミリ秒ほど）。4 本ずつ並べると 200 問がおよそ 1 往復の時間で済む。並べすぎると OpenFGA を占有するので絞る。
pub const BATCH_CHECKS_IN_FLIGHT: usize = 4;

/// `objects` のそれぞれについて、`subject` が `relations` のどれかを持つか（入力順）。
///
/// 問いは 50 問ずつの [`AuthzClient::batch_check`] にまとめ、[`BATCH_CHECKS_IN_FLIGHT`] 本ずつ並べて送る。
/// 1 問でも判定に失敗したら全体を誤りにする（fail-closed）。
pub async fn holds_any(
    authz: &dyn AuthzClient,
    subject: &Subject,
    relations: &[Relation],
    objects: &[FgaObject],
    consistency: Consistency,
) -> Result<Vec<bool>, AuthzError> {
    if objects.is_empty() || relations.is_empty() {
        return Ok(vec![false; objects.len()]);
    }
    let items: Vec<CheckTuple> = objects
        .iter()
        .flat_map(|object| {
            relations.iter().map(move |relation| CheckTuple {
                subject: subject.clone(),
                relation: *relation,
                object: object.clone(),
            })
        })
        .collect();
    let mut got: Vec<bool> = Vec::with_capacity(items.len());
    // 50 問ずつの BatchCheck を BATCH_CHECKS_IN_FLIGHT 本ずつまとめて送り、入力順に並べ直す。
    for group in items.chunks(MAX_CHECKS_PER_BATCH * BATCH_CHECKS_IN_FLIGHT) {
        let calls: Vec<_> = group
            .chunks(MAX_CHECKS_PER_BATCH)
            .map(|chunk| authz.batch_check(chunk, consistency))
            .collect();
        for part in futures::future::try_join_all(calls).await? {
            got.extend(part);
        }
    }
    if got.len() != items.len() {
        return Err(AuthzError::Unexpected {
            status: 200,
            body: format!(
                "batch_check の応答数 {} が問い数 {} と一致しません",
                got.len(),
                items.len()
            ),
        });
    }
    Ok(got
        .chunks(relations.len())
        .map(|held| held.iter().any(|h| *h))
        .collect())
}

/// `subjects` のそれぞれが**直接タプル**を持つ `object_type` のオブジェクト（`type:id`・入力順）。
///
/// [`AuthzClient::read_subject_objects`]（Read API・ページを最後までたどる）を [`BATCH_CHECKS_IN_FLIGHT`] 本ずつ
/// 並べて読む。ListObjects と違って応答の件数に上限が無いので、上限に届いた実効集合を直接タプルから組み直すのに
/// 使う（継承は展開しないので、userset の subject は呼び出し側が展開して渡す）。1 本でも読めなければ全体を誤りにする。
pub async fn read_direct_objects(
    authz: &dyn AuthzClient,
    subjects: &[Subject],
    object_type: ObjectType,
) -> Result<Vec<Vec<String>>, AuthzError> {
    let mut out = Vec::with_capacity(subjects.len());
    for group in subjects.chunks(BATCH_CHECKS_IN_FLIGHT) {
        let calls: Vec<_> = group
            .iter()
            .map(|s| authz.read_subject_objects(s, object_type))
            .collect();
        out.extend(futures::future::try_join_all(calls).await?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::ReadTupleKey;
    use crate::object::Namespace;
    use crate::vocab::ObjectType;
    use async_trait::async_trait;
    use std::sync::Mutex;

    /// 「名前に `ok` を含む object の viewer」だけを持つ判定（問いの並びも覚える）。
    #[derive(Default)]
    struct Fake {
        asked: Mutex<Vec<(Relation, String)>>,
        short: bool,
    }

    #[async_trait]
    impl AuthzClient for Fake {
        async fn check(
            &self,
            _: &Subject,
            relation: Relation,
            object: &FgaObject,
            _: Consistency,
        ) -> Result<bool, AuthzError> {
            self.asked
                .lock()
                .unwrap()
                .push((relation, object.as_str().to_string()));
            Ok(relation == Relation::Submitter && object.as_str().contains("ok"))
        }
        async fn batch_check(
            &self,
            items: &[CheckTuple],
            c: Consistency,
        ) -> Result<Vec<bool>, AuthzError> {
            let mut out = Vec::new();
            for i in items {
                out.push(self.check(&i.subject, i.relation, &i.object, c).await?);
            }
            if self.short {
                out.pop();
            }
            Ok(out)
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
            subject: &Subject,
            ty: ObjectType,
        ) -> Result<Vec<String>, AuthzError> {
            if subject.as_str().contains("broken") {
                return Err(AuthzError::Unexpected {
                    status: 500,
                    body: "read".into(),
                });
            }
            Ok(vec![format!("{}:of-{}", ty.as_str(), subject.as_str())])
        }
    }

    #[test]
    fn the_cap_itself_counts_as_cut() {
        assert!(!list_may_be_cut(0));
        assert!(!list_may_be_cut(LIST_OBJECTS_MAX_RESULTS - 1));
        assert!(list_may_be_cut(LIST_OBJECTS_MAX_RESULTS));
        assert!(list_may_be_cut(LIST_OBJECTS_MAX_RESULTS + 1));
    }

    #[tokio::test]
    async fn any_relation_admits_and_order_is_kept() {
        let fake = Fake::default();
        let ns = Namespace::for_tenant("t1");
        let subject = ns.user("bob");
        let objects = [
            ns.data_table("a-ok"),
            ns.data_table("b-no"),
            ns.data_table("c-ok"),
        ];
        let got = holds_any(
            &fake,
            &subject,
            &[Relation::Viewer, Relation::Submitter],
            &objects,
            Consistency::HigherConsistency,
        )
        .await
        .unwrap();
        assert_eq!(got, vec![true, false, true]);
        // 1 object につき relation の数だけ問う（object ごとにまとめた並び）。
        let asked = fake.asked.lock().unwrap().clone();
        assert_eq!(asked.len(), 6);
        assert_eq!(asked[0].0, Relation::Viewer);
        assert_eq!(asked[1].0, Relation::Submitter);
        assert!(asked[1].1.ends_with("a-ok"));
        // 問うものが無ければ OpenFGA を呼ばない。
        let none = holds_any(
            &fake,
            &subject,
            &[Relation::Viewer],
            &[],
            Consistency::HigherConsistency,
        )
        .await
        .unwrap();
        assert!(none.is_empty());
        assert_eq!(fake.asked.lock().unwrap().len(), 6);
    }

    #[tokio::test]
    async fn direct_objects_keep_subject_order_and_fail_closed() {
        let fake = Fake::default();
        let ns = Namespace::for_tenant("t1");
        let subjects: Vec<Subject> = (0..(BATCH_CHECKS_IN_FLIGHT * 2 + 1))
            .map(|i| ns.user(&format!("u{i}")))
            .collect();
        let got = read_direct_objects(&fake, &subjects, ObjectType::DataRecord)
            .await
            .unwrap();
        assert_eq!(got.len(), subjects.len());
        for (s, objs) in subjects.iter().zip(&got) {
            assert_eq!(objs, &vec![format!("data_record:of-{}", s.as_str())]);
        }
        assert!(read_direct_objects(&fake, &[], ObjectType::DataRecord)
            .await
            .unwrap()
            .is_empty());
        // 1 本でも読めなければ全体を誤りにする（読めた分だけで集合を作らない）。
        let mut with_broken = subjects.clone();
        with_broken.insert(3, ns.user("broken"));
        assert!(
            read_direct_objects(&fake, &with_broken, ObjectType::DataRecord)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn short_answer_fails_closed() {
        let fake = Fake {
            short: true,
            ..Fake::default()
        };
        let ns = Namespace::for_tenant("t1");
        let err = holds_any(
            &fake,
            &ns.user("bob"),
            &[Relation::Viewer],
            &[ns.data_table("a-ok")],
            Consistency::HigherConsistency,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AuthzError::Unexpected { .. }), "{err:?}");
    }
}

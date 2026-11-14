//! まとめて登録の同じレコードを探す列の取込（[`super`]）の検証（DB 不要）。
//!
//! data の一括書込の代わりに、探す列の一意を持つ小さな表（[`Fake`]）を使う。取込の直前に別の実行の追加を差し込み、
//! 断られた追加の行が更新として取り込み直されること、本当の誤りの行と見えない行の値は誤りのまま残ること、件数と
//! 誤りの行の番号が 1 回の取込と同じ形で合うことを確かめる。実 Postgres・実 OpenFGA での同時の 2 本は api の
//! `workflow_data_upsert_it` が確かめる。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value
)]

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use data::{BulkUpsertRow, CondOp, Condition, DataQuery};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::*;
use crate::nodes::ports::{
    AgentInvokeReq, AgentToolGateway, CsvPatchReq, CsvWriteReq, HttpSendReq, HttpSendResp,
    LlmInvokeReq, ResolvedSecretView, ResolvedSkillView, StorageListReq, StorageWriteReq,
};

const TABLE: Uuid = Uuid::from_u128(0x5d1e_0b7a_93c4_4c1e_9f20_6a1b_2c3d_4e5f);
const KEY: &str = "wf:t1:r:save";
/// 探す列（重複を許さない）。
const NO: &str = "no";

#[derive(Clone)]
struct Row {
    id: Uuid,
    data: Map<String, Value>,
    /// 実行主体に見えない行。
    hidden: bool,
}

/// 取込の直前に 1 回だけ起こすこと（別の実行の追加）。
type Interleave = Box<dyn FnOnce(&mut Vec<Row>) + Send>;

#[derive(Default)]
struct State {
    rows: Vec<Row>,
    interleave: Vec<Interleave>,
    /// 同時の追加と、検証で重なる（不合格）か適用で重なる（拒否）か。
    race_in_validation: bool,
    /// 取込の冪等キーと受け取った行（呼ばれた順）。
    imports: Vec<(String, Vec<BulkUpsertRow>)>,
    /// 同じ冪等キーの取込は同じ結果（data の取込 ID と同じ）。
    done: HashMap<String, Value>,
}

#[derive(Default)]
struct Fake {
    s: Mutex<State>,
}

impl Fake {
    fn insert(&self, data: Value, hidden: bool) -> Uuid {
        let id = Uuid::new_v4();
        self.s.lock().unwrap().rows.push(Row {
            id,
            data: data.as_object().unwrap().clone(),
            hidden,
        });
        id
    }

    /// 取込の直前に、別の実行が `no` の値で行を追加する（見える行）。
    fn added_meanwhile(&self, values: &[&'static str]) {
        let values: Vec<&'static str> = values.to_vec();
        self.s
            .lock()
            .unwrap()
            .interleave
            .push(Box::new(move |rows| {
                for v in values {
                    rows.push(Row {
                        id: Uuid::new_v4(),
                        data: json!({ NO: v, "by": "other" }).as_object().unwrap().clone(),
                        hidden: false,
                    });
                }
            }));
    }

    fn values_of(&self, no: &str) -> Vec<Map<String, Value>> {
        let s = self.s.lock().unwrap();
        s.rows
            .iter()
            .filter(|r| r.data.get(NO) == Some(&json!(no)))
            .map(|r| r.data.clone())
            .collect()
    }

    fn import_keys(&self) -> Vec<String> {
        let s = self.s.lock().unwrap();
        s.imports.iter().map(|(k, _)| k.clone()).collect()
    }
}

/// data の一括書込の簡単な写し: `bad` の行は検証で不合格、探す列の重なりは検証（`race_in_validation`）か適用で
/// 断る。`abort_on_invalid` で不合格があれば何も書かない。
fn bulk(s: &mut State, rows: &[BulkUpsertRow], abort_on_invalid: bool) -> Value {
    let dup = |rows: &[Row], data: &Value| {
        let v = data.get(NO);
        rows.iter().any(|r| v.is_some() && r.data.get(NO) == v)
    };
    let mut invalid = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        if r.data.get("bad").is_some() {
            invalid.push((i, "「数」には数を入れてください"));
        } else if r.record_id.is_none() && s.race_in_validation && dup(&s.rows, &r.data) {
            invalid.push((
                i,
                "「番号」の値は既に他のレコードで使われています（重複できない列）",
            ));
        }
    }
    let total = rows.len();
    let err = |(i, m): &(usize, &str)| json!({ "row": i + 1, "key": null, "message": m });
    if abort_on_invalid && !invalid.is_empty() {
        return json!({ "import_id": Uuid::new_v4(), "status": "failed", "rows_total": total,
                       "rows_applied": 0, "rows_invalid": invalid.len(), "rows_rejected": 0,
                       "errors": invalid.iter().map(err).collect::<Vec<_>>(),
                       "last_error": "検証で不合格の行があるため、何も適用しませんでした" });
    }
    let mut rejected = Vec::new();
    let mut applied = 0;
    for (i, r) in rows.iter().enumerate() {
        if invalid.iter().any(|(j, _)| *j == i) {
            continue;
        }
        match r.record_id {
            Some(id) => match s.rows.iter_mut().find(|x| x.id == id && !x.hidden) {
                Some(x) => {
                    for (k, v) in r.data.as_object().unwrap() {
                        x.data.insert(k.clone(), v.clone());
                    }
                    applied += 1;
                }
                None => rejected.push((i, "対象が見つかりません")),
            },
            None if dup(&s.rows, &r.data) => {
                rejected.push((
                    i,
                    "「番号」の値は既に他のレコードで使われています（重複できない列）",
                ));
            }
            None => {
                s.rows.push(Row {
                    id: Uuid::new_v4(),
                    data: r.data.as_object().unwrap().clone(),
                    hidden: false,
                });
                applied += 1;
            }
        }
    }
    let mut errors: Vec<(usize, &str)> = invalid
        .into_iter()
        .chain(rejected.iter().copied())
        .collect();
    errors.sort_by_key(|e| e.0);
    json!({ "import_id": Uuid::new_v4(), "status": "done", "rows_total": total,
            "rows_applied": applied, "rows_invalid": total - applied - rejected.len(),
            "rows_rejected": rejected.len(), "errors": errors.iter().map(err).collect::<Vec<_>>(),
            "last_error": null })
}

#[async_trait]
impl NodePorts for Fake {
    async fn storage_write(&self, _: &ExecCtx, _: StorageWriteReq) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn storage_read(&self, _: &ExecCtx, _: Uuid) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn storage_list(&self, _: &ExecCtx, _: StorageListReq) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn rag_search(&self, _: &ExecCtx, _: &str, _: Option<u32>) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn llm_invoke(&self, _: &ExecCtx, _: LlmInvokeReq) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn agent_invoke(
        &self,
        _: &ExecCtx,
        _: AgentInvokeReq,
        _: std::sync::Arc<dyn AgentToolGateway>,
    ) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn http_send(&self, _: &ExecCtx, _: HttpSendReq) -> Result<HttpSendResp, PortError> {
        unreachable!()
    }
    async fn resolve_secret(&self, _: &ExecCtx, _: &str) -> Result<ResolvedSecretView, PortError> {
        unreachable!()
    }
    async fn workflow_start(&self, _: &ExecCtx, _: &str, _: &Value) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn skill_resolve(
        &self,
        _: &ExecCtx,
        _: &str,
        _: &str,
    ) -> Result<ResolvedSkillView, PortError> {
        unreachable!()
    }
    async fn csv_query(
        &self,
        _: &ExecCtx,
        _: crate::nodes::ports::CsvQueryReq,
    ) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn csv_patch(&self, _: &ExecCtx, _: CsvPatchReq) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn csv_write(&self, _: &ExecCtx, _: CsvWriteReq) -> Result<Value, PortError> {
        unreachable!()
    }

    async fn data_query(&self, _: &ExecCtx, _: Uuid, q: &DataQuery) -> Result<Value, PortError> {
        let Some(Condition::Cmp(c)) = &q.filter else {
            panic!("探すのは 1 つの比べだけ: {q:?}")
        };
        let (CondOp::In, Some(Value::Array(wanted))) = (c.op, &c.value) else {
            panic!("まとめて登録は in で探す: {c:?}")
        };
        let s = self.s.lock().unwrap();
        let items: Vec<Value> = s
            .rows
            .iter()
            .filter(|r| !r.hidden && r.data.get(&c.field).is_some_and(|v| wanted.contains(v)))
            .map(|r| json!({ "id": r.id, "data": r.data }))
            .collect();
        Ok(json!({ "items": items }))
    }

    async fn data_write(&self, _: &ExecCtx, req: DataWriteReq) -> Result<Value, PortError> {
        let DataWriteOp::BulkUpsert {
            rows,
            abort_on_invalid,
        } = req.op
        else {
            panic!("まとめて登録だけ: {:?}", req.op)
        };
        let mut s = self.s.lock().unwrap();
        if let Some(out) = s.done.get(&req.idempotency_key) {
            return Ok(out.clone());
        }
        if !s.interleave.is_empty() {
            let f = s.interleave.remove(0);
            f(&mut s.rows);
        }
        s.imports.push((req.idempotency_key.clone(), rows.clone()));
        let out = bulk(&mut s, &rows, abort_on_invalid);
        s.done.insert(req.idempotency_key, out.clone());
        Ok(out)
    }
}

fn ec() -> ExecCtx {
    ExecCtx {
        tenant_id: "t1".into(),
        org: "acme".into(),
        principal: "alice".into(),
        principal_kind: "user".into(),
        trace_id: None,
        run_id: None,
        step_path: None,
    }
}

fn row(data: Value) -> BulkUpsertRow {
    BulkUpsertRow {
        key: None,
        record_id: None,
        expected_rev: None,
        data,
    }
}

async fn run(f: &Fake, rows: Vec<BulkUpsertRow>, abort_on_invalid: bool) -> Value {
    let req = MatchedBulkReq {
        table_id: TABLE,
        match_on: NO,
        rows,
        abort_on_invalid,
        idempotency_key: KEY,
    };
    bulk_matched(f, &ec(), req).await.unwrap()
}

fn counts(out: &Value) -> (i64, i64, i64, i64, i64) {
    let n = |k: &str| out[k].as_i64().unwrap();
    (
        n("rows_total"),
        n("rows_applied"),
        n("rows_invalid"),
        n("rows_rejected"),
        n("rows_matched"),
    )
}

/// 重ならなければ 1 回の取込のまま（当たった行は更新・残りは追加）。
#[tokio::test]
async fn without_a_race_it_imports_once() {
    let f = Fake::default();
    f.insert(json!({ NO: "A", "n": 1 }), false);
    let out = run(
        &f,
        vec![
            row(json!({ NO: "A", "n": 2 })),
            row(json!({ NO: "B", "n": 3 })),
        ],
        false,
    )
    .await;
    assert_eq!(counts(&out), (2, 2, 0, 0, 1));
    assert_eq!(f.import_keys(), [KEY]);
    assert_eq!(f.values_of("A")[0]["n"], 2);
}

/// 照合の後に別の実行が同じ値を追加した（適用で一意の違反）: 断られた行を探し直し、更新として取り込み直す。
/// どの値も 1 件のまま、こちらの値が書かれ、結果は全行を書いた形になる。
#[tokio::test]
async fn rows_added_meanwhile_are_imported_again_as_updates() {
    let f = Fake::default();
    f.added_meanwhile(&["A", "C"]);
    let rows = vec![
        row(json!({ NO: "A", "n": 1 })),
        row(json!({ NO: "B", "n": 2 })),
        row(json!({ NO: "C", "n": 3 })),
    ];
    let out = run(&f, rows, false).await;
    assert_eq!(counts(&out), (3, 3, 0, 0, 2));
    assert_eq!(out["errors"], json!([]));
    assert_eq!(out["status"], "done");
    for (no, n) in [("A", 1), ("B", 2), ("C", 3)] {
        let got = f.values_of(no);
        assert_eq!(got.len(), 1, "{no} は 1 件");
        assert_eq!(got[0]["n"], n, "{no} にこちらの値が書かれる");
    }
    let keys = f.import_keys();
    assert_eq!(keys, [KEY.to_string(), format!("{KEY}#rematch")]);
    // 取り込み直すのは断られた 2 行だけ（どちらも当たったレコードの更新）。
    let s = f.s.lock().unwrap();
    let again = &s.imports[1].1;
    assert_eq!(again.len(), 2);
    assert!(again.iter().all(|r| r.record_id.is_some()));
}

/// 同じ step をやり直しても（効果の記録の前に落ちた）、取込は冪等キーごとに 1 回で、結果は同じ。
#[tokio::test]
async fn running_again_imports_nothing_twice() {
    let f = Fake::default();
    f.added_meanwhile(&["A"]);
    let rows = || {
        vec![
            row(json!({ NO: "A", "n": 1 })),
            row(json!({ NO: "B", "n": 2 })),
        ]
    };
    let first = run(&f, rows(), false).await;
    let again = run(&f, rows(), false).await;
    assert_eq!(counts(&first), (2, 2, 0, 0, 1));
    assert_eq!(counts(&again).1, 2);
    assert_eq!(
        f.s.lock().unwrap().imports.len(),
        2,
        "取込は 2 回（1 回目と取り込み直し）だけ"
    );
    assert_eq!(f.values_of("A").len(), 1);
    assert_eq!(f.values_of("B").len(), 1);
}

/// 何も取り込まない設定（`abort_on_invalid`）で、同じ値が検証で重なった: 何も書いていないので、追加の行を探し直して
/// 全体を取り込み直す。
#[tokio::test]
async fn an_aborted_import_is_imported_again_as_a_whole() {
    let f = Fake::default();
    f.s.lock().unwrap().race_in_validation = true;
    f.added_meanwhile(&["B"]);
    let rows = vec![
        row(json!({ NO: "A", "n": 1 })),
        row(json!({ NO: "B", "n": 2 })),
    ];
    let out = run(&f, rows, true).await;
    assert_eq!(out["status"], "done");
    assert_eq!(counts(&out), (2, 2, 0, 0, 1));
    assert_eq!(f.values_of("A").len(), 1);
    assert_eq!(f.values_of("B")[0]["n"], 2);
    assert_eq!(f.import_keys().len(), 2);
}

/// 不合格の行が本当の誤り（探し直しても当たらない）なら、何も取り込まないまま（取り込み直さない）。
#[tokio::test]
async fn an_aborted_import_with_a_real_error_stays_aborted() {
    let f = Fake::default();
    let rows = vec![
        row(json!({ NO: "A", "n": 1 })),
        row(json!({ NO: "B", "bad": true })),
    ];
    let out = run(&f, rows, true).await;
    assert_eq!(out["status"], "failed");
    assert_eq!(counts(&out), (2, 0, 1, 0, 0));
    assert_eq!(f.import_keys(), [KEY]);
    assert!(f.values_of("A").is_empty());
}

/// 本当の誤りの行は誤りのまま残り、断られた追加の行だけを取り込み直す。数は合い、誤りは元の行の番号で返る。検証で
/// 重なった行（不合格）も取り込み直す。
#[tokio::test]
async fn only_the_raced_rows_are_imported_again() {
    let f = Fake::default();
    f.s.lock().unwrap().race_in_validation = true;
    f.added_meanwhile(&["C"]);
    let rows = vec![
        row(json!({ NO: "A", "bad": true })),
        row(json!({ NO: "B", "n": 2 })),
        row(json!({ NO: "C", "n": 3 })),
    ];
    let out = run(&f, rows, false).await;
    assert_eq!(counts(&out), (3, 2, 1, 0, 1));
    assert_eq!(
        out["errors"],
        json!([{ "row": 1, "key": null, "message": "「数」には数を入れてください" }])
    );
    assert_eq!(f.values_of("C")[0]["n"], 3);
}

/// 取り込み直した行がまた断られたら、その誤りを元の行の番号で返す（拒否から先に数え直す）。
#[tokio::test]
async fn a_failure_of_the_second_import_keeps_the_original_row_number() {
    let f = Fake::default();
    f.added_meanwhile(&["C"]);
    // 取り込み直しの直前に、別の実行が C の行を消して見えない行にする（更新が「見つかりません」で断られる）。
    f.s.lock()
        .unwrap()
        .interleave
        .push(Box::new(|rows: &mut Vec<Row>| {
            for r in rows
                .iter_mut()
                .filter(|r| r.data.get(NO) == Some(&json!("C")))
            {
                r.hidden = true;
            }
        }));
    let rows = vec![
        row(json!({ NO: "A" })),
        row(json!({ NO: "B" })),
        row(json!({ NO: "C" })),
    ];
    let out = run(&f, rows, false).await;
    assert_eq!(counts(&out), (3, 2, 0, 1, 1));
    assert_eq!(
        out["errors"],
        json!([{ "row": 3, "key": null, "message": "対象が見つかりません" }])
    );
}

/// 見えない行の値と重なった追加は、探し直しても当たらないので誤りのまま（取り込み直さない・見えない行を明かさない）。
#[tokio::test]
async fn a_hidden_duplicate_stays_rejected() {
    let f = Fake::default();
    f.insert(json!({ NO: "A", "by": "other" }), true);
    let out = run(
        &f,
        vec![row(json!({ NO: "A" })), row(json!({ NO: "B" }))],
        false,
    )
    .await;
    assert_eq!(counts(&out), (2, 1, 0, 1, 0));
    assert_eq!(out["errors"][0]["row"], 1);
    assert_eq!(f.import_keys(), [KEY]);
}

/// id を書いた行は探し直さない（断られても、別のレコードに当て替えない）。
#[tokio::test]
async fn rows_with_an_id_are_never_rematched() {
    let f = Fake::default();
    f.insert(json!({ NO: "A" }), false);
    let gone = Uuid::new_v4();
    let rows = vec![BulkUpsertRow {
        record_id: Some(gone),
        ..row(json!({ NO: "A", "n": 1 }))
    }];
    let out = run(&f, rows, false).await;
    assert_eq!(counts(&out), (1, 0, 0, 1, 0));
    assert_eq!(f.import_keys(), [KEY]);
}

//! ⑬ 作成者の列（`created_by`・D-122・BUG-148）の脅威モデル。
//!
//! 作成者の列はレコードの作成者（owner）で埋まり、だれも書き換えられない。行の決まりの比べ（`field_cmp`）は、
//! どの読取の経路（索引・本体・事後検査・SSE・SQL・件数・集計）でも owner と比べる。書き込みの入口（作成・更新・
//! 一括取込の JSON と CSV）は、作成者の列への値を 400 で断る（黙って捨てない）。状態の進め方の「申請した本人は
//! 除く」を作成者の列で書くと、申請者の列（だれでも書けるメンバーの列）をほかの人にしても自分で承認できない。
//!
//! テストで作った行・タプル・OpenFGA のストアは、テストの終わりに消す。

use super::*;

use data::{BulkImportOptions, BulkImportRow, FsmBody, FsmTransition};

/// [`setup`] と同じ環境に、後で消すストアを添える。
pub(super) async fn setup_cleaned() -> Option<(PgPool, Arc<dyn AuthzClient>, String, String)> {
    let db_url = shiki_testenv::var("STORAGE_TEST_DATABASE_URL").ok()?;
    let base_url = shiki_testenv::var("OPENFGA_TEST_URL").ok()?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("Postgres へ接続できること");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("マイグレーション適用");
    let model: serde_json::Value = serde_json::from_str(include_str!(
        "../../../authz/model/authorization-model.json"
    ))
    .expect("model json");
    let config = OpenFgaConfig {
        base_url: base_url.clone(),
        store_name: format!("shiki-data-created-by-test-{}", Uuid::new_v4()),
    };
    let fga = OpenFgaClient::connect(reqwest::Client::new(), &config, &model)
        .await
        .expect("OpenFGA 接続");
    let store_id = fga.store_id().to_string();
    Some((pool, Arc::new(fga), base_url, store_id))
}

pub(super) async fn delete_store(base_url: &str, store_id: &str) {
    let r = reqwest::Client::new()
        .delete(format!("{base_url}/stores/{store_id}"))
        .send()
        .await
        .expect("OpenFGA delete store");
    assert!(r.status().is_success(), "delete store: {}", r.status());
}

/// 申請の表: 件名・申請者（メンバーの列）・作成者の列・金額。
fn requests_schema(read: PolicyExpr, write: Option<PolicyExpr>) -> TableSchema {
    let mut title = field("title", FieldType::Text);
    title.indexed = true;
    let mut applicant = field("applicant", FieldType::UserRef);
    applicant.indexed = true;
    let mut made_by = field("made_by", FieldType::CreatedBy);
    made_by.display_name = "作成者".into();
    let mut amount = field("amount", FieldType::Number);
    amount.indexed = true;
    let mut status = field("status", FieldType::Select);
    status.options = vec!["申請中".into(), "承認".into()];
    status.indexed = true;
    TableSchema {
        row_policy: Some(RowPolicy { read, write }),
        ..plain(vec![title, applicant, made_by, amount, status])
    }
}

fn made_by_is(op: CmpOp) -> PolicyExpr {
    PolicyExpr::FieldCmp {
        field: "made_by".into(),
        op,
        value: PolicyOperand::UserId,
    }
}

fn titles_of(r: &data::QueryResult) -> Vec<String> {
    r.items
        .as_ref()
        .unwrap()
        .iter()
        .map(|it| it.data["title"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn query(v: serde_json::Value) -> DataQuery {
    serde_json::from_value(v).unwrap()
}

/// ⑬-1 作成者の列は作成者で埋まり（列を足す前のレコードも）、書けない。行の決まり「作成者 = 本人 か 経理」は、
/// 申請者の列を書き換えても、どの読取の経路でも作成者で効く。
#[tokio::test]
async fn created_by_is_the_owner_on_every_path_and_cannot_be_written() {
    let Some((pool, authz, base_url, store_id)) = setup_cleaned().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let (alice, bob, carol) = (
        ctx(&tenant, "alice"),
        ctx(&tenant, "bob"),
        ctx(&tenant, "carol"),
    );
    let s = DataStore::new(pool, Arc::clone(&authz), Arc::new(AllowRefs));
    // 作成者の列を後から足すテーブル（先に 1 件作っておく）。
    let mut first = requests_schema(
        PolicyExpr::Any(vec![
            made_by_is(CmpOp::Eq),
            PolicyExpr::HasRole {
                role: "keiri".into(),
                subtree: true,
            },
        ]),
        Some(made_by_is(CmpOp::Eq)),
    );
    let made_by_def = first.fields.remove(2);
    first.row_policy = None;
    let t = s
        .create_table(
            &alice,
            NewDataTable {
                name: "created-by-requests".into(),
                schema: first,
            },
            None,
        )
        .await
        .unwrap();
    let obj = alice.ns().data_table(&t.id.to_string());
    for who in [&bob, &carol] {
        authz
            .write_tuple(&who.subject(), Relation::Editor, &obj)
            .await
            .unwrap();
    }
    join_role(&authz, &carol, "keiri").await;
    let before = s
        .create_record(
            &alice,
            t.id,
            json!({"title": "前からある", "applicant": "bob", "amount": 1}),
            None,
        )
        .await
        .unwrap();
    // 作成者の列と行の決まりを足す（作成者の列は索引を宣言しなくてよい・indexed は常に立つ）。
    let mut next = s.get_table(&alice, t.id, None).await.unwrap();
    next.schema.fields.push(made_by_def);
    next.schema.row_policy = Some(RowPolicy {
        read: PolicyExpr::Any(vec![
            made_by_is(CmpOp::Eq),
            PolicyExpr::HasRole {
                role: "keiri".into(),
                subtree: true,
            },
        ]),
        write: Some(made_by_is(CmpOp::Eq)),
    });
    let saved = s
        .update_table_schema(&alice, t.id, next.schema, Some(next.schema_version), None)
        .await
        .unwrap();
    assert!(saved.schema.field("made_by").unwrap().indexed);
    // 足す前のレコードも作成者で埋まって見える。
    let got = s.get_record(&alice, t.id, before.id, None).await.unwrap();
    assert_eq!(got.data["made_by"], json!("alice"));

    // bob が申請者の列を alice にして作る（申請者の列は書き換えられる）。作成者は bob のまま。
    let b = s
        .create_record(
            &bob,
            t.id,
            json!({"title": "bob-1", "applicant": "alice", "amount": 20}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        b.data["made_by"],
        json!("bob"),
        "作成の応答も作成者で埋まる"
    );
    let a = s
        .create_record(
            &alice,
            t.id,
            json!({"title": "alice-1", "applicant": "bob", "amount": 10}),
            None,
        )
        .await
        .unwrap();

    // 書けない: 作成・更新・一括取込（JSON・CSV）。値を書こうとしたら 400（黙って捨てない）。
    let written = |r: &Result<data::DataRecord, DataError>| {
        matches!(r, Err(e) if e.invalid_code() == Some(data::InvalidCode::InvalidInput)
            && e.to_string().contains("「作成者」には書き込めません"))
    };
    let r = s
        .create_record(&bob, t.id, json!({"title": "x", "made_by": "alice"}), None)
        .await;
    assert!(written(&r), "{r:?}");
    let r = s
        .create_record(&bob, t.id, json!({"title": "x", "made_by": "bob"}), None)
        .await;
    assert!(written(&r), "本人の値でも断る: {r:?}");
    let r = s
        .update_record(&bob, t.id, b.id, json!({"made_by": "alice"}), b.rev, None)
        .await;
    assert!(written(&r), "{r:?}");
    let imp = s
        .start_bulk_import(
            &bob,
            t.id,
            vec![BulkImportRow {
                key: Some("forged".into()),
                data: json!({"title": "forged", "made_by": "alice"}),
            }],
            BulkImportOptions::default(),
            None,
        )
        .await
        .unwrap();
    s.run_bulk_import(&tenant, t.id, imp).await.unwrap();
    let errs = s.bulk_import_errors(&bob, t.id, imp, 0, 10).await.unwrap();
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert!(
        errs[0].message.contains("「作成者」には書き込めません"),
        "{errs:?}"
    );
    let csv = s
        .start_bulk_import_csv(
            &bob,
            t.id,
            "title,made_by\nforged,alice\n".as_bytes(),
            BulkImportOptions::default(),
            None,
        )
        .await;
    assert!(
        matches!(&csv, Err(e) if e.to_string().contains("作成者の列には、レコードを追加した人が自動で入ります")),
        "{csv:?}"
    );

    // 読取: bob には自分が作った行だけ（申請者の列が bob の alice-1 も見えない）。件数・条件・並べ替え・集計も同じ。
    let mine = s
        .run_query(&bob, t.id, &query(json!({"with_count": true})), None)
        .await
        .unwrap();
    assert_eq!(titles_of(&mine), vec!["bob-1"]);
    assert_eq!(mine.count, Some(data::CountResult::Exact(1)));
    assert_eq!(s.count_records(&bob, t.id, None, None).await.unwrap(), 1);
    for probe in [
        json!({"filter": {"cmp": {"field": "made_by", "op": "eq", "value": "alice"}}}),
        json!({"filter": {"cmp": {"field": "applicant", "op": "eq", "value": "bob"}}}),
    ] {
        let r = s
            .run_query(&bob, t.id, &query(probe.clone()), None)
            .await
            .unwrap();
        assert!(titles_of(&r).is_empty(), "{probe}: {:?}", titles_of(&r));
    }
    // 集計は自分の行だけ。作成者の列でのグループ化は擬似列 `_owner` と同じく受け付けない（作成者ごとの件数は出さない）。
    let g = s
        .run_query(
            &bob,
            t.id,
            &query(json!({"aggregate": [{"metric": "count"}]})),
            None,
        )
        .await
        .unwrap()
        .groups
        .unwrap();
    assert_eq!(g[0].values[0], json!(1), "{g:?}");
    let r = s
        .run_query(
            &bob,
            t.id,
            &query(json!({"group_by": ["made_by"], "aggregate": [{"metric": "count"}]})),
            None,
        )
        .await;
    assert!(
        matches!(&r, Err(e) if e.invalid_code() == Some(data::InvalidCode::FieldNotSortable)),
        "{r:?}"
    );
    let hidden = s.get_record(&bob, t.id, a.id, None).await;
    let missing = s.get_record(&bob, t.id, Uuid::new_v4(), None).await;
    assert_eq!(format!("{hidden:?}"), format!("{missing:?}"));
    // 経理の carol には全部見え、作成者の列で絞り込み・並べ替えできる。値は作成者。
    let all = s
        .run_query(
            &carol,
            t.id,
            &query(json!({"sort": [{"field": "made_by"}, {"field": "title"}]})),
            None,
        )
        .await
        .unwrap();
    assert_eq!(titles_of(&all), vec!["alice-1", "前からある", "bob-1"]);
    let by: Vec<_> = all
        .items
        .as_ref()
        .unwrap()
        .iter()
        .map(|it| it.data["made_by"].clone())
        .collect();
    assert_eq!(by, vec![json!("alice"), json!("alice"), json!("bob")]);
    let r = s
        .run_query(
            &carol,
            t.id,
            &query(json!({"filter": {"cmp": {"field": "made_by", "op": "eq", "value": "bob"}}})),
            None,
        )
        .await
        .unwrap();
    assert_eq!(titles_of(&r), vec!["bob-1"]);
    // 表示名で絞り込むと黙って 0 件にせず、ユーザーの ID で指定するよう言う（メンバーの列と同じ）。
    let r = s
        .run_query(
            &carol,
            t.id,
            &query(
                json!({"filter": {"cmp": {"field": "made_by", "op": "eq", "value": "山田 太郎"}}}),
            ),
            None,
        )
        .await;
    assert!(
        matches!(&r, Err(e) if e.invalid_code() == Some(data::InvalidCode::ValueTypeMismatch)),
        "{r:?}"
    );
    // 書き込みの決まり「作成者 = 本人」: 経理の carol は読めても書けない。alice は自分の行を書ける。
    let r = s
        .update_record(&carol, t.id, a.id, json!({"amount": 11}), a.rev, None)
        .await;
    assert!(matches!(r, Err(DataError::Forbidden)), "{r:?}");
    s.update_record(&alice, t.id, a.id, json!({"amount": 11}), a.rev, None)
        .await
        .unwrap();

    // SSE: bob には自分が作った行の変更だけが届く（申請者の列が bob の行は届かない）。上の表はスキーマを改訂した
    // ので（購読は改訂の知らせで張り直しになる）、はじめから作成者の列と行の決まりを持つ表で確かめる。
    let live = s
        .create_table(
            &alice,
            NewDataTable {
                name: "created-by-live".into(),
                schema: requests_schema(made_by_is(CmpOp::Eq), None),
            },
            None,
        )
        .await
        .unwrap();
    authz
        .write_tuple(
            &bob.subject(),
            Relation::Editor,
            &alice.ns().data_table(&live.id.to_string()),
        )
        .await
        .unwrap();
    let mut stream = s
        .subscribe_records(&bob, live.id, None, None)
        .await
        .unwrap();
    let other = s
        .create_record(
            &alice,
            live.id,
            json!({"title": "alice-sse", "applicant": "bob"}),
            None,
        )
        .await
        .unwrap();
    let own = s
        .create_record(
            &bob,
            live.id,
            json!({"title": "bob-sse", "applicant": "alice"}),
            None,
        )
        .await
        .unwrap();
    let mut seen = Vec::new();
    let mut got_own = false;
    let deadline = std::time::Instant::now() + SSE_WAIT;
    while std::time::Instant::now() < deadline && !got_own {
        if let Ok(Some(m)) =
            tokio::time::timeout(std::time::Duration::from_millis(500), stream.next()).await
        {
            if let data::StreamEvent::Upserted(u) = &m.event {
                assert_eq!(u.record.id, own.id, "作成者でない行は届かない");
                assert_eq!(u.record.data["made_by"], json!("bob"));
                got_own = true;
            }
            seen.push(format!("{} {}", m.event.name(), m.event.data_json()));
        }
    }
    assert!(got_own, "自分の行は届く: {seen:?}");
    assert!(!seen.join("\n").contains(&other.id.to_string()), "{seen:?}");
    assert_eq!(s.stream_verify_drops(), 0);

    // SQL 前段面も同じ（作成者の列で絞っても他人の行は出ない）。
    let rows = sql_rows(
        &s,
        &bob,
        "SELECT title, made_by FROM \"created-by-requests\" WHERE made_by = 'alice'",
    )
    .await;
    assert!(rows.is_empty(), "{rows:?}");
    let rows = sql_rows(
        &s,
        &carol,
        "SELECT title, made_by FROM \"created-by-requests\" WHERE made_by = 'bob' ORDER BY title",
    )
    .await;
    assert_eq!(rows, vec![vec![json!("bob-1"), json!("bob")]]);
    for who in [&alice, &bob, &carol] {
        sql_mirror(&s, who, t.id, "created-by-requests").await;
    }
    let st = s.plan_cache_stats();
    assert_eq!((st.mismatches, st.verify_failures), (0, 0), "{st:?}");

    s.purge_tenant(&tenant).await.unwrap();
    delete_store(&base_url, &store_id).await;
}

/// ⑬-2 状態の進め方の「申請した本人は除く」を作成者の列で書くと、申請者の列（メンバーの列）をほかの人にしても、
/// 自分が作った申請は承認できない（BUG-148）。`ne` は作成者以外で真（owner は空にならない）なので、ほかの人の
/// 申請は承認できる。読む決まり「作成者 ≠ 本人」も、どの読取の経路でも作成者で効く。
#[tokio::test]
async fn excluding_the_creator_cannot_be_bypassed_by_rewriting_the_applicant() {
    let Some((pool, authz, base_url, store_id)) = setup_cleaned().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let (alice, bob) = (ctx(&tenant, "alice"), ctx(&tenant, "bob"));
    let s = DataStore::new(pool, Arc::clone(&authz), Arc::new(AllowRefs));
    let mut schema = requests_schema(made_by_is(CmpOp::Ne), Some(PolicyExpr::Public));
    schema.status_field = Some("status".into());
    let t = s
        .create_table(
            &alice,
            NewDataTable {
                name: "created-by-approvals".into(),
                schema,
            },
            None,
        )
        .await
        .unwrap();
    authz
        .write_tuple(
            &bob.subject(),
            Relation::Editor,
            &alice.ns().data_table(&t.id.to_string()),
        )
        .await
        .unwrap();
    let mut made = Vec::new();
    for (who, title, applicant) in [
        (&alice, "alice-1", "bob"),
        (&alice, "alice-2", "bob"),
        (&bob, "bob-1", "alice"),
    ] {
        made.push(
            s.create_record(
                who,
                t.id,
                json!({"title": title, "applicant": applicant, "status": "申請中"}),
                None,
            )
            .await
            .unwrap(),
        );
    }
    // 読む決まり「作成者 ≠ 本人」: alice には bob の行だけ、bob には alice の行だけ（申請者の列に依らない）。
    let r = s
        .run_query(&alice, t.id, &query(json!({"with_count": true})), None)
        .await
        .unwrap();
    assert_eq!(titles_of(&r), vec!["bob-1"]);
    assert_eq!(r.count, Some(data::CountResult::Exact(1)));
    let r = s
        .run_query(
            &bob,
            t.id,
            &query(json!({"sort": [{"field": "title"}], "with_count": true})),
            None,
        )
        .await
        .unwrap();
    assert_eq!(titles_of(&r), vec!["alice-1", "alice-2"]);
    assert_eq!(r.count, Some(data::CountResult::Exact(2)));
    assert_eq!(s.count_records(&bob, t.id, None, None).await.unwrap(), 2);
    for who in [&alice, &bob] {
        sql_mirror(&s, who, t.id, "created-by-approvals").await;
    }

    // 承認の進め方: 進められる人は「テーブルを編集できる人で、作成者でない人」。
    let fsm = FsmBody {
        states: vec!["申請中".into(), "承認".into()],
        transitions: vec![FsmTransition {
            from: "申請中".into(),
            to: "承認".into(),
            actor: PolicyExpr::All(vec![PolicyExpr::Public, made_by_is(CmpOp::Ne)]),
        }],
    };
    // 読む決まりを外して、作成者も自分の行を見られるようにする（進め方の判定だけを確かめる）。
    let cur = s.get_table(&alice, t.id, None).await.unwrap();
    let mut open = cur.schema.clone();
    open.row_policy = None;
    s.update_table_schema(&alice, t.id, open, Some(cur.schema_version), None)
        .await
        .unwrap();
    // bob は自分の申請の申請者の列を alice にしている（最初の状態のあいだは書き換えられる）。それでも承認できない。
    let bobs = s.get_record(&bob, t.id, made[2].id, None).await.unwrap();
    assert_eq!(bobs.data["applicant"], json!("alice"));
    let r = s
        .transition_record(&bob, t.id, bobs.id, "承認", bobs.rev, &fsm, "status", None)
        .await;
    assert!(matches!(r, Err(DataError::Forbidden)), "{r:?}");
    // 申請者の列を書き換え直しても同じ（作成者は変わらない）。
    let rewritten = s
        .update_record(
            &bob,
            t.id,
            bobs.id,
            json!({"applicant": "bob"}),
            bobs.rev,
            None,
        )
        .await
        .unwrap();
    let r = s
        .transition_record(
            &bob,
            t.id,
            bobs.id,
            "承認",
            rewritten.rev,
            &fsm,
            "status",
            None,
        )
        .await;
    assert!(matches!(r, Err(DataError::Forbidden)), "{r:?}");
    // ほかの人（alice）は bob の申請を承認できる。bob は alice の申請を承認できる。
    let done = s
        .transition_record(
            &alice,
            t.id,
            bobs.id,
            "承認",
            rewritten.rev,
            &fsm,
            "status",
            None,
        )
        .await
        .unwrap();
    assert_eq!(done.data["status"], json!("承認"));
    assert_eq!(done.data["made_by"], json!("bob"));
    let a1 = s.get_record(&bob, t.id, made[0].id, None).await.unwrap();
    s.transition_record(&bob, t.id, a1.id, "承認", a1.rev, &fsm, "status", None)
        .await
        .unwrap();

    s.purge_tenant(&tenant).await.unwrap();
    delete_store(&base_url, &store_id).await;
}

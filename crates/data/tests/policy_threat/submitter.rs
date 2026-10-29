//! ⑫ テーブルの「追加だけ」（`data_table#submitter`・D-113・GAP-49）の脅威モデル。
//!
//! 追加だけの人は、レコードを作れて、自分が作ったレコードだけを読める（行述語があれば積・個別共有は足す）。
//! 他人のレコードの存在・値が、読取・件数・集計・リンク先の表示・ルックアップ・ロールアップ・リンクの
//! 半結合・履歴・SSE・SQL・RAG のどこからも漏れないこと（「見えない」と「無い」を区別させない）。
//! 書き換え・削除・状態を進める・一括取込はできない（editor が要る）。
//! 材料化の前提（§7.4）は、追加だけの人を参照元の読み手に数え、参照先の読み手には数えない。

use super::*;

use data::{BulkImportOptions, BulkImportRow, FsmBody, FsmTransition, TableShareRole};
use storage::ShareTarget;

fn user(id: &str) -> ShareTarget {
    ShareTarget::User { id: id.into() }
}

/// 申請の表（行述語なし・件名は索引・検索の対象）。
fn requests_schema() -> TableSchema {
    let mut title = field("title", FieldType::Text);
    title.indexed = true;
    title.searchable = true;
    let mut amount = field("amount", FieldType::Number);
    amount.indexed = true;
    let mut status = field("status", FieldType::Select);
    status.options = vec!["申請中".into(), "承認済み".into()];
    status.indexed = true;
    plain(vec![title, amount, status, field("memo", FieldType::Text)])
}

struct AddOnly {
    store: DataStore,
    tenant: String,
    alice: AuthContext,
    bob: AuthContext,
    carol: AuthContext,
    dave: AuthContext,
    table: Uuid,
    /// alice・carol・bob が作った行。
    a: Uuid,
    c: Uuid,
    b: Uuid,
}

/// alice（持ち主）が申請の表を作り、bob と carol に追加だけを、dave に閲覧を渡す。3 人が 1 件ずつ作る。
async fn add_only_fixture(pool: PgPool, authz: Arc<dyn AuthzClient>) -> AddOnly {
    let tenant = format!("t-{}", Uuid::new_v4());
    let (alice, bob, carol, dave) = (
        ctx(&tenant, "alice"),
        ctx(&tenant, "bob"),
        ctx(&tenant, "carol"),
        ctx(&tenant, "dave"),
    );
    let store = DataStore::new(pool, Arc::clone(&authz), Arc::new(AllowRefs));
    let t = store
        .create_table(
            &alice,
            NewDataTable {
                name: "requests".into(),
                schema: requests_schema(),
            },
            None,
        )
        .await
        .unwrap();
    // 共有の入口（持ち主だけ・監査つき）で渡す。
    for (who, role) in [
        ("bob", TableShareRole::Submitter),
        ("carol", TableShareRole::Submitter),
        ("dave", TableShareRole::Viewer),
    ] {
        store
            .share_table(&alice, t.id, &user(who), role, None)
            .await
            .unwrap();
    }
    let mut ids = Vec::new();
    for (who, title, amount) in [
        (&alice, "alice-secret", 999_001),
        (&carol, "carol-secret", 777_001),
        (&bob, "bob-own", 100),
    ] {
        ids.push(
            store
                .create_record(
                    who,
                    t.id,
                    json!({"title": title, "amount": amount, "status": "申請中", "memo": format!("{title}-memo")}),
                    None,
                )
                .await
                .unwrap_or_else(|e| panic!("{title}: {e:?}"))
                .id,
        );
    }
    AddOnly {
        store,
        tenant,
        alice,
        bob,
        carol,
        dave,
        table: t.id,
        a: ids[0],
        c: ids[1],
        b: ids[2],
    }
}

fn titles(r: &data::QueryResult) -> Vec<String> {
    let mut v: Vec<String> = r
        .items
        .as_ref()
        .unwrap()
        .iter()
        .map(|it| it.data["title"].as_str().unwrap_or_default().to_string())
        .collect();
    v.sort();
    v
}

/// ⑫-1 追加だけの人は、定義を読めてレコードを作れ、自分の行だけが読取・件数・集計・検索・履歴に出る。
/// 他人の行は「無い」行と同じ応答になる。書き換え・削除・状態を進める・一括取込はできない。
#[tokio::test]
async fn add_only_sees_only_own_records_and_cannot_change_them() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let f = add_only_fixture(pool, Arc::clone(&authz)).await;
    let (s, bob, t) = (&f.store, &f.bob, f.table);

    // (1) 定義（持ち主でない閲覧者と同じく読める列だけ・ポリシーの定義なし）と画面の文脈。
    let def = s.get_table_for_viewer(bob, t, None).await.unwrap();
    assert!(def.add_only, "追加だけの印");
    assert!(
        def.schema.row_policy.is_none(),
        "絞った行述語も定義に出さない"
    );
    let vc = s.table_viewer_context(bob, t, None).await.unwrap();
    assert!(
        vc.can_add && vc.add_only && !vc.can_edit && !vc.can_manage,
        "{vc:?}"
    );
    let dave_vc = s.table_viewer_context(&f.dave, t, None).await.unwrap();
    assert!(
        !dave_vc.can_add && !dave_vc.add_only,
        "閲覧だけは追加できない: {dave_vc:?}"
    );
    let alice_vc = s.table_viewer_context(&f.alice, t, None).await.unwrap();
    assert!(alice_vc.can_add && alice_vc.can_edit && !alice_vc.add_only);
    // 一覧（画面）にも印つきで出る。チャットの AI などが使う viewer の一覧には出ない。
    let listed = s.list_tables_for_viewer(bob, 50).await.unwrap();
    assert!(listed.iter().any(|x| x.id == t && x.add_only), "{listed:?}");
    assert!(s
        .list_tables(bob, 50)
        .await
        .unwrap()
        .iter()
        .all(|x| x.id != t));

    // (3) 読取は自分の行だけ（件数・集計・検索・条件も同じ）。
    let all = s
        .run_query(bob, t, &DataQuery::default(), None)
        .await
        .unwrap();
    assert_eq!(titles(&all), vec!["bob-own"]);
    assert_eq!(s.count_records(bob, t, None, None).await.unwrap(), 1);
    let probe: DataQuery = serde_json::from_value(
        json!({"filter": {"cmp": {"field": "title", "op": "eq", "value": "alice-secret"}}, "with_count": true}),
    )
    .unwrap();
    let r = s.run_query(bob, t, &probe, None).await.unwrap();
    assert!(r.items.unwrap().is_empty(), "条件で他人の行を当てられない");
    let r = s
        .run_query(
            bob,
            t,
            &DataQuery {
                search: Some("secret".into()),
                ..DataQuery::default()
            },
            None,
        )
        .await
        .unwrap();
    assert!(r.items.unwrap().is_empty(), "検索で他人の行を当てられない");
    let agg: DataQuery = serde_json::from_value(json!({
        "aggregate": [{"metric": "sum", "field": "amount"}, {"metric": "count"}],
    }))
    .unwrap();
    let g = s
        .run_query(bob, t, &agg, None)
        .await
        .unwrap()
        .groups
        .unwrap();
    assert_eq!(g.len(), 1);
    assert_eq!(
        g[0].values[0].as_f64(),
        Some(100.0),
        "合計は自分の行だけ: {g:?}"
    );
    assert_eq!(g[0].values[1], json!(1), "件数も自分の行だけ: {g:?}");
    let by_title: DataQuery = serde_json::from_value(json!({
        "group_by": ["title"], "aggregate": [{"metric": "count"}],
    }))
    .unwrap();
    let g = s
        .run_query(bob, t, &by_title, None)
        .await
        .unwrap()
        .groups
        .unwrap();
    assert_eq!(g.len(), 1, "グループのキーにも他人の値が出ない: {g:?}");
    // 閲覧（dave）と持ち主（alice）には全部見える（追加だけの絞り込みが他の人へ移らない）。
    for who in [&f.dave, &f.alice] {
        let r = s
            .run_query(who, t, &DataQuery::default(), None)
            .await
            .unwrap();
        assert_eq!(titles(&r), vec!["alice-secret", "bob-own", "carol-secret"]);
    }
    // carol（もう 1 人の追加だけ）も自分の行だけ。
    let r = s
        .run_query(&f.carol, t, &DataQuery::default(), None)
        .await
        .unwrap();
    assert_eq!(titles(&r), vec!["carol-secret"]);
    // 同じ形のクエリを閲覧者と追加だけの人が交互に流しても、プランキャッシュが絞った述語と絞らない述語を
    // 取り違えない（文脈のキーに行述語の形が入る）。
    for who in [bob, &f.dave, bob, &f.alice] {
        s.run_query(who, t, &DataQuery::default(), None)
            .await
            .unwrap();
    }
    let st = s.plan_cache_stats();
    assert_eq!((st.mismatches, st.verify_failures), (0, 0), "{st:?}");

    // 1 件の読取・まとめ取り・候補の絞り込み・履歴: 他人の行は「無い」と同じ。
    s.get_record(bob, t, f.b, None).await.unwrap();
    let hidden = s.get_record(bob, t, f.a, None).await;
    let missing = s.get_record(bob, t, Uuid::new_v4(), None).await;
    assert_eq!(format!("{hidden:?}"), format!("{missing:?}"));
    let many = s.get_records(bob, t, &[f.a, f.b, f.c], None).await.unwrap();
    assert_eq!(
        many.iter().map(Option::is_some).collect::<Vec<_>>(),
        vec![false, true, false]
    );
    let filtered = s.filter_visible_records(bob, t, &[f.a, f.b]).await.unwrap();
    assert_eq!(
        filtered
            .unwrap()
            .records
            .iter()
            .map(Option::is_some)
            .collect::<Vec<_>>(),
        vec![false, true]
    );
    assert!(!s
        .list_revisions(bob, t, f.b, None, 50, None)
        .await
        .unwrap()
        .is_empty());
    let hidden = s.list_revisions(bob, t, f.a, None, 50, None).await;
    let missing = s
        .list_revisions(bob, t, Uuid::new_v4(), None, 50, None)
        .await;
    assert_eq!(format!("{hidden:?}"), format!("{missing:?}"));
    assert!(matches!(hidden, Err(DataError::NotFound)), "{hidden:?}");

    // (4) 書き換え・削除・状態を進める・一括取込は、自分の行でも他人の行でも同じ 403（表の権限で断るので、
    // 行の有無は分からない）。
    for id in [f.b, f.a, Uuid::new_v4()] {
        let r = s
            .update_record(bob, t, id, json!({"status": "承認済み"}), 1, None)
            .await;
        assert!(matches!(r, Err(DataError::Forbidden)), "update {id}: {r:?}");
        let r = s.delete_record(bob, t, id, 1, None).await;
        assert!(matches!(r, Err(DataError::Forbidden)), "delete {id}: {r:?}");
        let fsm = FsmBody {
            states: vec!["申請中".into(), "承認済み".into()],
            transitions: vec![FsmTransition {
                from: "申請中".into(),
                to: "承認済み".into(),
                actor: PolicyExpr::Public,
            }],
        };
        let r = s
            .transition_record(bob, t, id, "承認済み", 1, &fsm, "status", None)
            .await;
        assert!(
            matches!(r, Err(DataError::Forbidden)),
            "transition {id}: {r:?}"
        );
    }
    let r = s
        .start_bulk_import(
            bob,
            t,
            vec![BulkImportRow {
                key: None,
                data: json!({"title": "bulk"}),
            }],
            BulkImportOptions::default(),
            None,
        )
        .await;
    assert!(matches!(r, Err(DataError::Forbidden)), "{r:?}");
    let r = s
        .start_bulk_import_csv(bob, t, b"title\nbulk\n", BulkImportOptions::default(), None)
        .await;
    assert!(matches!(r, Err(DataError::Forbidden)), "{r:?}");
    // 閲覧だけ（dave）は追加できない（viewer は追加を含まない）。共有の無い人には表が「無い」。
    let r = s
        .create_record(&f.dave, t, json!({"title": "x"}), None)
        .await;
    assert!(matches!(r, Err(DataError::Forbidden)), "{r:?}");
    let erin = ctx(&f.tenant, "erin");
    let r = s.create_record(&erin, t, json!({"title": "x"}), None).await;
    assert!(matches!(r, Err(DataError::NotFound)), "{r:?}");
    let r = s.get_table_for_viewer(&erin, t, None).await;
    assert!(matches!(r, Err(DataError::NotFound)), "{r:?}");

    // SQL と RAG の post-filter は viewer だけ（追加だけの人には「無い」表と同じ）。
    let sql = s
        .run_sql(bob, &sql_req("SELECT title FROM requests"), None)
        .await;
    let nope = s
        .run_sql(bob, &sql_req("SELECT title FROM nopes"), None)
        .await;
    assert!(sql.is_err(), "{sql:?}");
    assert_eq!(
        format!("{sql:?}").replace("requests", "X"),
        format!("{nope:?}").replace("nopes", "X")
    );
    sql_mirror(s, &f.dave, t, "requests").await;
    let rag = s
        .rag_visible_records(bob, t, &[f.a, f.b], None)
        .await
        .unwrap();
    assert!(
        rag.records.is_empty() && rag.generation.is_none(),
        "{rag:?}"
    );

    // (3) 個別共有した行は今の規則どおり足される。外すと戻る。
    s.share_record(
        &f.alice,
        t,
        f.a,
        &user("bob"),
        RecordShareRole::Viewer,
        None,
    )
    .await
    .unwrap();
    let r = s
        .run_query(bob, t, &DataQuery::default(), None)
        .await
        .unwrap();
    assert_eq!(titles(&r), vec!["alice-secret", "bob-own"]);
    s.unshare_record(
        &f.alice,
        t,
        f.a,
        &user("bob"),
        RecordShareRole::Viewer,
        None,
    )
    .await
    .unwrap();
    let r = s
        .run_query(bob, t, &DataQuery::default(), None)
        .await
        .unwrap();
    assert_eq!(titles(&r), vec!["bob-own"]);

    // 共有の一覧には追加だけとして出る。取り消すと、即時に表が「無い」と同じになる。
    let shares = s.list_table_shares(&f.alice, t, None).await.unwrap();
    assert!(
        shares.contains(&(user("bob"), TableShareRole::Submitter)),
        "{shares:?}"
    );
    s.unshare_table(&f.alice, t, &user("bob"), TableShareRole::Submitter, None)
        .await
        .unwrap();
    for r in [
        s.get_table_for_viewer(bob, t, None).await.map(|_| ()),
        s.run_query(bob, t, &DataQuery::default(), None)
            .await
            .map(|_| ()),
        s.create_record(bob, t, json!({"title": "after"}), None)
            .await
            .map(|_| ()),
    ] {
        assert!(matches!(r, Err(DataError::NotFound)), "{r:?}");
    }
    s.purge_tenant(&f.tenant).await.unwrap();
}

/// ⑫-2 行述語がある表では、追加だけの人は「自分が作った行 AND 行述語」だけを読める（積）。
#[tokio::test]
async fn add_only_is_intersected_with_the_row_policy() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let (alice, bob) = (ctx(&tenant, "alice"), ctx(&tenant, "bob"));
    let store = DataStore::new(pool, Arc::clone(&authz), Arc::new(AllowRefs));
    let mut schema = requests_schema();
    // 行述語: 状態が「申請中」の行だけ（全員に同じ）。
    schema.row_policy = Some(RowPolicy {
        read: PolicyExpr::FieldCmp {
            field: "status".into(),
            op: CmpOp::Eq,
            value: PolicyOperand::Lit(json!("申請中")),
        },
        write: None,
    });
    let t = store
        .create_table(
            &alice,
            NewDataTable {
                name: "requests".into(),
                schema,
            },
            None,
        )
        .await
        .unwrap();
    grant(&authz, &bob, Relation::Submitter, t.id).await;
    for (who, title, status) in [
        (&bob, "bob-open", "申請中"),
        (&bob, "bob-done", "承認済み"),
        (&alice, "alice-open", "申請中"),
    ] {
        store
            .create_record(who, t.id, json!({"title": title, "status": status}), None)
            .await
            .unwrap();
    }
    let r = store
        .run_query(&bob, t.id, &DataQuery::default(), None)
        .await
        .unwrap();
    assert_eq!(
        titles(&r),
        vec!["bob-open"],
        "自分の行のうち行述語を満たすものだけ"
    );
    assert_eq!(
        store.count_records(&bob, t.id, None, None).await.unwrap(),
        1
    );
    let r = store
        .run_query(&alice, t.id, &DataQuery::default(), None)
        .await
        .unwrap();
    assert_eq!(
        titles(&r),
        vec!["alice-open", "bob-open"],
        "持ち主は行述語どおり"
    );
    // 同じ形のクエリ（条件・並べ替えつき）を、絞った述語と元の述語で交互に流してもプランキャッシュが
    // 取り違えない（同じスキーマの世代で述語の形だけが違う）。
    let shaped: DataQuery = serde_json::from_value(json!({
        "filter": {"cmp": {"field": "title", "op": "starts_with", "value": "bob"}},
        "sort": [{"field": "amount", "descending": true}],
        "with_count": true,
    }))
    .unwrap();
    for who in [&bob, &alice, &bob, &alice] {
        let r = store.run_query(who, t.id, &shaped, None).await.unwrap();
        assert_eq!(titles(&r), vec!["bob-open"]);
    }
    let st = store.plan_cache_stats();
    assert_eq!((st.mismatches, st.verify_failures), (0, 0), "{st:?}");
    store.purge_tenant(&tenant).await.unwrap();
}

/// ⑫-3 リンク先の表示・ルックアップ・ロールアップ・リンクの半結合・逆向きのリンクで、追加だけの表の
/// 他人の行を辿れない（参照先の行を読む入口は 1 つで、同じ絞り込みが効く）。
#[tokio::test]
async fn add_only_reference_rows_never_leak_through_links() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let (alice, bob) = (ctx(&tenant, "alice"), ctx(&tenant, "bob"));
    let store = DataStore::new(pool, Arc::clone(&authz), Arc::new(AllowRefs));
    let mut name = field("name", FieldType::Text);
    name.indexed = true;
    let lines = store
        .create_table(
            &alice,
            NewDataTable {
                name: "lines".into(),
                schema: plain(vec![name, field("amount", FieldType::Number)]),
            },
            None,
        )
        .await
        .unwrap();
    let orders = store
        .create_table(
            &alice,
            NewDataTable {
                name: "orders".into(),
                schema: plain(derived_fields(lines.id)),
            },
            None,
        )
        .await
        .unwrap();
    // bob は lines に追加だけ、orders は閲覧と追加。
    grant(&authz, &bob, Relation::Submitter, lines.id).await;
    grant(&authz, &bob, Relation::Viewer, orders.id).await;
    grant(&authz, &bob, Relation::Submitter, orders.id).await;
    let l_alice = store
        .create_record(
            &alice,
            lines.id,
            json!({"name": "secret-line", "amount": 9_000_001}),
            None,
        )
        .await
        .unwrap()
        .id;
    let l_bob = store
        .create_record(
            &bob,
            lines.id,
            json!({"name": "bob-line", "amount": 50}),
            None,
        )
        .await
        .unwrap()
        .id;
    // bob は他人の行をリンクに持ち込めない（参照先が見えない＝無いと同じ応答）。自分の行は持ち込める。
    let hidden = store
        .create_record(&bob, orders.id, json!({"items": ids(&[l_alice])}), None)
        .await;
    let missing = store
        .create_record(
            &bob,
            orders.id,
            json!({"items": ids(&[Uuid::new_v4()])}),
            None,
        )
        .await;
    assert!(hidden.is_err(), "{hidden:?}");
    assert_eq!(format!("{hidden:?}"), format!("{missing:?}"));
    store
        .create_record(&bob, orders.id, json!({"items": ids(&[l_bob])}), None)
        .await
        .unwrap();
    let o = store
        .create_record(
            &alice,
            orders.id,
            json!({"items": ids(&[l_alice, l_bob])}),
            None,
        )
        .await
        .unwrap();
    let got = store.get_record(&bob, orders.id, o.id, None).await.unwrap();
    assert_eq!(got.data["total"], json!(50), "ロールアップは自分の行だけ");
    assert_eq!(got.data["n"], json!(1), "件数も自分の行だけ");
    assert_eq!(
        got.data["names"],
        json!(["bob-line"]),
        "ルックアップも自分の行だけ"
    );
    assert_eq!(got.data["doubled"], json!(100), "数式越しにも自分の行だけ");
    assert_eq!(
        got.data["items"],
        ids(&[l_bob]),
        "リンク先の表示に他人の行の id を出さない"
    );
    let alice_view = store
        .get_record(&alice, orders.id, o.id, None)
        .await
        .unwrap();
    assert_eq!(alice_view.data["total"], json!(9_000_051));
    // リンクの半結合で他人の行の値を当てられない（持ち主には当たる）。
    let probe: DataQuery = serde_json::from_value(json!({"filter": {"link": {
        "field": "items",
        "filter": {"cmp": {"field": "name", "op": "eq", "value": "secret-line"}}
    }}}))
    .unwrap();
    let r = store
        .run_query(&bob, orders.id, &probe, None)
        .await
        .unwrap();
    assert!(r.items.unwrap().is_empty(), "半結合で他人の行に当たらない");
    let r = store
        .run_query(&alice, orders.id, &probe, None)
        .await
        .unwrap();
    assert_eq!(r.items.unwrap().len(), 1);
    store.purge_tenant(&tenant).await.unwrap();
}

/// ⑫-4 SSE: 追加だけの購読者には自分が作った行の変更だけが届き、他人の行は id も値も流れない。
/// 共有を取り消すと購読は閉じる。
#[tokio::test]
async fn add_only_sse_streams_only_own_records() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let f = add_only_fixture(pool, Arc::clone(&authz)).await;
    let (s, t) = (&f.store, f.table);
    let mut stream = s.subscribe_records(&f.bob, t, None, None).await.unwrap();
    // 閲覧者（dave）の購読も同じ表で並べる（絞り込みが混ざらない）。
    let mut dave_stream = s.subscribe_records(&f.dave, t, None, None).await.unwrap();
    let hidden = s
        .create_record(
            &f.alice,
            t,
            json!({"title": "alice-sse-secret", "amount": 4_242_001}),
            None,
        )
        .await
        .unwrap();
    let mine = s
        .create_record(&f.bob, t, json!({"title": "bob-sse"}), None)
        .await
        .unwrap();
    // 持ち主が bob の行を書き換える（承認など）と、bob にも届く。他人の行の書き換えは届かない。
    s.update_record(
        &f.alice,
        t,
        mine.id,
        json!({"memo": "承認しました"}),
        1,
        None,
    )
    .await
    .unwrap();
    s.update_record(
        &f.alice,
        t,
        hidden.id,
        json!({"memo": "alice-sse-hidden-memo"}),
        1,
        None,
    )
    .await
    .unwrap();
    let marker = s
        .create_record(&f.bob, t, json!({"title": "bob-sse-last"}), None)
        .await
        .unwrap();
    let mut seen: Vec<String> = Vec::new();
    let deadline = std::time::Instant::now() + SSE_WAIT;
    let mut got_last = false;
    while std::time::Instant::now() < deadline && !got_last {
        if let Ok(Some(m)) =
            tokio::time::timeout(std::time::Duration::from_millis(500), stream.next()).await
        {
            if let data::StreamEvent::Upserted(u) = &m.event {
                // 購読の前に作った自分の行（f.b）は、配信が追いついた後で届くことがある。
                assert!(
                    [f.b, mine.id, marker.id].contains(&u.record.id),
                    "他人の行は届かない: {:?}",
                    u.record.id
                );
                assert_eq!(u.record.owner, "bob");
                got_last |= u.record.id == marker.id;
            }
            seen.push(format!("{} {}", m.event.name(), m.event.data_json()));
        }
    }
    assert!(got_last, "自分の行は届く: {seen:?}");
    let all = seen.join("\n");
    assert!(all.contains("承認しました"), "自分の行の更新は届く: {all}");
    for leak in [
        hidden.id.to_string(),
        f.a.to_string(),
        f.c.to_string(),
        "alice-sse-secret".into(),
        "alice-sse-hidden-memo".into(),
        "4242001".into(),
        "carol-secret".into(),
    ] {
        assert!(!all.contains(&leak), "{leak} が SSE に出た: {all}");
    }
    // 閲覧者には他人の行も届く（絞り込みが閲覧者へ移らない）。
    let mut dave_saw_hidden = false;
    let deadline = std::time::Instant::now() + SSE_WAIT;
    while std::time::Instant::now() < deadline && !dave_saw_hidden {
        if let Ok(Some(m)) =
            tokio::time::timeout(std::time::Duration::from_millis(500), dave_stream.next()).await
        {
            if let data::StreamEvent::Upserted(u) = &m.event {
                dave_saw_hidden |= u.record.id == hidden.id;
            }
        }
    }
    assert!(dave_saw_hidden, "閲覧者には届く");
    assert_eq!(s.stream_verify_drops(), 0);
    // 取り消すと、次のバッチで購読ごと閉じる（authz_changed）。
    s.unshare_table(&f.alice, t, &user("bob"), TableShareRole::Submitter, None)
        .await
        .unwrap();
    let mut reset = false;
    let deadline = std::time::Instant::now() + SSE_WAIT;
    while std::time::Instant::now() < deadline && !reset {
        match tokio::time::timeout(std::time::Duration::from_millis(500), stream.next()).await {
            Ok(Some(m)) => {
                if let data::StreamEvent::Reset(_) = &m.event {
                    reset = true;
                }
            }
            Ok(None) => reset = true,
            Err(_) => {}
        }
    }
    assert!(reset, "取り消した購読は閉じる");
    s.purge_tenant(&f.tenant).await.unwrap();
}

/// ⑫-5 材料化（§7.4）: 参照元の追加だけの人は参照元の読み手に数える（参照先を読めなければ材料化を
/// 破棄する）。参照先の追加だけの人は参照先の読み手に数えない。追加だけの人の読取は、絞った行述語を
/// 理由に材料化を破棄しない。
#[tokio::test]
async fn add_only_counts_toward_materialization_containment() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    // 参照元（orders）の追加だけ → bob は lines を読めない → 包含が崩れて破棄。
    let m = materialized_fixture(pool.clone(), Arc::clone(&authz)).await;
    grant(&m.authz, &m.bob, Relation::Submitter, m.orders).await;
    m.store.after_table_rebac_changed(&m.alice, m.orders).await;
    m.assert_discarded("参照元の追加だけ").await;

    // 参照先（lines）の追加だけ・参照元の閲覧 → bob は lines の全行を読めない → 破棄。bob には 0 件の集計。
    let m = materialized_fixture(pool.clone(), Arc::clone(&authz)).await;
    grant(&m.authz, &m.bob, Relation::Submitter, m.lines).await;
    grant(&m.authz, &m.bob, Relation::Viewer, m.orders).await;
    m.store.after_table_rebac_changed(&m.alice, m.orders).await;
    m.assert_discarded("参照先の追加だけ").await;
    assert_eq!(m.total_seen_by(&m.bob).await, serde_json::Value::Null);
    assert_eq!(m.total_seen_by(&m.alice).await, json!(350));

    // 参照元の追加だけ ＋ 参照先の閲覧 → 包含は保たれる。bob の読取（絞った行述語）で破棄しない。
    let m = materialized_fixture(pool, authz).await;
    grant(&m.authz, &m.bob, Relation::Submitter, m.orders).await;
    grant(&m.authz, &m.bob, Relation::Viewer, m.lines).await;
    m.store.after_table_rebac_changed(&m.alice, m.orders).await;
    m.store.after_table_rebac_changed(&m.alice, m.lines).await;
    assert_eq!(m.materialized_rows().await, 2, "包含は保たれる");
    let mine = m
        .store
        .create_record(&m.bob, m.orders, json!({"items": ids(&m.line_ids)}), None)
        .await
        .unwrap();
    let r = m
        .store
        .run_query(&m.bob, m.orders, &m.by_total(), None)
        .await
        .unwrap();
    let got: Vec<Uuid> = r.items.unwrap().iter().map(|x| x.id).collect();
    assert_eq!(got, vec![mine.id], "自分の行だけを材料化列で並べる");
    assert!(
        m.materialized_rows().await >= 2,
        "追加だけの人の読取で材料化を破棄しない"
    );
}

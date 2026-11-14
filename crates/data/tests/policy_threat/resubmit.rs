//! ⑭ 「追加だけ」の人の出し直し（D-127・GAP-52）の脅威モデル。
//!
//! 状態の進め方で作成者に許した進め方（差し戻し → 申請中）と、そのあいだ作成者の書き換えを許す書き込みの決まり
//! （状態の進め方の画面の決まり・`fsm_write_guard`）があると、追加だけの人は**自分が作成者のレコード**を、差し戻しの
//! あいだ直して申請中へ進められる。それ以外は今のまま断る:
//!
//! - 他人のレコード（見えない行は「無い」と同じ 404・個別に共有された行は 403）
//! - 作成者に許していない状態（最初の状態・承認の後）の書き換え、作成者に許していない進め方
//! - 削除・一括取込・作成者の列と状態の列の書き換え
//! - 作成者の列の無いテーブル・メンバーの列で本人を指すテーブル（レコードを引く前に表の権限で 403）
//!
//! テストで作った行・状態の進め方の定義・OpenFGA のストアは、テストの終わりに消す。

use super::*;

use data::{
    BulkImportOptions, BulkImportRow, BulkUpsertRow, FsmBody, FsmStore, FsmTransition, InvalidCode,
    SaveTableFsm, TableShareRole,
};
use storage::ShareTarget;

use super::created_by::{delete_store, setup_cleaned};

fn user(id: &str) -> ShareTarget {
    ShareTarget::User { id: id.into() }
}

fn me_in(field: &str, op: CmpOp) -> PolicyExpr {
    PolicyExpr::FieldCmp {
        field: field.into(),
        op,
        value: PolicyOperand::UserId,
    }
}

const STATES: [&str; 4] = ["申請中", "承認", "却下", "差し戻し"];

/// 申請の表: 件名・金額・承認者（メンバーの列）・申請者（メンバーの列）・作成者の列（任意）・状態。
fn requests_schema(with_creator: bool) -> TableSchema {
    let mut title = field("title", FieldType::Text);
    title.indexed = true;
    let mut amount = field("amount", FieldType::Number);
    amount.indexed = true;
    let mut approver = field("approver", FieldType::UserRef);
    approver.indexed = true;
    let mut applicant = field("applicant", FieldType::UserRef);
    applicant.indexed = true;
    let mut status = field("status", FieldType::Select);
    status.options = STATES.iter().map(|s| (*s).to_string()).collect();
    status.indexed = true;
    let mut fields = vec![title, amount, approver, applicant, status];
    if with_creator {
        let mut made_by = field("made_by", FieldType::CreatedBy);
        made_by.display_name = "作成者".into();
        fields.push(made_by);
    }
    plain(fields)
}

/// 承認者が承認・却下・差し戻し（申請した本人は除く）、`resubmitter` が差し戻し → 申請中、承認者だけが差し戻し → 却下。
fn resubmit_flow(applicant: &str, resubmitter: PolicyExpr) -> FsmBody {
    let approver = PolicyExpr::All(vec![
        me_in("approver", CmpOp::Eq),
        me_in(applicant, CmpOp::Ne),
    ]);
    let mv = |from: &str, to: &str, actor: PolicyExpr| FsmTransition {
        from: from.into(),
        to: to.into(),
        actor,
    };
    FsmBody {
        states: STATES.iter().map(|s| (*s).to_string()).collect(),
        transitions: vec![
            mv("申請中", "承認", approver.clone()),
            mv("申請中", "却下", approver.clone()),
            mv("申請中", "差し戻し", approver.clone()),
            mv("差し戻し", "申請中", resubmitter),
            mv("差し戻し", "却下", me_in("approver", CmpOp::Eq)),
        ],
    }
}

/// 持ち主の alice が表を作り、状態の進め方の画面と同じ入口で状態の進め方を付ける（書き込みの決まりは申請者の列）。
/// bob・carol に追加だけ、dave に編集者（承認者）を渡す。
async fn resubmit_table(
    s: &DataStore,
    fsms: &FsmStore,
    alice: &AuthContext,
    with_creator: bool,
    applicant: &str,
    resubmitter: PolicyExpr,
) -> Uuid {
    let t = s
        .create_table(
            alice,
            NewDataTable {
                name: format!("resubmit-{}", Uuid::new_v4().simple()),
                schema: requests_schema(with_creator),
            },
            None,
        )
        .await
        .unwrap();
    fsms.save_for_table(
        alice,
        t.id,
        &SaveTableFsm {
            status_field: "status".into(),
            body: resubmit_flow(applicant, resubmitter),
            applicant_write_lock: Some(applicant.into()),
            expected_schema_version: t.schema_version,
        },
        None,
    )
    .await
    .unwrap();
    for (who, role) in [
        ("bob", TableShareRole::Submitter),
        ("carol", TableShareRole::Submitter),
        ("dave", TableShareRole::Editor),
    ] {
        s.share_table(alice, t.id, &user(who), role, None)
            .await
            .unwrap();
    }
    t.id
}

async fn rev_of(s: &DataStore, who: &AuthContext, t: Uuid, id: Uuid) -> i64 {
    s.get_record(who, t, id, None).await.unwrap().rev
}

fn is_code(r: &Result<data::DataRecord, DataError>, code: InvalidCode) -> bool {
    matches!(r, Err(e) if e.invalid_code() == Some(code))
}

/// ⑭-1 追加だけの bob は、差し戻された自分の申請を直して申請中へ出し直せる。作成者に許していない状態・進め方・
/// 他人のレコード・削除・取込・作成者の列と状態の列の書き換えは断る。
#[tokio::test]
async fn add_only_creator_fixes_and_resubmits_only_own_sent_back_records() {
    let Some((pool, authz, base_url, store_id)) = setup_cleaned().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let (alice, bob, carol, dave) = (
        ctx(&tenant, "alice"),
        ctx(&tenant, "bob"),
        ctx(&tenant, "carol"),
        ctx(&tenant, "dave"),
    );
    let s = DataStore::new(pool.clone(), Arc::clone(&authz), Arc::new(AllowRefs));
    let artifacts = Arc::new(artifact::ArtifactStore::new(
        pool.clone(),
        Arc::clone(&authz),
    ));
    let fsms = FsmStore::new(artifacts, s.clone());
    let t = resubmit_table(
        &s,
        &fsms,
        &alice,
        true,
        "made_by",
        me_in("made_by", CmpOp::Eq),
    )
    .await;
    let new = |title: &str| json!({"title": title, "amount": 100, "approver": "dave"});
    let b1 = s
        .create_record(&bob, t, new("bob の出張"), None)
        .await
        .unwrap();
    let c1 = s
        .create_record(&carol, t, new("carol の出張"), None)
        .await
        .unwrap();
    assert_eq!(b1.data["status"], json!("申請中"));

    // 申請中（最初の状態）: 追加だけの人は書き換えも進めることもできない（作成者に許した進め方が無い状態）。
    let r = s
        .update_record(&bob, t, b1.id, json!({"amount": 1}), b1.rev, None)
        .await;
    assert!(
        matches!(r, Err(DataError::Forbidden)),
        "申請中の書き換え: {r:?}"
    );
    for to in ["承認", "差し戻し"] {
        let r = fsms.transition(&bob, t, b1.id, to, b1.rev, None).await;
        assert!(
            matches!(r, Err(DataError::Forbidden)),
            "申請中 → {to}: {r:?}"
        );
    }
    let a = fsms
        .allowed_transitions(&bob, t, b1.id, None)
        .await
        .unwrap();
    assert!(a.transitions.is_empty(), "{a:?}");
    // 画面の手がかり: 差し戻しから進められ、差し戻しのあいだ直せる。
    let vc = s.table_viewer_context(&bob, t, None).await.unwrap();
    let rights = vc.own_records.expect("作成者に許したもの");
    assert_eq!(rights.move_from, vec!["差し戻し".to_string()]);
    assert_eq!(rights.edit_in, vec!["差し戻し".to_string()]);
    assert!(
        s.table_viewer_context(&dave, t, None)
            .await
            .unwrap()
            .own_records
            .is_none(),
        "追加だけでない人には出さない"
    );

    // 承認者 dave が 2 件とも差し戻す。
    let b1 = fsms
        .transition(&dave, t, b1.id, "差し戻し", b1.rev, None)
        .await
        .unwrap();
    let c1 = fsms
        .transition(&dave, t, c1.id, "差し戻し", c1.rev, None)
        .await
        .unwrap();

    // 差し戻し: 進め先の候補は申請中だけ（承認者だけの差し戻し → 却下は出ない）。
    let a = fsms
        .allowed_transitions(&bob, t, b1.id, None)
        .await
        .unwrap();
    assert_eq!(a.transitions, vec!["申請中".to_string()], "{a:?}");
    let r = fsms.transition(&bob, t, b1.id, "却下", b1.rev, None).await;
    assert!(
        matches!(r, Err(DataError::Forbidden)),
        "差し戻し → 却下: {r:?}"
    );
    // 作成者の列と状態の列は書けない。削除・一括取込（JSON・CSV・更新行）はできない。
    let r = s
        .update_record(&bob, t, b1.id, json!({"made_by": "alice"}), b1.rev, None)
        .await;
    assert!(is_code(&r, InvalidCode::InvalidInput), "作成者の列: {r:?}");
    let r = s
        .update_record(&bob, t, b1.id, json!({"status": "申請中"}), b1.rev, None)
        .await;
    assert!(is_code(&r, InvalidCode::FsmInvalid), "状態の列: {r:?}");
    let r = s.delete_record(&bob, t, b1.id, b1.rev, None).await;
    assert!(matches!(r, Err(DataError::Forbidden)), "削除: {r:?}");
    let r = s
        .start_bulk_import(
            &bob,
            t,
            vec![BulkImportRow {
                key: None,
                data: json!({"title": "bulk"}),
            }],
            BulkImportOptions::default(),
            None,
        )
        .await;
    assert!(matches!(r, Err(DataError::Forbidden)), "取込: {r:?}");
    let r = s
        .start_bulk_import_csv(
            &bob,
            t,
            b"title\nbulk\n",
            BulkImportOptions::default(),
            None,
        )
        .await;
    assert!(matches!(r, Err(DataError::Forbidden)), "CSV の取込: {r:?}");
    let r = s
        .start_bulk_upsert(
            &bob,
            t,
            vec![BulkUpsertRow {
                key: None,
                record_id: Some(b1.id),
                expected_rev: Some(b1.rev),
                data: json!({"amount": 5}),
            }],
            BulkImportOptions::default(),
            None,
        )
        .await;
    assert!(matches!(r, Err(DataError::Forbidden)), "一括の更新: {r:?}");

    // 他人（carol）の差し戻しのレコード: 見えない行は「無い」と同じ（書き換え・進める・候補）。
    let missing = Uuid::new_v4();
    for id in [c1.id, missing] {
        let r = s
            .update_record(&bob, t, id, json!({"amount": 9}), c1.rev, None)
            .await;
        assert!(
            matches!(r, Err(DataError::NotFound)),
            "他人の行の書き換え {id}: {r:?}"
        );
        let r = fsms.transition(&bob, t, id, "申請中", c1.rev, None).await;
        assert!(
            matches!(r, Err(DataError::NotFound)),
            "他人の行を進める {id}: {r:?}"
        );
        let r = fsms.allowed_transitions(&bob, t, id, None).await;
        assert!(
            matches!(r, Err(DataError::NotFound)),
            "他人の行の候補 {id}: {r:?}"
        );
    }
    // 個別に共有されて見える他人の行でも、作成者ではないので書き換えも進めることもできない。
    s.share_record(
        &alice,
        t,
        c1.id,
        &user("bob"),
        RecordShareRole::Viewer,
        None,
    )
    .await
    .unwrap();
    let r = s
        .update_record(&bob, t, c1.id, json!({"amount": 9}), c1.rev, None)
        .await;
    assert!(
        matches!(r, Err(DataError::Forbidden)),
        "共有された他人の行: {r:?}"
    );
    let r = fsms
        .transition(&bob, t, c1.id, "申請中", c1.rev, None)
        .await;
    assert!(
        matches!(r, Err(DataError::Forbidden)),
        "共有された他人の行を進める: {r:?}"
    );
    let a = fsms
        .allowed_transitions(&bob, t, c1.id, None)
        .await
        .unwrap();
    assert!(a.transitions.is_empty(), "{a:?}");

    // 自分の差し戻しのレコードは直せる（作成者は変わらない）。直したら申請中へ出し直せる。
    let fixed = s
        .update_record(
            &bob,
            t,
            b1.id,
            json!({"amount": 120, "title": "bob の出張（直した）"}),
            b1.rev,
            None,
        )
        .await
        .unwrap();
    assert_eq!(fixed.data["amount"], json!(120));
    assert_eq!(fixed.data["made_by"], json!("bob"));
    let again = fsms
        .transition(&bob, t, b1.id, "申請中", fixed.rev, None)
        .await
        .unwrap();
    assert_eq!(again.data["status"], json!("申請中"));
    // 出し直した後（申請中）はもう直せない。読めるのは今までどおり自分の行と共有された行だけ。
    let r = s
        .update_record(&bob, t, b1.id, json!({"amount": 1}), again.rev, None)
        .await;
    assert!(
        matches!(r, Err(DataError::Forbidden)),
        "出し直した後: {r:?}"
    );
    let all = s
        .run_query(&bob, t, &DataQuery::default(), None)
        .await
        .unwrap();
    let mut seen: Vec<String> = all
        .items
        .unwrap()
        .iter()
        .map(|i| i.data["title"].as_str().unwrap_or_default().to_string())
        .collect();
    seen.sort();
    assert_eq!(seen, vec!["bob の出張（直した）", "carol の出張"]);

    // 承認の後は何もできない（作成者に許した進め方の無い状態）。
    let done = fsms
        .transition(&dave, t, b1.id, "承認", again.rev, None)
        .await
        .unwrap();
    let r = s
        .update_record(&bob, t, b1.id, json!({"amount": 1}), done.rev, None)
        .await;
    assert!(
        matches!(r, Err(DataError::Forbidden)),
        "承認の後の書き換え: {r:?}"
    );
    // 承認からの進め方は定義に無い（編集者と同じく定義の外は 400）。候補も無い。
    let r = fsms
        .transition(&bob, t, b1.id, "差し戻し", done.rev, None)
        .await;
    assert!(
        is_code(&r, InvalidCode::FsmInvalid),
        "承認の後に進める: {r:?}"
    );
    let a = fsms
        .allowed_transitions(&bob, t, b1.id, None)
        .await
        .unwrap();
    assert!(a.transitions.is_empty(), "{a:?}");

    // 編集者も、作成者なら差し戻しのあいだ直せる（同じ書き込みの決まり・これまでは最初の状態のあいだだけ）。
    // 作成者でない編集者は今までどおり直せる。
    let d1 = s
        .create_record(
            &dave,
            t,
            json!({"title": "dave の出張", "approver": "alice"}),
            None,
        )
        .await
        .unwrap();
    let d1 = fsms
        .transition(&alice, t, d1.id, "差し戻し", d1.rev, None)
        .await
        .unwrap();
    let r = s
        .update_record(&dave, t, d1.id, json!({"amount": 140}), d1.rev, None)
        .await;
    assert!(r.is_ok(), "差し戻しの自分の行（編集者）: {r:?}");
    let c1 = s.get_record(&dave, t, c1.id, None).await.unwrap();
    let r = s
        .update_record(&dave, t, c1.id, json!({"amount": 130}), c1.rev, None)
        .await;
    assert!(r.is_ok(), "作成者でない編集者: {r:?}");
    // 決まりが止めるのは申請者だけ: 作成者でない編集者は、承認の後もこれまでどおり直せる。
    let r = s
        .update_record(&dave, t, b1.id, json!({"amount": 1}), done.rev, None)
        .await;
    assert!(r.is_ok(), "作成者でない編集者は承認の後も直せる: {r:?}");

    s.purge_tenant(&tenant).await.unwrap();
    sqlx::query("DELETE FROM artifact WHERE tenant_id = $1")
        .bind(&tenant)
        .execute(&pool)
        .await
        .unwrap();
    delete_store(&base_url, &store_id).await;
}

/// ⑭-2 作成者の列の無いテーブル・メンバーの列で本人を指すテーブル・is_owner の進め方では、追加だけの人は今のまま
/// 何もできない（レコードを引く前に表の権限で 403・無いレコードでも同じ）。
#[tokio::test]
async fn add_only_gets_nothing_without_a_creator_column_rule() {
    let Some((pool, authz, base_url, store_id)) = setup_cleaned().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let (alice, bob, dave) = (
        ctx(&tenant, "alice"),
        ctx(&tenant, "bob"),
        ctx(&tenant, "dave"),
    );
    let s = DataStore::new(pool.clone(), Arc::clone(&authz), Arc::new(AllowRefs));
    let artifacts = Arc::new(artifact::ArtifactStore::new(
        pool.clone(),
        Arc::clone(&authz),
    ));
    let fsms = FsmStore::new(artifacts, s.clone());
    let cases = [
        // メンバーの列（申請者）で本人を指す。
        ("member", false, "applicant", me_in("applicant", CmpOp::Eq)),
        // 作成者の列はあるが、進め方は is_owner（作成者の列で本人を指さない）。
        ("is_owner", true, "made_by", PolicyExpr::IsOwner),
        // 作成者の列はあるが、出し直しはメンバーの列の申請者だけ。
        (
            "creator-col-unused",
            true,
            "made_by",
            me_in("applicant", CmpOp::Eq),
        ),
    ];
    let mut outcomes = Vec::new();
    for (name, with_creator, applicant, resubmitter) in cases {
        let t = resubmit_table(&s, &fsms, &alice, with_creator, applicant, resubmitter).await;
        let b1 = s
            .create_record(
                &bob,
                t,
                json!({"title": name, "approver": "dave", "applicant": "bob"}),
                None,
            )
            .await
            .unwrap();
        let back = fsms
            .transition(&dave, t, b1.id, "差し戻し", b1.rev, None)
            .await
            .unwrap();
        let vc = s.table_viewer_context(&bob, t, None).await.unwrap();
        let update = s
            .update_record(&bob, t, b1.id, json!({"amount": 1}), back.rev, None)
            .await;
        let moved = fsms
            .transition(&bob, t, b1.id, "申請中", back.rev, None)
            .await;
        let missing = fsms
            .transition(&bob, t, Uuid::new_v4(), "申請中", 1, None)
            .await;
        let candidates = fsms.allowed_transitions(&bob, t, b1.id, None).await;
        let still = rev_of(&s, &dave, t, b1.id).await;
        outcomes.push((
            name,
            vc.own_records,
            update,
            moved,
            missing,
            candidates,
            still,
            back.rev,
        ));
    }
    s.purge_tenant(&tenant).await.unwrap();
    sqlx::query("DELETE FROM artifact WHERE tenant_id = $1")
        .bind(&tenant)
        .execute(&pool)
        .await
        .unwrap();
    delete_store(&base_url, &store_id).await;

    for (name, rights, update, moved, missing, candidates, still, rev) in outcomes {
        assert_eq!(rights, None, "{name}");
        assert!(
            matches!(update, Err(DataError::Forbidden)),
            "{name}: {update:?}"
        );
        assert!(
            matches!(moved, Err(DataError::Forbidden)),
            "{name}: {moved:?}"
        );
        assert!(
            matches!(missing, Err(DataError::Forbidden)),
            "{name}: {missing:?}"
        );
        assert!(
            matches!(candidates, Err(DataError::Forbidden)),
            "{name}: {candidates:?}"
        );
        assert_eq!(still, rev, "{name}: 書き換わっていない");
    }
}

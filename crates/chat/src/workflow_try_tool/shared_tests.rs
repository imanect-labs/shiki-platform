//! 共有の相手がいる会話の試運転の観測の単体テスト（PIT-84・D-214）。
//!
//! 要: 全部入りの材料（[`Observed`] と [`RunDigest`]）の全欄に目印を入れても、絞った形には 1 つも出ないこと。
//! 材料は構造体の字面で組む（`..Default::default()` を使わない）。全部入りの側に欄を足すとここが組めなくなるので、
//! 足した欄にも目印を入れて、この試験で漏れないことを確かめる。

use std::time::Duration;

use serde_json::json;
use uuid::Uuid;
use workflow_engine::run::{
    DigestError, DigestMapCount, DigestRecord, DigestSkip, DigestStep, RecordChange, RunDigest,
};
use workflow_engine::{FailureCategory, RunFailure, RunStatus, StepStatus, WaitingFor, WorkflowIr};

use super::super::observe::tests::{digest, ir, observed};
use super::super::observe::{self, Observed, Origin, RunCost, CLOSE, NOTICE, OPEN};
use super::super::wait::WaitEnd;
use super::*;

/// 作成者・外の相手・run から来る文字に入れる目印（どれも `SECRET` を含む）。
const MARK: &str = "SECRET";
/// 費用の目印（数字が出たら分かる値）。
const COST_TOKENS: i64 = 987_654;
const COST_MICROS: i64 = 4_321_000;

fn table_id() -> Uuid {
    Uuid::from_u128(0x5ec1_0000_0000_0000_0000_0000_0000_0011)
}

fn record_id() -> Uuid {
    Uuid::from_u128(0x5ec1_0000_0000_0000_0000_0000_0000_0012)
}

/// IR の文字の欄に目印を入れたもの（スケジュールのきっかけつき）。
fn marked_ir() -> WorkflowIr {
    serde_json::from_value(json!({
        "ir_version": 1,
        "name": "secret-flow",
        "display_name": "表示名SECRET-display",
        "description": "説明SECRET-description",
        "triggers": [ { "kind": "schedule", "cron": "0 9 * * *", "tz": "Asia/Tokyo" } ],
        "input_schema": {
            "type": "object",
            "properties": { "SECRET-input": { "type": "string", "description": "SECRET-input-desc" } }
        },
        "nodes": [
            { "id": "secret-ask", "type": "llm.invoke", "label": "ラベルSECRET-label", "params": { "prompt": "SECRET-prompt" } },
            { "id": "secret-after", "type": "storage.read", "label": "SECRET-after-label", "params": {} }
        ],
        "edges": [ { "from": "secret-ask", "to": "secret-after" } ]
    }))
    .unwrap()
}

/// run の要約の全欄に目印を入れたもの（字面で組む・欄を足したらここも直す）。
fn marked_digest(run_id: Uuid, status: RunStatus, waiting_for: Option<WaitingFor>) -> RunDigest {
    RunDigest {
        run_id,
        workflow_id: Uuid::from_u128(1),
        version: 3,
        status,
        waiting_for,
        failure: Some(RunFailure {
            category: FailureCategory::Permission,
            message: "失敗の理由SECRET-failure".into(),
            reference: "SECRET-ref".into(),
        }),
        steps: vec![
            DigestStep {
                node_id: "secret-ask".into(),
                node_type: Some("llm.invoke"),
                status: StepStatus::Failed,
                taken_ports: vec!["SECRET-port".into()],
                error: Some(DigestError {
                    code: Some("SECRET-code".into()),
                    message: Some("誤りの文SECRET-error".into()),
                }),
                output_head: Some("{\"text\":\"出力SECRET-output\"}".into()),
                output_truncated: true,
                skipped_because: vec![],
            },
            DigestStep {
                node_id: "secret-after".into(),
                node_type: Some("storage.read"),
                status: StepStatus::Skipped,
                taken_ports: vec![],
                error: None,
                output_head: None,
                output_truncated: false,
                skipped_because: vec![DigestSkip {
                    upstream: "secret-upstream".into(),
                    port: "SECRET-skip-port".into(),
                    upstream_status: StepStatus::Failed,
                    upstream_taken: vec!["SECRET-taken".into()],
                }],
            },
        ],
        map_counts: vec![DigestMapCount {
            map_node: "secret-map".into(),
            node_id: "secret-inner".into(),
            status: StepStatus::Succeeded,
            count: 3,
        }],
        records: vec![DigestRecord {
            node_id: "secret-add".into(),
            step_path: "secret-add/0".into(),
            table_id: table_id(),
            record_id: record_id(),
            change: RecordChange::Created,
        }],
        records_omitted: 4,
        finished_at: None,
    }
}

/// 全部入りの材料（字面で組む・欄を足したらここも直す）。
fn marked_observed<'a>(
    ir: &'a WorkflowIr,
    d: Option<&'a RunDigest>,
    origin: Origin,
    end: Option<WaitEnd>,
) -> Observed<'a> {
    Observed {
        origin,
        end,
        max_wait: Duration::from_secs(30),
        workflow_id: Uuid::from_u128(1),
        version: 3,
        run_id: d.map_or(Uuid::from_u128(2), |d| d.run_id),
        ir,
        digest: d,
        cost: RunCost {
            tokens: COST_TOKENS,
            cost_usd_micros: COST_MICROS,
        },
    }
}

fn shared_of(o: &Observed<'_>) -> String {
    render(&SharedFacts::from_observed(o))
}

/// 絞った形に、作成者・外の相手・run から来た文字、IR の中身、レコードの id、費用の数字が 1 つも無い。
fn assert_nothing_leaks(out: &str) {
    for leaked in [
        MARK,
        "secret",
        "llm.invoke",
        "storage.read",
        "（参照番号",
        "失敗の理由",
        OPEN,
        CLOSE,
        "run_output",
        NOTICE,
        "費用:",
        "トークン",
        "987654",
        "987,654",
        "4.3210",
        "$",
        "通らなかった理由",
        "繰り返しの内訳",
        "出口",
    ] {
        assert!(!out.contains(leaked), "{leaked} が出た: {out}");
    }
    for id in [table_id(), record_id()] {
        assert!(!out.contains(&id.to_string()), "{id} が出た: {out}");
        assert!(
            !out.contains(&id.simple().to_string()),
            "{id} が出た: {out}"
        );
    }
}

/// 全欄に目印を入れても絞った形には出ない。状態・run の id・版・件数・画面の道・注記はある。先頭の行は全部入りの
/// 先頭から費用を除いたもの（同じ `head_core`）。
#[test]
fn marked_fields_never_reach_the_shared_form() {
    let ir = marked_ir();
    let run_id = Uuid::from_u128(0xabc);
    let d = marked_digest(run_id, RunStatus::Failed, None);
    let o = marked_observed(&ir, Some(&d), Origin::Started, Some(WaitEnd::Settled));
    let full = observe::render(&o);
    // 空振りでない: 全部入りには目印が出る。
    for shown in [
        "SECRET-display",
        "SECRET-label",
        "SECRET-output",
        "SECRET-error",
        "SECRET-failure",
    ] {
        assert!(full.contains(shown), "全部入りに {shown} が無い: {full}");
    }
    assert!(full.contains(&record_id().to_string()), "{full}");
    assert!(full.contains("費用: 987654 トークン"), "{full}");

    let out = shared_of(&o);
    assert_nothing_leaks(&out);
    let head = out.lines().next().unwrap();
    assert_eq!(
        head,
        format!("試運転: 失敗 / run: {run_id} / 版: v3 / step: 2 件 / レコード: 5 件")
    );
    assert_eq!(
        head,
        observe::head_core(
            Origin::Started,
            Some((RunStatus::Failed, None)),
            run_id,
            3,
            Some((2, 5))
        )
    );
    assert!(full.lines().next().unwrap().starts_with(head), "{full}");
    assert!(
        out.contains(&format!(
            "\n画面: /workflows/{}/runs/{run_id}\n",
            Uuid::from_u128(1)
        )),
        "{out}"
    );
    assert!(out.contains(WITHHELD_NOTE), "{out}");
    assert!(out.contains(FAILED_SHARED), "{out}");
    assert!(!out.contains(observe::FAILED_FULL), "{out}");
    assert!(
        out.contains("レコード（5 件）は本物のテーブルに残ります"),
        "{out}"
    );
    assert!(out.contains(observe::RETRY_NOTE), "{out}");
    assert!(
        out.contains(SCHEDULE_NOTE_GENERAL) && out.contains(COST_NOTE_GENERAL),
        "{out}"
    );
}

/// 経路の印と添え書きは全部入りと同じ（合流・前回の結果）。
#[test]
fn origins_keep_the_mark_and_note() {
    let ir = marked_ir();
    let d = marked_digest(Uuid::from_u128(5), RunStatus::Running, None);
    let joined = shared_of(&marked_observed(
        &ir,
        Some(&d),
        Origin::Joined,
        Some(WaitEnd::TimedOut),
    ));
    assert!(
        joined.starts_with("試運転（実行中のものに合流）: 実行中 / run: "),
        "{joined}"
    );
    assert!(joined.contains(observe::JOINED_NOTE), "{joined}");
    assert!(
        joined.contains("まだ終わっていません（30 秒待ちました）"),
        "{joined}"
    );
    assert_nothing_leaks(&joined);
    let done = marked_digest(Uuid::from_u128(5), RunStatus::Succeeded, None);
    let previous = shared_of(&marked_observed(&ir, Some(&done), Origin::Previous, None));
    assert!(
        previous.starts_with("試運転（前回の結果）: 成功 / run: "),
        "{previous}"
    );
    assert!(previous.contains(observe::PREVIOUS_NOTE), "{previous}");
    assert!(previous.contains("\n終わりました。\n"), "{previous}");
    assert_nothing_leaks(&previous);
}

/// レコードが 0 件なら片付けの文は無い。スケジュールと費用の注記は、きっかけと費用に関わらず毎回出る。
#[test]
fn general_notes_are_always_there_and_cleanup_only_with_records() {
    // きっかけ無し・費用 0・レコード 0 件。
    let ir: WorkflowIr = serde_json::from_value(json!({
        "ir_version": 1,
        "name": "plain",
        "nodes": [ { "id": "a", "type": "storage.read", "params": {} } ],
        "edges": []
    }))
    .unwrap();
    let mut d = marked_digest(Uuid::from_u128(5), RunStatus::Succeeded, None);
    d.records.clear();
    d.records_omitted = 0;
    let mut o = marked_observed(&ir, Some(&d), Origin::Started, Some(WaitEnd::Settled));
    o.cost = RunCost::default();
    let full = observe::render(&o);
    assert!(
        !full.contains("委譲") && !full.contains("予算"),
        "全部入りは出し分ける: {full}"
    );
    let out = shared_of(&o);
    assert!(!out.contains(" / レコード:"), "{out}");
    assert!(!out.contains("本物のテーブルに残ります"), "{out}");
    assert!(!out.contains("消す手順"), "{out}");
    assert!(
        out.contains(SCHEDULE_NOTE_GENERAL) && out.contains(COST_NOTE_GENERAL),
        "{out}"
    );
    // 要約が読めないときは件数を付けない。
    let unknown = shared_of(&marked_observed(
        &ir,
        None,
        Origin::Started,
        Some(WaitEnd::Unknown),
    ));
    let head = unknown.lines().next().unwrap();
    assert_eq!(
        head,
        format!(
            "試運転: 状態を読めません / run: {} / 版: v3",
            Uuid::from_u128(2)
        )
    );
    assert!(
        unknown.contains("run の状態を読めませんでした"),
        "{unknown}"
    );
    assert!(unknown.contains(SCHEDULE_NOTE_GENERAL) && unknown.contains(COST_NOTE_GENERAL));
}

/// 状態ごとの案内の試験の 1 件（run の要約・待つのをやめた理由・案内に含まれるはずの文）。
type Case = (Option<RunDigest>, Option<WaitEnd>, &'static str);

/// 状態ごとの案内（順番待ち・待ち・停止・状態を読めない・保留の上限）にも、ブロックの名前や中身は出ない。
#[test]
fn guidance_per_state_names_no_block() {
    let ir = marked_ir();
    let run = Uuid::from_u128(5);
    let cases: Vec<Case> =
        vec![
        (
            Some(marked_digest(run, RunStatus::Queued, None)),
            Some(WaitEnd::TimedOut),
            "順番待ちです（同時に動かせる数の上限に達しています。30 秒待っても始まりませんでした）",
        ),
        (
            Some(marked_digest(run, RunStatus::Running, Some(WaitingFor::Approval))),
            Some(WaitEnd::Settled),
            "待っているところで止まっています",
        ),
        (
            Some(marked_digest(run, RunStatus::Running, None)),
            Some(WaitEnd::Cancelled),
            "run は取り消していません",
        ),
        (None, Some(WaitEnd::Unknown), "run の状態を読めませんでした"),
        (
            Some(marked_digest(run, RunStatus::Running, None)),
            Some(WaitEnd::ParkRefused),
            "実行中です。待てる数の上限に達しているので",
        ),
        (None, Some(WaitEnd::ParkRefused), "run の状態は読めませんでした"),
        (
            Some(marked_digest(run, RunStatus::Failed, None)),
            Some(WaitEnd::ParkRefused),
            FAILED_SHARED,
        ),
    ];
    for (d, end, expected) in &cases {
        let out = shared_of(&marked_observed(&ir, d.as_ref(), Origin::Started, *end));
        assert!(out.contains(expected), "{expected} が無い: {out}");
        assert_nothing_leaks(&out);
    }
}

/// 状態の案内は全部入りと同じ関数（失敗の枝だけが違う）。全部入りの試験の fixture でも同じ。
#[test]
fn guidance_matches_the_full_form_except_failure() {
    let ir = ir("確認");
    for (status, waiting, end) in [
        (RunStatus::Queued, None, Some(WaitEnd::TimedOut)),
        (
            RunStatus::Running,
            Some(WaitingFor::Event),
            Some(WaitEnd::Settled),
        ),
        (RunStatus::Succeeded, None, None),
        (RunStatus::Cancelled, None, Some(WaitEnd::Settled)),
    ] {
        let d = digest(Uuid::from_u128(5), status, waiting);
        let o = observed(&ir, Some(&d), Origin::Started, end);
        let guidance =
            observe::state_guidance(Some((status, waiting)), end, o.max_wait, FAILED_SHARED);
        assert!(observe::render(&o).contains(&guidance), "{guidance}");
        assert!(shared_of(&o).contains(&guidance), "{guidance}");
    }
}

/// run が無い道の文（名前を解決しない・最新の版を言わない・設定名を出さない・件数だけ）。
#[test]
fn runless_texts_carry_no_ir() {
    let named = named();
    assert!(named.starts_with("起動していません。"), "{named}");
    assert!(
        !named.contains("最新") && !named.contains("id は"),
        "{named}"
    );
    let none = no_such_version(9);
    assert!(none.starts_with("v9 はありません。"), "{none}");
    assert!(!none.contains("最新"), "{none}");
    assert!(NOT_LAUNCHED.starts_with("起動されませんでした"));
    assert!(!NOT_LAUNCHED.contains("policies") && !NOT_LAUNCHED.contains("on_trigger_overflow"));
    let wf = Uuid::from_u128(7);
    let refused = launch_refused(wf, 2, "入力の誤り", 3, INPUT_REFUSED_GUIDANCE);
    assert_eq!(
        refused,
        format!(
            "試運転: 起動できませんでした（入力の誤り 3 件） / 版: v2 / workflow_id: {wf}\n{INPUT_REFUSED_GUIDANCE}"
        )
    );
    assert!(
        !refused.contains(OPEN) && !refused.contains(NOTICE),
        "{refused}"
    );
}

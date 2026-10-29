//! 判定の評価のランナーの、普段のテストでも走る試験（実 LLM を呼ばない）: データの件数・凍結・指標の計算・
//! 採点し直し・裁定の材料・評価の設定の検査。

use std::collections::BTreeMap;

use llm_gateway::providers::llm_decider::PROMPT_VERSION;

use crate::data::{self, Dataset, DATASETS};
use crate::metrics::{self, RepMetrics, THETAS};
use crate::run::{self, CallResult};
use crate::{report, score, setup};

/// 正解をそのまま答えた（確からしさ `conf`）結果。
fn perfect(ds: &Dataset, split: &str, conf: f64) -> Vec<CallResult> {
    ds.rows
        .iter()
        .filter(|r| r.split == split)
        .map(|r| CallResult {
            dataset: ds.name.to_string(),
            id: r.id.clone(),
            rep: 1,
            latency_ms: 100,
            error: None,
            resolved_model: Some("m+llm_decider/v1/k1/low".into()),
            answers: r
                .labels
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        run::AnswerOut {
                            value: v.clone(),
                            confidence: conf,
                        },
                    )
                })
                .collect(),
        })
        .collect()
}

#[test]
fn datasets_have_the_designed_counts() {
    let inq = data::load("inquiry_triage");
    let exp = data::load("expense_policy");
    for ds in [&inq, &exp] {
        assert_eq!(ds.rows.len(), 120, "{}", ds.name);
        assert_eq!(
            ds.rows.iter().filter(|r| r.split == "dev").count(),
            40,
            "{}",
            ds.name
        );
        assert_eq!(
            ds.rows.iter().filter(|r| r.split == "test").count(),
            80,
            "{}",
            ds.name
        );
        let ids: std::collections::BTreeSet<_> = ds.rows.iter().map(|r| &r.id).collect();
        assert_eq!(ids.len(), 120, "{}: id が重複している", ds.name);
        // ラベルのキーは質問のキー（質問を足し引きしたらラベルも合わせる）。
        let keys: Vec<&str> = ds.questions.iter().map(|q| q.key.as_str()).collect();
        for r in &ds.rows {
            assert!(
                r.labels.keys().all(|k| keys.contains(&k.as_str())),
                "{}",
                r.id
            );
        }
    }
    let count = |cat: &str| {
        inq.rows
            .iter()
            .filter(|r| r.label_str("category") == Some(cat))
            .count()
    };
    assert_eq!(
        [
            count("billing"),
            count("bug"),
            count("howto"),
            count("cancel"),
            count("other")
        ],
        [24, 28, 28, 20, 20]
    );
    assert_eq!(
        inq.rows.iter().filter(|r| r.tag_bool("ambiguous")).count(),
        15
    );
    let injected: Vec<_> = inq
        .rows
        .iter()
        .filter(|r| r.tag_bool("injection"))
        .collect();
    assert_eq!(injected.len(), 6);
    for r in injected {
        let target = r
            .tag_str("inject_target")
            .expect("注入の行は指示した分類を持つ");
        assert_ne!(Some(target), r.label_str("category"), "{}", r.id);
    }
    let positives = exp
        .rows
        .iter()
        .filter(|r| r.label_bool("needs_review") == Some(true))
        .count();
    assert_eq!(positives, 48);
    let injected: Vec<_> = exp
        .rows
        .iter()
        .filter(|r| r.tag_bool("injection"))
        .collect();
    assert_eq!(injected.len(), 6);
    assert!(injected
        .iter()
        .all(|r| r.label_bool("needs_review") == Some(true)));
    // 規程のフラグが出る行は疑いあり（フラグだけで経理確認に回る）。
    assert!(exp
        .rows
        .iter()
        .filter(|r| !r.rule_flags.is_empty())
        .all(|r| r.label_bool("needs_review") == Some(true)));
}

#[test]
fn the_freeze_matches_the_questions_and_the_prompt() {
    // 凍結（frozen.json）は今の質問のファイルと同じで、しきい値は候補のどれか。質問を変えたら凍結し直す。
    let Some(f) = data::frozen() else { return };
    for name in DATASETS {
        let ds = data::load(name);
        assert_eq!(
            f.questions_sha256.get(name),
            Some(&ds.questions_sha256),
            "{name}"
        );
        let t = f.theta[name];
        assert!(THETAS.iter().any(|x| (x - t).abs() < 1e-9), "{name}: {t}");
    }
    // 凍結した版はコードの版と同じ（版を上げたら凍結し直す・PIT-80）。シナリオの版は workflow-engine の
    // decision_scenarios が frozen.json と照らす。
    assert_eq!(f.prompt_version, PROMPT_VERSION);
}

#[test]
fn version_mismatch_finds_every_other_version() {
    let f = frozen_v3();
    let v = |s: &[&str]| s.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();
    assert_eq!(
        data::version_mismatch(&v(&["m+llm_decider/v3/k1/low"]), Some(&f)),
        None
    );
    // 先頭が凍結の版でも、後ろに違う版があれば見つける。版の無い決定器（llm_decider でない）も違う版。
    for other in ["m+llm_decider/v4/k1/low", "jev-1.13.0"] {
        let got = data::version_mismatch(&v(&["m+llm_decider/v3/k1/low", other]), Some(&f));
        assert!(got.is_some_and(|m| m.contains(other)), "{other}");
    }
    // 凍結の無い段・答えの無い実行は確かめない。
    assert_eq!(
        data::version_mismatch(&v(&["m+llm_decider/v4/k1/low"]), None),
        None
    );
    assert_eq!(data::version_mismatch(&[], Some(&f)), None);
    // 答えた版は誤りの行を除いて重複なく集める。
    let mut calls = perfect(&data::load("inquiry_triage"), "dev", 0.9);
    calls[0].resolved_model = None;
    calls[1].resolved_model = Some("m+llm_decider/v4/k1/low".into());
    assert_eq!(
        data::resolved_models(&calls),
        v(&["m+llm_decider/v1/k1/low", "m+llm_decider/v4/k1/low"])
    );
}

#[test]
fn adjudicated_files_are_not_overwritten() {
    // 一時ファイルは target の下に置く（/tmp は tmpfs）。
    let path = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "adjudication-{}.jsonl",
        uuid::Uuid::new_v4().simple()
    ));
    let empty = r#"{"id":"a","adjudication":{"labels":null,"note":""}}"#;
    std::fs::write(&path, format!("{empty}\n")).unwrap();
    assert!(!report::has_adjudicated(&path));
    let filled = r#"{"id":"b","adjudication":{"labels":{"category":"bug"},"note":""}}"#;
    std::fs::write(&path, format!("{empty}\n{filled}\n")).unwrap();
    assert!(report::has_adjudicated(&path));
    std::fs::remove_file(&path).unwrap();
    assert!(!report::has_adjudicated(&path));
}

/// 凍結した段（test・反復 1 回）の結果の形（採点し直しの試験に使う）。
fn saved_result(ds: &Dataset, calls: Vec<CallResult>, theta: f64) -> report::ResultFile {
    let frozen = data::Frozen {
        prompt_version: "v1".into(),
        theta: [(ds.name.to_string(), theta)].into_iter().collect(),
        questions_sha256: [(ds.name.to_string(), ds.questions_sha256.clone())]
            .into_iter()
            .collect(),
        based_on: "E4".into(),
        notes: String::new(),
    };
    let datasets = vec![score::dataset_result(ds, &calls, 1, Some(theta), false)];
    report::ResultFile {
        label: "2000-01-01-E1".into(),
        stage: "E1".into(),
        date: "2000-01-01".into(),
        started_at: String::new(),
        finished_at: String::new(),
        model: "m".into(),
        resolved_model: None,
        resolved_models: data::resolved_models(&calls),
        split: "test".into(),
        reps: 1,
        effort: "low".into(),
        samples: 1,
        concurrency: 1,
        budget_usd: 0.0,
        estimate_usd: 0.0,
        relay_before_usd: 0.0,
        relay_after_usd: 0.0,
        relay_delta_usd: 0.0,
        db_rows: 0,
        db_cost_usd: 0.0,
        calls: calls.len(),
        cost_per_1000_usd: None,
        latency_p50_ms: None,
        latency_p95_ms: None,
        stopped: None,
        frozen: Some(frozen),
        datasets,
        datasets_sha256: BTreeMap::new(),
        rescored_from: None,
        results: calls,
    }
}

#[test]
fn rescore_scores_the_saved_answers_with_the_current_data() {
    // 採点し直しは、保存した指標を使わず、保存した答えと今のデータ（ラベル・規程のフラグ）から数え直す。
    let inq = data::load("inquiry_triage");
    let mut calls = perfect(&inq, "test", 0.95);
    for c in calls.iter_mut().take(4) {
        c.answers.get_mut("category").unwrap().value = "nonsense".into();
    }
    let mut saved = saved_result(&inq, calls.clone(), 0.6);
    let before = saved.datasets[0].reps[0].clone();
    let RepMetrics::Inquiry(m) = &before else {
        panic!()
    };
    assert_eq!(m.accuracy.k, 76);
    // 保存した指標（76 件）ではなく、結果の答えから数え直すことを、答えを正解に戻して確かめる。
    for c in saved.results.iter_mut().take(4) {
        let row = inq.row(&c.id).unwrap();
        c.answers.get_mut("category").unwrap().value = row.labels["category"].clone();
    }
    let again = score::rescore_file(saved.clone(), "2000-01-01-E1");
    let RepMetrics::Inquiry(m) = &again.datasets[0].reps[0] else {
        panic!()
    };
    assert_eq!(m.accuracy.k, 80);
    assert_eq!(again.label, "2000-01-01-E1-rescored");
    assert_eq!(again.rescored_from.as_deref(), Some("2000-01-01-E1"));
    assert_eq!(again.datasets_sha256["inquiry_triage"], inq.rows_sha256);
    assert!(again.datasets[0]
        .acceptance
        .as_ref()
        .is_some_and(|a| (a.theta - 0.6).abs() < 1e-12));
    // 裁定を書いたデータの合否は暫定ではない（問い合わせは 2026-10-28 に裁定した）。
    assert!(report::has_adjudicated(&report::adjudication_path(
        "inquiry_triage"
    )));
    assert!(again.datasets[0]
        .acceptance
        .as_ref()
        .is_some_and(|a| !a.provisional));
    assert!(again.datasets[0].suggested_theta.is_none());
    // 凍結の版と違う版の答えが混ざっていれば、合否を出さずに止めた印を付ける。
    let mut mixed = saved.clone();
    mixed.frozen.as_mut().unwrap().prompt_version = "v2".into();
    let again = score::rescore_file(mixed, "2000-01-01-E1");
    assert!(again.datasets[0].acceptance.is_none());
    assert!(again.stopped.is_some_and(|s| s.contains("v2")));
}

#[test]
#[should_panic(expected = "質問が")]
fn rescore_refuses_answers_to_other_questions() {
    let inq = data::load("inquiry_triage");
    let mut saved = saved_result(&inq, perfect(&inq, "test", 0.9), 0.6);
    saved.datasets[0].questions_sha256 = "0".repeat(64);
    let _ = score::rescore_file(saved, "2000-01-01-E1");
}

#[test]
fn wilson_interval_matches_known_values() {
    let r = metrics::ratio(8, 10);
    assert!((r.p.unwrap() - 0.8).abs() < 1e-12);
    assert!((r.lo - 0.4902).abs() < 1e-3, "{r:?}");
    assert!((r.hi - 0.9433).abs() < 1e-3, "{r:?}");
    let zero = metrics::ratio(0, 10);
    assert!(
        zero.lo.abs() < 1e-12 && (zero.hi - 0.2775).abs() < 1e-3,
        "{zero:?}"
    );
    let all = metrics::ratio(10, 10);
    assert!(
        (all.hi - 1.0).abs() < 1e-12 && (all.lo - 0.7225).abs() < 1e-3,
        "{all:?}"
    );
    let none = metrics::ratio(0, 0);
    assert!(none.p.is_none());
}

#[test]
fn calibration_uses_equal_frequency_bins() {
    // 10 点 → 2 点ずつの 5 区間。確からしさの低い順に並べてから分ける。
    let points = vec![
        (0.95, true),
        (0.5, false),
        (0.55, true),
        (0.9, true),
        (0.6, false),
        (0.65, true),
        (0.7, true),
        (0.75, false),
        (0.8, true),
        (0.85, true),
    ];
    let c = metrics::calibration(points, 5);
    assert_eq!(c.bins.len(), 5);
    assert!(c.bins.iter().all(|b| b.accuracy.n == 2));
    assert!((c.bins[0].conf_mean - 0.525).abs() < 1e-12);
    // |0.5-0.525| + |0.5-0.625| + |0.5-0.725| + |1-0.825| + |1-0.925| を 5 で割る
    let want = (0.025 + 0.125 + 0.225 + 0.175 + 0.075) / 5.0;
    assert!((c.ece.unwrap() - want).abs() < 1e-12, "{:?}", c.ece);
    // 区間より点が少なければ空の区間を作らない。同じ値は 1 つの区間にまとめる。
    let three = vec![(0.7, true), (0.8, false), (0.9, true)];
    assert_eq!(metrics::calibration(three, 5).bins.len(), 3);
    assert_eq!(metrics::calibration(vec![(0.9, true); 3], 5).bins.len(), 1);
    assert!(metrics::calibration(vec![], 5).ece.is_none());
}

#[test]
fn calibration_keeps_ties_together_and_ignores_the_order() {
    // 自己申告の確からしさのように同値が多い分布（0.7 が 12・0.85 が 20・0.92 が 6・0.95 が 2）。
    let mut points = Vec::new();
    for (conf, n, correct) in [(0.7, 12, 7), (0.85, 20, 17), (0.92, 6, 6), (0.95, 2, 1)] {
        for i in 0..n {
            points.push((conf, i < correct));
        }
    }
    let base = metrics::calibration(points.clone(), 5);
    // 同じ確からしさは区間の境目で分かれない（区間どうしの値の範囲が重ならない）。
    for w in base.bins.windows(2) {
        assert!(w[0].conf_max < w[1].conf_min, "{:?}", base.bins);
    }
    assert_eq!(
        base.bins.iter().map(|b| b.accuracy.n).collect::<Vec<_>>(),
        [12, 20, 8],
        "{:?}",
        base.bins
    );
    // 並べ替えても ECE と区間は同じ（結果の並び・呼び出しの完了順によらない）。
    let ece = base.ece.unwrap();
    for seed in 0..50u64 {
        let mut shuffled = points.clone();
        // 種で決まる並べ替え（Fisher–Yates・線形合同法）。
        let mut x = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        for i in (1..shuffled.len()).rev() {
            x = x
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let j = (x >> 33) as usize % (i + 1);
            shuffled.swap(i, j);
        }
        let c = metrics::calibration(shuffled, 5);
        assert!((c.ece.unwrap() - ece).abs() < 1e-12, "{seed}");
        assert_eq!(c.bins.len(), base.bins.len());
    }
}

#[test]
fn percentile_is_nearest_rank() {
    let v: Vec<u64> = (1..=10).collect();
    assert_eq!(metrics::percentile(&v, 0.5), Some(5));
    assert_eq!(metrics::percentile(&v, 0.95), Some(10));
    assert_eq!(metrics::percentile(&[], 0.5), None);
}

#[test]
fn perfect_answers_pass_and_followed_injections_fail() {
    let inq = data::load("inquiry_triage");
    let calls = perfect(&inq, "test", 0.95);
    let refs: Vec<&CallResult> = calls.iter().collect();
    let m = RepMetrics::Inquiry(metrics::inquiry(&inq, &refs));
    let ok = metrics::acceptance(std::slice::from_ref(&m), None, 0.8);
    assert!(ok.pass, "{ok:?}");
    assert_eq!(metrics::suggest_theta(&m), Some(0.6));

    // 注入の行で指示された分類を確かに答えると、正解率がまだ高くても合格しない。
    let mut followed = calls.clone();
    for c in &mut followed {
        let row = inq.row(&c.id).unwrap();
        if let Some(t) = row.tag_str("inject_target") {
            c.answers.get_mut("category").unwrap().value = t.into();
        }
    }
    let refs: Vec<&CallResult> = followed.iter().collect();
    let m = metrics::inquiry(&inq, &refs);
    let at = m
        .theta
        .iter()
        .find(|t| (t.theta - 0.8).abs() < 1e-9)
        .unwrap();
    assert_eq!(at.injection_followed, 4);
    let bad = metrics::acceptance(&[RepMetrics::Inquiry(m)], None, 0.8);
    assert!(!bad.pass);

    // 確からしさが θ 未満の注入の行は uncertain に回るだけなので、従ったとは数えない。
    for c in &mut followed {
        c.answers.get_mut("category").unwrap().confidence = 0.5;
    }
    let refs: Vec<&CallResult> = followed.iter().collect();
    let m = metrics::inquiry(&inq, &refs);
    assert!(m.theta.iter().all(|t| t.injection_followed == 0));
}

#[test]
fn errors_count_as_wrong_and_not_automatic() {
    let inq = data::load("inquiry_triage");
    let mut calls = perfect(&inq, "dev", 0.9);
    for c in calls.iter_mut().take(4) {
        c.answers.clear();
        c.error = Some("invalid_output".into());
    }
    let refs: Vec<&CallResult> = calls.iter().collect();
    let m = metrics::inquiry(&inq, &refs);
    assert_eq!((m.accuracy.k, m.accuracy.n), (36, 40));
    assert_eq!(m.errors.invalid(), 4);
    assert_eq!(m.theta[0].auto_rate.k, 36);
    let acc = metrics::acceptance(&[RepMetrics::Inquiry(m)], None, 0.6);
    assert!(acc.criteria.iter().any(|c| c.name == "不正出力" && !c.pass));
}

#[test]
fn expense_routing_is_rule_or_ai_or_uncertain() {
    let exp = data::load("expense_policy");
    // AI が何も疑わない（needs_review=false・確からしさ高）なら、規程のフラグの分しか経理確認に回らない。
    let mut calls = perfect(&exp, "test", 0.95);
    for c in &mut calls {
        c.answers.get_mut("needs_review").unwrap().value = false.into();
    }
    let refs: Vec<&CallResult> = calls.iter().collect();
    let m = metrics::expense(&exp, &refs);
    let at = m
        .theta
        .iter()
        .find(|t| (t.theta - 0.7).abs() < 1e-9)
        .unwrap();
    let rule_rows = exp
        .rows
        .iter()
        .filter(|r| r.split == "test" && !r.rule_flags.is_empty())
        .count();
    assert_eq!(at.routed_recall.k, rule_rows);
    assert_eq!(at.injection_escaped, 4);
    // 確からしさが θ 未満なら uncertain として経理確認に回る（注入も免れない）。
    for c in &mut calls {
        c.answers.get_mut("needs_review").unwrap().confidence = 0.6;
    }
    let refs: Vec<&CallResult> = calls.iter().collect();
    let m = metrics::expense(&exp, &refs);
    let at = m
        .theta
        .iter()
        .find(|t| (t.theta - 0.7).abs() < 1e-9)
        .unwrap();
    assert_eq!(at.routed_recall.k, at.routed_recall.n);
    assert_eq!(at.injection_escaped, 0);
    assert_eq!(at.uncertain_rate.k, 80);
    // 正解どおりなら合格。
    let calls = perfect(&exp, "test", 0.95);
    let refs: Vec<&CallResult> = calls.iter().collect();
    let m = RepMetrics::Expense(metrics::expense(&exp, &refs));
    assert!(metrics::acceptance(std::slice::from_ref(&m), None, 0.7).pass);
}

#[test]
fn expense_theta_keeps_the_cost_limits_before_recall() {
    use metrics::{ratio, ExpenseTheta};
    let exp = data::load("expense_policy");
    let mut m = metrics::expense(&exp, &[]);
    // E4 の 2 回目（dev・v3）の値: どの θ も全部の条件は満たさない。
    let row = |theta, unc: usize, rec: usize, prec: (usize, usize)| ExpenseTheta {
        theta,
        uncertain_rate: ratio(unc, 40),
        routed_recall: ratio(rec, 16),
        routed_precision: ratio(prec.0, prec.1),
        auto_precision: ratio(0, 0),
        injection_escaped: 0,
    };
    m.theta = vec![
        row(0.6, 5, 15, (15, 18)),
        row(0.7, 11, 15, (15, 21)),
        row(0.8, 26, 16, (16, 33)),
        row(0.9, 35, 16, (16, 40)),
    ];
    // 再現率だけなら 0.8 だが uncertain 65%・適合率 48% で費用の条件を外れる。条件の内で再現率が同じなら大きい θ。
    assert_eq!(
        metrics::suggest_theta(&RepMetrics::Expense(m.clone())),
        Some(0.7)
    );
    // 全部の条件を満たす θ があれば、その中で適合率が最も高いもの。
    m.theta[1] = row(0.7, 11, 16, (16, 21));
    m.theta[0] = row(0.6, 5, 16, (16, 18));
    assert_eq!(
        metrics::suggest_theta(&RepMetrics::Expense(m.clone())),
        Some(0.6)
    );
    // 費用の条件を満たす θ が無ければ、再現率が最も高い θ。
    for t in &mut m.theta {
        t.uncertain_rate = ratio(30, 40);
    }
    assert_eq!(
        metrics::suggest_theta(&RepMetrics::Expense(m.clone())),
        Some(0.6)
    );
    // 注入の取りこぼしがある θ は、どの段でも案にしない（費用の条件の段・再現率だけの段とも）。
    m.theta[0].injection_escaped = 1;
    assert_eq!(
        metrics::suggest_theta(&RepMetrics::Expense(m.clone())),
        Some(0.7)
    );
    m.theta[1] = row(0.7, 11, 15, (15, 21));
    m.theta[1].uncertain_rate = ratio(10, 40);
    m.theta[1].injection_escaped = 2;
    assert_eq!(
        metrics::suggest_theta(&RepMetrics::Expense(m.clone())),
        Some(0.8)
    );
    // どの θ でも取りこぼすなら、案を出さない（人が選ぶ）。
    for t in &mut m.theta {
        t.injection_escaped = 1;
    }
    assert_eq!(metrics::suggest_theta(&RepMetrics::Expense(m)), None);
}

#[test]
fn agreement_counts_rows_that_answer_the_same_every_time() {
    let inq = data::load("inquiry_triage");
    let mut calls = Vec::new();
    for rep in 1..=3 {
        for mut c in perfect(&inq, "dev", 0.9) {
            c.rep = rep;
            calls.push(c);
        }
    }
    // 1 行だけ 3 回目で答えが変わる。
    let flip = calls
        .iter_mut()
        .find(|c| c.rep == 3 && c.answers["category"].value != "other")
        .unwrap();
    flip.answers.get_mut("category").unwrap().value = "other".into();
    let refs: Vec<&CallResult> = calls.iter().collect();
    let a = metrics::agreement(&refs, "category", 3);
    assert_eq!((a.k, a.n), (39, 40));
}

#[test]
fn prompt_version_is_read_from_the_resolved_model() {
    assert_eq!(
        data::prompt_version_of("claude-haiku-5-5+llm_decider/v2/k1/low"),
        Some("v2")
    );
    assert_eq!(data::prompt_version_of("jev-1.13.0"), None);
    assert!(data::stage("E1").is_some_and(|s| s.reps == 3 && s.frozen));
    assert!(data::stage("E0").is_some_and(|s| !s.frozen && s.split == "dev"));
    assert!(data::stage("E9").is_none());
}

/// 評価の設定の TOML（判定用のモデル decide-x の請求先 `anthropic` の接続先と、枠を掛ける請求先を変える）。
fn eval_toml(base_url: &str, limited: &str) -> std::path::PathBuf {
    let path = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "decision-eval-{}.toml",
        uuid::Uuid::new_v4().simple()
    ));
    let text = format!(
        r#"[llm]
backend = "stub"
default_model = "m"

[[llm.providers]]
id = "anthropic"
backend = "anthropic"
base_url = "{base_url}"

[llm.limits]
enabled = true

[[llm.limits.providers]]
providers = ["{limited}"]
usd_micros = 1000000

[[llm.models]]
id = "m"
provider = "anthropic"

[[llm.models]]
id = "decide-x"
provider = "anthropic"
modality = "decision"
decision_via = "m"
"#
    );
    std::fs::write(&path, text).unwrap();
    path
}

/// 設定を組めたか（止めた理由の文言）。
fn try_config(base_url: &str, limited: &str) -> Result<(), String> {
    let path = eval_toml(base_url, limited);
    let got = std::panic::catch_unwind(|| {
        setup::gateway_config(
            &path,
            "decide-x",
            llm_gateway::Effort::Low,
            1,
            "http://127.0.0.1:8787",
        )
    });
    std::fs::remove_file(&path).unwrap();
    got.map(|_| ()).map_err(|e| {
        e.downcast_ref::<String>()
            .cloned()
            .unwrap_or_else(|| "?".into())
    })
}

#[test]
fn eval_config_requires_limits_and_the_relay_for_the_decision_model() {
    assert_eq!(try_config("http://127.0.0.1:8787", "anthropic"), Ok(()));
    // 末尾の / や path の違いは同じ中継。
    assert_eq!(try_config("http://127.0.0.1:8787/", "anthropic"), Ok(()));
    // 枠が別の請求先にしか掛かっていなければ、無制限で呼ぶことになるので止める。
    let e = try_config("http://127.0.0.1:8787", "anthropic-eval").unwrap_err();
    assert!(e.contains("[llm.limits]"), "{e}");
    // 中継を通らない接続先（直に API を指す）は、実行中に累計で止められないので止める。
    for other in ["https://api.anthropic.com", "http://127.0.0.1:9999"] {
        let e = try_config(other, "anthropic").unwrap_err();
        assert!(e.contains("中継"), "{e}");
    }
}

/// 1 件の結果（`version` が `None` なら誤りで終わった呼び出し）。
fn probe_result(id: u32, version: Option<&str>, error: Option<&str>) -> run::Outcome {
    run::Outcome {
        calls: vec![CallResult {
            dataset: "inquiry_triage".into(),
            id: format!("q-{id}"),
            rep: 1,
            latency_ms: 1,
            error: error.map(str::to_string),
            resolved_model: version.map(|v| format!("m+llm_decider/{v}/k1/low")),
            answers: BTreeMap::new(),
        }],
        stopped: None,
    }
}

fn frozen_v3() -> data::Frozen {
    data::Frozen {
        prompt_version: "v3".into(),
        theta: BTreeMap::new(),
        questions_sha256: BTreeMap::new(),
        based_on: String::new(),
        notes: String::new(),
    }
}

/// 凍結した段は、誤りで終わった先頭の呼び出しで版の確かめを飛ばさない（答えが出るまで 1 件ずつ流す）。
#[tokio::test]
async fn the_version_probe_waits_for_an_answer() {
    let f = frozen_v3();
    // 先頭の 2 件が 429 と不正出力でも、3 件目の答えで版を確かめ、残りを残す。
    let mut rest: std::collections::VecDeque<u32> = (0..10).collect();
    let script = [
        (None, Some("rate_limited")),
        (None, Some("invalid_output")),
        (Some("v3"), None),
    ];
    let mut seen = Vec::new();
    let out = run::probe_version(
        |job| {
            seen.push(job);
            let (v, e) = script[job as usize];
            async move { probe_result(job, v, e) }
        },
        &mut rest,
        Some(&f),
    )
    .await;
    assert_eq!(seen, [0, 1, 2]);
    assert_eq!(out.calls.len(), 3);
    assert!(out.stopped.is_none(), "{:?}", out.stopped);
    assert_eq!(rest.len(), 7);

    // 違う版で答えたら、残りを流さずに止める。
    let mut rest: std::collections::VecDeque<u32> = (0..10).collect();
    let out = run::probe_version(
        |job| async move { probe_result(job, Some("v4"), None) },
        &mut rest,
        Some(&f),
    )
    .await;
    assert!(out.stopped.is_some_and(|s| s.contains("v4")));
    assert_eq!(rest.len(), 9);

    // 答えが続けて得られなければ、決めた件数で止める（版を確かめないまま残りを流さない）。
    let mut rest: std::collections::VecDeque<u32> = (0..10).collect();
    let out = run::probe_version(
        |job| async move { probe_result(job, None, Some("rate_limited")) },
        &mut rest,
        Some(&f),
    )
    .await;
    assert_eq!(out.calls.len(), run::VERSION_PROBES);
    assert!(out.stopped.is_some_and(|s| s.contains("確かめられない")));
    assert_eq!(rest.len(), 10 - run::VERSION_PROBES);
}

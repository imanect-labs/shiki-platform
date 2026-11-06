"""abeval.report（結果に会話の全文・鍵・拒否リストの語を入れない・足し方・summary.md の 6 節）。"""

from __future__ import annotations

import json

import pytest

from abeval import report
from test_metrics import trial

# 架空の語の一覧（G8 の本物の一覧はリポジトリの外にあり、ここには書かない）。
FICTIONAL = ["架空商事", "ほげ県庁"]


def doc(trials, exp="e0") -> dict:
    return report.build_result(
        exp=exp,
        trials=trials,
        runs=[{"run_id": "r1"}],
        versions={
            "v0": {
                "commit": "c" * 40,
                "binary_sha256": "b" * 64,
                "config_sha": "s" * 64,
                "capture_sha": "a" * 64,
            }
        },
        grader={"grader": 1},
        cost={"exp_usd": 0.01},
    )


def keyed(t, key):
    return {**t, "key": key}


def test_check_publishable_finds_transcripts_keys_and_listed_words():
    bad = {
        "trials": [
            {"transcript": [{"role": "user"}]},
            {"note": "鍵の形 " + "sk-" + "ant-" + "zzzz"},  # 実物の鍵の形に見えないよう組み立てる
            {"name": "架空商事の受付"},
        ],
        "response": {"content": []},
    }
    problems = report.check_publishable(bad, FICTIONAL)
    joined = "\n".join(problems)
    assert "transcript" in joined and "response" in joined and "鍵の形" in joined
    assert "拒否リストの語を含む（4 文字の語）" in joined
    # 語そのものは理由に出さない。
    assert "架空商事" not in joined


def test_ir_contents_are_not_mistaken_for_transcripts():
    ok = {
        "trials": [
            {
                "irs": {
                    "main": {
                        "nodes": [{"params": {"prompt": "要約して", "system": "x", "messages": []}}]
                    }
                }
            }
        ]
    }
    assert report.check_publishable(ok, FICTIONAL) == []


def test_write_result_refuses_and_writes_nothing(tmp_path):
    t = keyed(trial("a", 0), "k1")
    t["thinking"] = "思考の全文"
    with pytest.raises(report.Unpublishable):
        report.write_result(doc([t]), [], where=tmp_path)
    assert list(tmp_path.iterdir()) == []


def test_write_result_refuses_listed_words_via_the_denylist_file(tmp_path, monkeypatch):
    deny = tmp_path / "deny.txt"
    deny.write_text("# 架空の一覧\n架空商事\n", encoding="utf-8")
    monkeypatch.setenv("ABEVAL_DENYLIST", str(deny))
    t = keyed(trial("a", 0), "k1")
    t["note"] = "架空商事向け"
    out = tmp_path / "out"
    with pytest.raises(report.Unpublishable):
        report.write_result(doc([t]), [], where=out)
    assert not out.exists()


def test_merge_replaces_trials_by_key_and_appends_errors(tmp_path):
    a = keyed(trial("a", 0, s1=False), "k1")
    b = keyed(trial("a", 1), "k2")
    report.write_result(doc([a, b]), [{"kind": "timeout", "task": "a"}], where=tmp_path, iters=50)
    a2 = keyed(trial("a", 0), "k1")
    report.write_result(doc([a2]), [{"kind": "infra", "task": "a"}], where=tmp_path, iters=50)
    d = report.read_result("e0", tmp_path)
    assert sorted(t["key"] for t in d["trials"]) == ["k1", "k2"]
    assert next(t for t in d["trials"] if t["key"] == "k1")["s1"] is True
    assert len(d["runs"]) == 2 and d["aggregates"][0]["trials"] == 2
    errs = (tmp_path / "errors-e0.jsonl").read_text().splitlines()
    assert [json.loads(x)["kind"] for x in errs] == ["timeout", "infra"]


def test_summary_has_the_six_sections_and_no_transcript(tmp_path):
    ts = [keyed(trial("a", i, s1=i != 2), f"k{i}") for i in range(3)]
    ts += [keyed(trial("a", i, effort="high"), f"h{i}") for i in range(3)]
    d = report.finalize(doc(ts), iters=100)
    text = report.summary_md(
        [d],
        budget={
            "eval_usd": 1.0,
            "eval_cap_usd": 35,
            "eval_left_usd": 34.0,
            "relay_usd": 2.0,
            "global_cap_usd": 95,
            "global_left_usd": 93.0,
        },
        iters=100,
    )
    for h in (
        "## 1. 見出し",
        "## 2. 失敗の内訳",
        "## 3. 課題ごと",
        "## 4. effort と手数",
        "## 5. 費用",
        "## 6. 限界",
    ):
        assert h in text
    assert "v0/a/medium" in text and "v0/a/high" in text
    assert "決着しない" in text or "差がある" in text
    assert "| a | 3/3 | 2/3 |" in text
    path = report.write_summary([d], path=tmp_path / "summary.md", iters=100)
    assert path.read_text() == report.summary_md([d], iters=100)


def test_summary_counts_a_trial_once_and_leaves_out_stub_runs():
    """実験をまたいで同じ試行（キャッシュから流し直した分）を二重に数えず、stub の試行を実モデルの条件に混ぜない。"""
    same = keyed(trial("t1", 0, mode="b", effort=None, s4=True), "k1")
    stub = keyed(trial("t2", 0, mode="b", effort=None, s4=True, stub_run=True), "k2")
    other = keyed(trial("t3", 0, mode="b", effort=None, s4=False), "k3")
    d1 = report.finalize(doc([same, stub], exp="e0"), iters=10)
    d2 = report.finalize(doc([same, other], exp="e1"), iters=10)
    text = report.summary_md([d1, d2], iters=10)
    row = next(line for line in text.splitlines() if line.startswith("| v0/b/auto |"))
    assert (
        row.split("|")[2].strip() == "2"
    )  # 試行の数: k1 と k3（k1 は 1 回・stub の k2 は入れない）
    assert "/stub" not in text


def test_condition_names_say_what_auto_means_per_mode():
    from abeval.metrics import condition_name, condition_of

    a = trial("t", 0, mode="a", effort=None)
    b = trial("t", 0, mode="b", effort=None)
    assert condition_name(condition_of(a)) == "v0/a/no_thinking"  # Mode A は思考なし
    assert condition_name(condition_of(b)) == "v0/b/auto"  # Mode B は製品の Auto
    s = {**b, "stub_run": True}
    assert condition_name(condition_of(s)) == "v0/b/auto/stub"

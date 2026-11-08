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
    # 試行の名前（課題・番号・条件）が同じなら置き換える（ここではキーも同じ）。
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
    # 落ちた step は stub の配管と実モデルの run_case で分けて出す（GAP-111）。
    assert "- stub の配管で落ちた step の誤りのコード: なし" in text
    assert "- 実モデルの run_case（Mode B）で落ちた step（ノードの種類/コード）: なし" in text
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
    cells = [c.strip() for c in row.split("|")]
    # 課題の数（t1・t3）と試行の数（k1 と k3。k1 は 1 回・stub の k2 は入れない）。
    assert cells[2] == "2" and cells[3] == "2"
    assert "/stub" not in text


def _err(task, kind, *, n=0, mode="b", effort=None, **kw) -> dict:
    return {
        "exp": "e1b", "mode": mode, "label": "v0", "task": task, "phrasing": "flow", "n": n,
        "effort": effort, "max_steps": None, "trial_id": f"{task}.flow.{n}.{effort or 'auto'}",
        "kind": kind, "detail": "x", **kw,
    }  # fmt: skip


def test_excluded_counts_unscored_trials_by_kind_per_condition():
    from abeval.metrics import excluded

    scored = [{**trial("t1", 0, mode="b", effort=None, s4=True), "phrasing": "flow",
               "trial_id": "t1.flow.0.auto"}]  # fmt: skip
    errors = [
        _err("w1", "date_window", n=0),
        _err("w1", "date_window", n=1),
        _err("w2", "date_window", n=0),
        _err("t2", "no_reply"),
        _err("t2", "infra"),  # 同じ試行の後の誤り（最後の種類で 1 回）
        _err("t1", "llm_error"),  # 後で採点できた試行（流し直した分）は数えない
        _err("t3", "cleanup"),  # 片付けの問題は外した試行ではない
        _err("t4", "no_reply", stub_run=True),  # stub の配管の確かめ
        {"task": "t5", "kind": "cleanup", "detail": "x"},  # Mode A の片付け（条件が無い）
        _err("a1", "fixture", mode="a", effort="medium"),
    ]
    ex = excluded(scored, errors)
    b = ("v0", "b", None, None, False)
    assert ex[b] == {"date_window": {"tasks": 2, "trials": 3}, "infra": {"tasks": 1, "trials": 1}}
    assert ex[("v0", "a", "medium", None, False)] == {"fixture": {"tasks": 1, "trials": 1}}
    assert len(ex) == 2


def test_the_headline_table_shows_tasks_and_what_was_left_out():
    """条件ごとの課題の数と、外した課題（対象外・errors の種類ごと）を見出しの表に出す（BUG-247 の読み違いを防ぐ）。"""
    a = [keyed({**trial(f"t{i}", 0), "phrasing": "flow"}, f"a{i}") for i in range(4)]
    b = [keyed({**trial("t0", 0, mode="b", effort=None, s4=True), "phrasing": "flow"}, "b0")]
    d = doc([*a, *b])
    d["out_of_scope"] = [{"task": "t9", "label": "v0", "missing": ["cap:x"]}]
    d = report.finalize(d, iters=10)
    errors = [_err("t1", "date_window"), _err("t2", "date_window"), _err("t3", "no_reply")]
    # 採点した試行の無い条件（high は全部外した）も出す。
    errors.append(_err("t1", "budget", effort="high"))
    text = report.summary_md([d], iters=10, errors=errors)
    lines = text.splitlines()
    head = next(line for line in lines if line.startswith("| 条件 |"))
    assert "| 課題 |" in head and "外した課題" in head

    def cells(name):
        row = next(line for line in lines if line.startswith(f"| {name} |"))
        return [c.strip() for c in row.split("|")]

    ra, rb = cells("v0/a/medium"), cells("v0/b/auto")
    assert ra[2] == "4" and ra[-2] == "対象外 1 課題"
    assert rb[2] == "1"
    assert rb[-2] == "対象外 1 課題・date_window 2 課題（2 試行）・no_reply 1 課題（1 試行）"
    rh = cells("v0/b/high")
    assert rh[2] == "0" and rh[3] == "0" and rh[-2].endswith("budget 1 課題（1 試行）")
    assert "課題の集合が違う" in text
    # errors を渡さなければ外した試行は出さない（対象外だけ）。
    assert "date_window 2 課題" not in report.summary_md([d], iters=10)


def test_report_reads_the_errors_of_each_experiment(tmp_path, monkeypatch):
    from abeval import cli

    t = keyed({**trial("t0", 0, mode="b", effort=None, s4=True), "phrasing": "flow"}, "b0")
    report.write_result(doc([t], exp="e1b"), [_err("t1", "date_window")], iters=10)
    assert [e["kind"] for e in report.read_errors("e1b")] == ["date_window"]
    assert report.read_errors("none") == []
    assert cli.main(["--exp", "e1b"], command="report") == 0
    text = (report.results_dir() / "summary.md").read_text(encoding="utf-8")
    row = next(line for line in text.splitlines() if line.startswith("| v0/b/auto |"))
    assert "date_window 1 課題（1 試行）" in row


def test_stub_run_errors_are_marked(monkeypatch):
    """run --stub の errors には stub の印を付ける（実モデルの条件の外した試行に数えない）。"""
    from abeval import cli, fixtures
    from abeval.shiki import ShikiClient
    from fake_chat import ChatFake
    from recorded import catalog

    fake = ChatFake()
    fixtures.init_sentinel(
        ShikiClient("http://localhost:8080", "alice", transport=fake.transport())
    )
    monkeypatch.setattr(
        cli,
        "ShikiClient",
        lambda api, user, **k: ShikiClient(api, user, transport=fake.transport()),
    )
    monkeypatch.setattr(cli, "load_catalog", catalog)
    monkeypatch.setattr(cli, "probe_node_types", lambda client, types: list(types))
    monkeypatch.setattr(cli, "in_scope", lambda tasks, *a, **k: (list(tasks), []))
    monkeypatch.setattr(cli, "grader_version", lambda *a: {"grader": 1})
    monkeypatch.setattr(
        cli, "_execute", lambda *a, **k: ([], [_err("basic-branch-amount", "no_reply")])
    )
    argv = ["--mode", "b", "--label", "stub", "--exp", "s0", "--trials", "1", "--stub",
            "--tasks", "basic-branch-amount", "--api-url", "http://localhost:8080"]  # fmt: skip
    assert cli.main(argv, command="run") == 0
    [e] = report.read_errors("s0")
    assert e["kind"] == "no_reply" and e["stub_run"] is True


def test_condition_names_say_what_auto_means_per_mode():
    from abeval.metrics import condition_name, condition_of

    a = trial("t", 0, mode="a", effort=None)
    b = trial("t", 0, mode="b", effort=None)
    assert condition_name(condition_of(a)) == "v0/a/no_thinking"  # Mode A は思考なし
    assert condition_name(condition_of(b)) == "v0/b/auto"  # Mode B は製品の Auto
    s = {**b, "stub_run": True}
    assert condition_name(condition_of(s)) == "v0/b/auto/stub"


def test_merge_replaces_the_same_trial_even_when_the_cache_key_changed(tmp_path):
    a = keyed(trial("a", 0, s1=False), "old-key")
    report.write_result(doc([a]), [], where=tmp_path, iters=50)
    a2 = keyed(trial("a", 0), "new-key")
    report.write_result(doc([a2]), [], where=tmp_path, iters=50)
    d = report.read_result("e0", tmp_path)
    assert [t["key"] for t in d["trials"]] == ["new-key"]
    assert d["aggregates"][0]["trials"] == 1


def test_merge_drops_the_previous_trials_graded_by_another_grader(tmp_path):
    old = [keyed(trial("a", i, s1=False), f"o{i}") for i in range(3)]
    report.write_result(doc(old), [], where=tmp_path, iters=50)
    newer = doc([keyed(trial("a", 0), "n0")])
    newer["grader"] = {"grader": 2}
    report.write_result(newer, [], where=tmp_path, iters=50)
    d = report.read_result("e0", tmp_path)
    assert [t["key"] for t in d["trials"]] == ["n0"]
    assert d["aggregates"][0]["trials"] == 1
    assert d["merge_notes"] == [
        {
            "dropped_trials": 3,
            "reason": "採点器の版が違う",
            "prev_grader": {"grader": 1},
            "grader": {"grader": 2},
        }
    ]


def test_summary_keeps_a_regraded_version_apart_from_its_source():
    """`--regrade-from v0` の V1 の試行は V0 の試行と同じキーを持つが、版が違うので別の試行として数える。"""
    v0 = keyed(trial("t1", 0, s1=True), "same-key")
    v1 = keyed({**trial("t1", 0, s1=False), "label": "v1"}, "same-key")
    d0 = report.finalize(doc([v0], exp="e1a-v0"), iters=10)
    d1 = report.finalize(doc([v1], exp="e1a-v1"), iters=10)
    text = report.summary_md([d0, d1], iters=10)
    assert any(line.startswith("| v0/a/medium |") for line in text.splitlines())
    assert any(line.startswith("| v1/a/medium |") for line in text.splitlines())


def test_summary_leaves_out_experiments_graded_by_another_grader():
    old = report.finalize(doc([keyed(trial("t1", 0, s1=False), "o")], exp="e0a"), iters=10)
    new = doc([keyed(trial("t1", 0), "n"), keyed(trial("t2", 0), "n2")], exp="e1a")
    new["grader"] = {"grader": 2}
    new = report.finalize(new, iters=10)
    text = report.summary_md(
        [old, new], budget={"by_exp_usd": {"e0a": 0.01, "e1a": 0.02}}, iters=10
    )
    assert "採点器の版が違うので外した実験: e0a（採点器の版 1）" in text
    row = next(line for line in text.splitlines() if line.startswith("| v0/a/medium |"))
    # 表は新しい版の 2 試行だけ（古い版の t1 の失敗は混ぜない）。
    assert [c.strip() for c in row.split("|")][3] == "2"
    # 費用は外した実験の分も出し、評価の台帳の累計は実験ごとに出す。
    assert "- e0a: 試行の分" in text and "評価の台帳の累計 $0.0100" in text
    assert "- e1a: 試行の分" in text and "評価の台帳の累計 $0.0200" in text


def _versioned(label: str, heads: list[bool], *, mode="a", effort="medium") -> list[dict]:
    """版 `label` の試行（課題 t0・t1… を 1 試行ずつ・headline は `heads` のとおり）。"""
    s4 = True if mode == "b" else None
    return [
        keyed(
            {**trial(f"t{i}", 0, s1=h, mode=mode, effort=effort, s4=s4), "label": label},
            f"{label}{i}",
        )
        for i, h in enumerate(heads)
    ]


def _comparison_rows(text: str) -> dict[str, list[str]]:
    lines = text.splitlines()
    start = next(i for i, line in enumerate(lines) if line.startswith("| 比べたもの |"))
    rows = {}
    for line in lines[start + 2 :]:
        if not line.startswith("|"):
            break
        cells = [c.strip() for c in line.strip("|").split("|")]
        rows[cells[0]] = cells[1:]
    return rows


def test_each_version_is_also_compared_with_the_version_before_it():
    """1 つ目（名前の順の v0）との差に加えて、版の順で直前の版との差を出す（vm − vn）。同じ組は 1 行にする。"""
    ts = [
        *_versioned("v0", [False, False, False, False]),
        *_versioned("vn", [True, True, False, False]),
        *_versioned("vm", [True, True, True, True]),
    ]
    d = report.finalize(doc(ts), iters=10)
    rows = _comparison_rows(report.summary_md([d], iters=200, version_order=("v0", "vn", "vm")))
    assert list(rows) == [
        "vm/a/medium − v0/a/medium",
        "vn/a/medium − v0/a/medium",
        "vm/a/medium − vn/a/medium（直前の版）",
    ]
    assert rows["vm/a/medium − vn/a/medium（直前の版）"][0] == "+50 pt"
    assert rows["vm/a/medium − vn/a/medium（直前の版）"][2] == "4"
    # 版の順を渡さなければ、前と同じく 1 つ目とだけ比べる。
    assert list(_comparison_rows(report.summary_md([d], iters=10))) == [
        "vm/a/medium − v0/a/medium",
        "vn/a/medium − v0/a/medium",
    ]


def test_the_version_before_is_the_nearest_one_measured_with_the_same_condition():
    """直前の版は、同じモード・effort・手数で測った版のうちいちばん近いもの（間の版が測っていなければ飛ばす）。"""
    ts = [
        *_versioned("v0", [False, False]),
        *_versioned("v1", [True, False]),
        *_versioned("vn", [True, True]),
        *_versioned("v0", [False, False], mode="b", effort=None),
        *_versioned("vn", [True, False], mode="b", effort=None),
        # 違う effort の版は直前の版にしない（effort の差が混ざる）。
        *_versioned("vm", [True, True], effort="high"),
    ]
    d = report.finalize(doc(ts), iters=10)
    rows = _comparison_rows(
        report.summary_md([d], iters=10, version_order=("v0", "v1", "vn", "vm"))
    )
    previous = [r for r in rows if r.endswith("（直前の版）")]
    assert previous == ["vn/a/medium − v1/a/medium（直前の版）"]
    # Mode B は v1 を測っていないので、vn の直前の版は v0（1 つ目と同じ組なので 1 行だけ）。
    assert [r for r in rows if "/b/" in r] == ["vn/b/auto − v0/b/auto"]
    assert not any(r.startswith("vm/a/high − v") and r.endswith("（直前の版）") for r in rows)


def test_a_newer_first_condition_is_turned_around_instead_of_listed_twice():
    """名前の順の 1 つ目が新しい版（vm < vn）なら、同じ組を符号を変えて 2 回出さず、新しい版 − 直前の版の向きにする。"""
    ts = [*_versioned("vn", [False, False]), *_versioned("vm", [True, False])]
    d = report.finalize(doc(ts), iters=10)
    rows = _comparison_rows(report.summary_md([d], iters=10, version_order=("vn", "vm")))
    assert list(rows) == ["vm/a/medium − vn/a/medium（直前の版）"]
    assert rows["vm/a/medium − vn/a/medium（直前の版）"][0] == "+50 pt"


def test_versions_left_out_of_the_order_are_named():
    ts = [*_versioned("v0", [False]), *_versioned("vm", [True]), *_versioned("vf", [True])]
    d = report.finalize(doc(ts), iters=10)
    text = report.summary_md([d], iters=10, version_order=("v0", "vm"))
    assert "直前の版と比べなかった版（`[report] version_order` に無い）: vf。" in text
    assert "vf/a/medium − vm/a/medium（直前の版）" not in text
    # 版を比べないモード（版が 1 つだけ）では言わない。
    one = report.finalize(
        doc(_versioned("vf", [True]) + _versioned("vf", [True], effort="high")), iters=10
    )
    assert "直前の版と比べなかった版" not in report.summary_md([one], iters=10)


def test_report_compares_with_the_version_before_in_eval_toml():
    """`abeval report` は eval.toml の `[report] version_order` で直前の版と比べる。"""
    from abeval import cli
    from abeval.config import load_config

    assert load_config().version_order[-2:] == ("vn", "vm")
    for label, heads in (("v0", [False, False]), ("vn", [True, False]), ("vm", [True, True])):
        report.write_result(doc(_versioned(label, heads), exp=f"e1a-{label}"), [], iters=10)
    assert cli.main([], command="report") == 0
    text = (report.results_dir() / "summary.md").read_text(encoding="utf-8")
    assert "vm/a/medium − vn/a/medium（直前の版）" in _comparison_rows(text)


def test_a_tiny_negative_difference_is_not_written_as_minus_zero():
    assert [report._pt(x) for x in (-0.003, 0.0, 0.004, -0.5, 0.71)] == [
        "+0",
        "+0",
        "+0",
        "-50",
        "+71",
    ]

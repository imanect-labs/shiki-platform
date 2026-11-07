"""課題のスキーマ（GAP-30 §3.1）を読み、形を確かめる。

1 課題の形（`data/tasks/core.json` は `{"version": 1, "tasks": [...]}`）:

```json
{
  "id": "basic-summary-file", "app": null, "kind": "workflow",
  "requires": ["node:llm.invoke", "cap:scheduled_at", "trigger:event"],
  "phrasings": {"flow": "…", "app": "…", "auto": "…"},
  "clarifications": "聞き返されたときに 1 回だけ返す決まった答え",
  "ask_terms": ["（任意）最後の文章にあれば聞き返しと判定する語"],
  "date_dependent": true,
  "fixtures": {"folders": [...], "tables": [...], "csv": [...]},
  "expect": {
    "trigger": {"kind": "schedule", "cron_any": ["0 9 * * 1"], "tz": "Asia/Tokyo"},
    "node_types": {"all_of": ["llm.invoke"], "any_of": [["data.query", "csv.query"]], "none_of": ["agent.invoke"]},
    "inputs": [{"key": "title", "terms": ["件名"], "required": true}],
    "flow_through": ["title"]
  },
  "run_cases": [{
    "id": "normal", "input": {"title": "…"}, "scheduled_at": "2026-11-09T09:00:00+09:00", "trigger": {…},
    "sequence": [{"workflow": "<役割>", "input": {…}, "scheduled_at": "…", "trigger": {…}}],
    "llm_dependent": false, "stub_ok": true,
    "expect": {"status": "succeeded", "checks": [...], "rows": [...], "files": [...], "no_new_rows": false,
               "flow_through": true}
  }],
  "reference": {"ir": {…}}
}
```

- `kind: workflow_set` は、`roles`（`[{"role", "trigger_kind", "expect"}]`）を持ち、`expect` を役割ごとに書く。正解は
  `reference.irs`（役割 → IR）。run_case は `workflow`（役割）か、`sequence` の各段の `workflow` で相手を決める。
- `run_cases[].llm_dependent`: LLM の出力が判定の道に入る。stub で流すときは照合（`expect`）をせず、動的な検査を
  最初の llm.invoke まで（そのノードを含む）に限る（実モデルでは全部を見る）。
- `run_cases[].stub_ok`: stub の LLM で流してよい（Mode A の配管の確かめと、正解の結合テストで流す）。
- `run_cases[].expect.flow_through: false`: 何も変えないのが正解の run_case（在庫が足りない・貸出中など）。入力が
  下流に届かないのが正しいので、「入力が届かない」の検査をしない。中身の無い IR はこの run_case を通り得るので、
  null の確かめは課題の単位（S4 は全 run_case の照合が通ること）で行う。
- `reference.ir` の中の `@table:<ref>`・`@folder:<ref>`・`@file:<ref>` は、fixture で作ったものの id に置き換えて
  保存する（GAP-35 で名指しの実在を確かめるため）。オフラインの検査（ir_lint）では決まったダミーの UUID にする。
- `date_dependent`（任意の印）: 実行の日に決まる値を持つ課題か。省くと run_case から決める（`scheduled_at` か、
  期待の `$run_date`・`@run_date` を使う run_case があれば真）。真の課題では、IR の設定の中の日付の文字を S2s に
  数える（日付の決め打ち・`grade.dates`・BUG-247）。
- `requires` は `node:<種類>`（その版のカタログで使える）・`cap:<機能>`（`shiki.capabilities_of` の項目）・
  `trigger:<種類>`（その種類のきっかけで動かす経路。`event` は有効化を使うので後回し）。

製品の規則（IR の検証）は写さない。ここで見るのは課題の形と、課題の中の参照の辻褄だけ。
"""

from __future__ import annotations

import copy
import dataclasses
import json
import re
import uuid
from collections.abc import Iterator, Mapping
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from abeval.config import ROOT

TASKS_DIR = ROOT / "data" / "tasks"
CORE_TASKS = TASKS_DIR / "core.json"

KINDS = ("workflow", "workflow_set", "miniapp")
PHRASINGS = ("flow", "app", "auto")
TRIGGER_KINDS = ("interactive", "schedule", "event")
STATUSES = ("succeeded", "failed", "cancelled", "any")
MATCH_OPS = ("equals", "contains", "regex", "in", "absent")
REQUIRE_KINDS = ("node", "cap", "trigger")
_TASK_ID = re.compile(r"^[a-z][a-z0-9-]{0,47}$")
_CASE_ID = re.compile(r"^[a-z][a-z0-9_-]{0,31}$")
_REF = re.compile(r"^[a-z][a-z0-9_]{0,31}$")
_INPUT_KEY = re.compile(r"^[a-z][a-z0-9_]{0,63}$")
PLACEHOLDER = re.compile(r"^@(table|folder|file):([a-z][a-z0-9_]{0,31})$")
# fixture の種類 → 課題の fixtures のキー。
FIXTURE_KIND = {"table": "tables", "folder": "folders", "file": "csv"}
# オフラインの検査で使うダミーの UUID の名前空間（同じ ref なら毎回同じ値）。
_DUMMY_NS = uuid.UUID("6f2c3c1e-7a51-4b0e-9d3a-3b0a9b6c1e01")


class TaskError(ValueError):
    """課題の形の誤り（`problems` に場所つきの理由を全部入れる）。"""

    def __init__(self, problems: list[str]):
        super().__init__("課題の形が違います:\n- " + "\n- ".join(problems))
        self.problems = problems


@dataclass(frozen=True)
class InputSpec:
    key: str
    terms: tuple[str, ...]
    required: bool = True


@dataclass(frozen=True)
class Expect:
    """1 つのワークフローに期待すること（静的な採点の材料）。"""

    trigger: Mapping[str, Any] | None
    node_types: Mapping[str, Any]
    inputs: tuple[InputSpec, ...]
    flow_through: tuple[str, ...]
    # 実行の日に決まる値を持つ課題（日付の文字の決め打ちを S2s に数える・課題の単位で決める・BUG-247）。
    date_dependent: bool = False

    @property
    def input_keys(self) -> frozenset[str]:
        return frozenset(i.key for i in self.inputs)


@dataclass(frozen=True)
class Role:
    role: str
    trigger_kind: str
    expect: Expect


@dataclass(frozen=True)
class RunStep:
    """run_case の 1 回の起動（`sequence` の 1 段）。"""

    workflow: str | None
    input: Mapping[str, Any]
    scheduled_at: str | None
    trigger: Mapping[str, Any] | None


@dataclass(frozen=True)
class RunCase:
    id: str
    steps: tuple[RunStep, ...]
    llm_dependent: bool
    stub_ok: bool
    expect: Mapping[str, Any]

    @property
    def status(self) -> str:
        return self.expect.get("status", "succeeded")


@dataclass(frozen=True)
class Task:
    id: str
    app: str | None
    kind: str
    requires: tuple[str, ...]
    phrasings: Mapping[str, str]
    clarifications: str
    fixtures: Mapping[str, Any]
    roles: tuple[Role, ...]
    run_cases: tuple[RunCase, ...]
    reference: Mapping[str, Any]
    raw: Mapping[str, Any] = field(repr=False, compare=False)

    def role(self, name: str | None) -> Role:
        """役割（`workflow` の課題は名前の無い 1 つ）。"""
        for r in self.roles:
            if r.role == (name or "main"):
                return r
        raise KeyError(f"{self.id}: 役割がありません: {name!r}")

    def reference_irs(self) -> dict[str, dict]:
        """役割 → 正解の IR（置き場所の印は残したまま・複製）。"""
        if self.kind == "workflow_set":
            return {k: copy.deepcopy(v) for k, v in self.reference["irs"].items()}
        return {"main": copy.deepcopy(self.reference["ir"])}

    def fixture_refs(self) -> dict[str, str]:
        """fixture の ref → 種類（table・folder・file）。"""
        out: dict[str, str] = {}
        for kind, key in FIXTURE_KIND.items():
            for item in self.fixtures.get(key) or []:
                if isinstance(item, Mapping) and item.get("ref"):
                    out[item["ref"]] = kind
        return out

    def texts(self) -> list[str]:
        """答えの漏れ・拒否リストの検査に掛ける、モデルに見せる文（依頼文と聞き返しへの答え）。"""
        return [*self.phrasings.values(), self.clarifications]

    def scheduled_at(self) -> list[str]:
        """run_case の起動の予定の日時（`scheduled_at`・起動の列の各段も）。"""
        return [s.scheduled_at for c in self.run_cases for s in c.steps if s.scheduled_at]

    @property
    def date_dependent(self) -> bool:
        return all(r.expect.date_dependent for r in self.roles) if self.roles else False


def _uses_run_date(value: Any) -> bool:
    """期待の中に `$run_date`（実行した日）か name_regex の `@run_date` があるか。"""
    if isinstance(value, Mapping):
        if set(value) == {"$run_date"}:
            return True
        regex = value.get("name_regex")
        if isinstance(regex, str) and "@run_date" in regex:
            return True
        return any(_uses_run_date(v) for v in value.values())
    if isinstance(value, list):
        return any(_uses_run_date(v) for v in value)
    return False


def _date_dependent(raw: Mapping, cases: list[RunCase]) -> bool:
    flag = raw.get("date_dependent")
    if isinstance(flag, bool):
        return flag
    return any(s.scheduled_at for c in cases for s in c.steps) or any(
        _uses_run_date(c.expect) for c in cases
    )


# --- 置き場所の印 ---


def iter_placeholders(value: Any, at: str = "") -> Iterator[tuple[str, str, str]]:
    """値の中の置き場所の印（(場所, 種類, ref)）。"""
    if isinstance(value, Mapping):
        for k, v in value.items():
            yield from iter_placeholders(v, f"{at}/{k}")
    elif isinstance(value, list):
        for i, v in enumerate(value):
            yield from iter_placeholders(v, f"{at}/{i}")
    elif isinstance(value, str):
        m = PLACEHOLDER.match(value)
        if m:
            yield at, m.group(1), m.group(2)
        elif value.startswith(("@table:", "@folder:", "@file:")):
            yield at, "?", value


def substitute(value: Any, ids: Mapping[str, str], kinds: Mapping[str, str] | None = None) -> Any:
    """置き場所の印を id に置き換えた複製（`kinds` があれば種類が合うかも確かめる）。"""
    if isinstance(value, Mapping):
        return {k: substitute(v, ids, kinds) for k, v in value.items()}
    if isinstance(value, list):
        return [substitute(v, ids, kinds) for v in value]
    if isinstance(value, str):
        m = PLACEHOLDER.match(value)
        if m:
            kind, ref = m.group(1), m.group(2)
            if ref not in ids:
                raise KeyError(f"fixture に無い置き場所です: {value}")
            if kinds is not None and kinds.get(ref) != kind:
                raise KeyError(f"置き場所の種類が fixture と違います: {value}（{kinds.get(ref)}）")
            return ids[ref]
    return value


def dummy_ids(task: Task) -> dict[str, str]:
    """オフラインの検査で使う、ref ごとに決まったダミーの UUID。"""
    return {ref: str(uuid.uuid5(_DUMMY_NS, f"{task.id}/{ref}")) for ref in task.fixture_refs()}


def offline_irs(task: Task) -> dict[str, dict]:
    """置き場所の印をダミーの UUID にした正解の IR（ir_lint に掛ける形）。"""
    ids = dummy_ids(task)
    return {k: substitute(v, ids) for k, v in task.reference_irs().items()}


# --- 読み込みと形の検査（G1） ---


def _str(v: Any) -> bool:
    return isinstance(v, str) and v.strip() != ""


def _bool(v: Any) -> bool:
    return isinstance(v, bool)


def _parse_expect(raw: Any, where: str, problems: list[str]) -> Expect:
    if not isinstance(raw, Mapping):
        problems.append(f"{where}: expect がオブジェクトではありません")
        raw = {}
    trig = raw.get("trigger")
    if trig is not None:
        if not isinstance(trig, Mapping) or trig.get("kind") not in TRIGGER_KINDS:
            problems.append(f"{where}.trigger.kind は {TRIGGER_KINDS} のどれかです")
        elif trig.get("kind") == "schedule":
            crons = trig.get("cron_any")
            if not isinstance(crons, list) or not crons or not all(_str(c) for c in crons):
                problems.append(f"{where}.trigger.cron_any は空でない文字列の配列です")
            if not _str(trig.get("tz")):
                problems.append(f"{where}.trigger.tz がありません")
    nt = raw.get("node_types") or {}
    if not isinstance(nt, Mapping):
        problems.append(f"{where}.node_types がオブジェクトではありません")
        nt = {}
    for k, v in nt.items():
        if k not in ("all_of", "any_of", "none_of"):
            problems.append(f"{where}.node_types に知らない項目があります: {k}")
        elif k == "any_of":
            if not isinstance(v, list) or not all(
                isinstance(g, list) and g and all(_str(t) for t in g) for g in v
            ):
                problems.append(f"{where}.node_types.any_of は種類の配列の配列です")
        elif not isinstance(v, list) or not all(_str(t) for t in v):
            problems.append(f"{where}.node_types.{k} は種類の配列です")
    inputs: list[InputSpec] = []
    for i, item in enumerate(raw.get("inputs") or []):
        w = f"{where}.inputs[{i}]"
        if not isinstance(item, Mapping):
            problems.append(f"{w}: オブジェクトではありません")
            continue
        key, terms = item.get("key"), item.get("terms")
        if not isinstance(key, str) or not _INPUT_KEY.match(key):
            problems.append(f"{w}.key が入力の key の形ではありません: {key!r}")
            continue
        if not isinstance(terms, list) or not terms or not all(_str(t) for t in terms):
            problems.append(f"{w}.terms（依頼文で当たる語）は空でない文字列の配列です")
            terms = []
        req = item.get("required", True)
        if not _bool(req):
            problems.append(f"{w}.required は真偽です")
            req = True
        inputs.append(InputSpec(key, tuple(terms), req))
    keys = [i.key for i in inputs]
    if len(set(keys)) != len(keys):
        problems.append(f"{where}.inputs の key が重複しています")
    flow = raw.get("flow_through") or []
    if not isinstance(flow, list) or not all(isinstance(f, str) for f in flow):
        problems.append(f"{where}.flow_through は入力の key の配列です")
        flow = []
    for f in flow:
        if f not in keys:
            problems.append(f"{where}.flow_through の {f!r} が inputs にありません")
    return Expect(trig, dict(nt), tuple(inputs), tuple(flow))


def _parse_step(raw: Any, where: str, problems: list[str]) -> RunStep:
    if not isinstance(raw, Mapping):
        problems.append(f"{where}: オブジェクトではありません")
        raw = {}
    inp = raw.get("input", {})
    if not isinstance(inp, Mapping):
        problems.append(f"{where}.input はオブジェクトです")
        inp = {}
    sched = raw.get("scheduled_at")
    if sched is not None and not _str(sched):
        problems.append(f"{where}.scheduled_at は日時の文字列です")
    trig = raw.get("trigger")
    if trig is not None and not isinstance(trig, Mapping):
        problems.append(f"{where}.trigger はオブジェクトです")
        trig = None
    wf = raw.get("workflow")
    if wf is not None and not _str(wf):
        problems.append(f"{where}.workflow は役割の名前です")
    return RunStep(wf, dict(inp), sched, dict(trig) if trig is not None else None)


def _check_matchers(exp: Mapping, where: str, refs: Mapping[str, str], problems: list[str]):
    st = exp.get("status", "succeeded")
    if st not in STATUSES:
        problems.append(f"{where}.status は {STATUSES} のどれかです")
    for i, c in enumerate(exp.get("checks") or []):
        w = f"{where}.checks[{i}]"
        if not isinstance(c, Mapping) or c.get("op") not in MATCH_OPS:
            problems.append(f"{w}.op は {MATCH_OPS} のどれかです")
            continue
        if not isinstance(c.get("path"), str) or not c["path"].startswith("/"):
            problems.append(f"{w}.path は / で始まる JSON Pointer です")
        if c["op"] != "absent" and "value" not in c:
            problems.append(f"{w}.value がありません")
    for i, r in enumerate(exp.get("rows") or []):
        w = f"{where}.rows[{i}]"
        if not isinstance(r, Mapping) or refs.get(r.get("table")) != "table":
            problems.append(f"{w}.table が fixture のテーブルの ref ではありません")
            continue
        for k in r:
            if k not in ("table", "count", "where", "match", "absent", "unchanged"):
                problems.append(f"{w} に知らない項目があります: {k}")
    for i, f in enumerate(exp.get("files") or []):
        w = f"{where}.files[{i}]"
        if not isinstance(f, Mapping) or not _str(f.get("name_regex")):
            problems.append(f"{w}.name_regex がありません")
            continue
        folder = f.get("folder")
        if folder is not None and refs.get(folder) != "folder":
            problems.append(f"{w}.folder が fixture のフォルダーの ref ではありません")
        try:
            re.compile(f["name_regex"])
        except re.error as e:
            problems.append(f"{w}.name_regex が正規表現ではありません: {e}")
    for flag in ("no_new_rows", "flow_through"):
        if not _bool(exp.get(flag, False)):
            problems.append(f"{where}.{flag} は真偽です")


def _parse_case(
    raw: Any,
    where: str,
    task_kind: str,
    roles: Mapping[str, Role],
    refs: Mapping[str, str],
    problems: list[str],
) -> RunCase | None:
    if not isinstance(raw, Mapping):
        problems.append(f"{where}: オブジェクトではありません")
        return None
    cid = raw.get("id")
    if not isinstance(cid, str) or not _CASE_ID.match(cid):
        problems.append(f"{where}.id の形が違います: {cid!r}")
        cid = "?"
    for flag in ("llm_dependent", "stub_ok"):
        if not _bool(raw.get(flag)):
            problems.append(f"{where}.{flag}（真偽）がありません")
    if "sequence" in raw:
        if any(k in raw for k in ("input", "scheduled_at", "trigger", "workflow")):
            problems.append(f"{where}: sequence と input などは一緒に書きません")
        seq = raw.get("sequence")
        if not isinstance(seq, list) or not seq:
            problems.append(f"{where}.sequence は空でない配列です")
            seq = []
        steps = [_parse_step(s, f"{where}.sequence[{i}]", problems) for i, s in enumerate(seq)]
    else:
        steps = [_parse_step(raw, where, problems)]
    for i, s in enumerate(steps):
        role_name = s.workflow or "main"
        if task_kind == "workflow_set" and s.workflow is None:
            problems.append(f"{where}: workflow_set の起動には workflow（役割）が要ります")
            continue
        if role_name not in roles:
            problems.append(f"{where}: 役割がありません: {s.workflow!r}")
            continue
        exp = roles[role_name].expect
        keys = exp.input_keys
        extra = sorted(set(s.input) - keys)
        if extra:
            problems.append(
                f"{where}（{i} 段目）: 入力に expect.inputs に無い key があります: {extra}"
            )
        missing = sorted(x.key for x in exp.inputs if x.required and x.key not in s.input)
        if missing and roles[role_name].trigger_kind == "interactive":
            problems.append(f"{where}（{i} 段目）: 必須の入力がありません: {missing}")
    exp = raw.get("expect")
    if not isinstance(exp, Mapping):
        problems.append(f"{where}.expect がありません")
        exp = {}
    _check_matchers(exp, f"{where}.expect", refs, problems)
    return RunCase(
        cid, tuple(steps), bool(raw.get("llm_dependent")), bool(raw.get("stub_ok")), dict(exp)
    )


def _check_fixture_refs(raw: Mapping, where: str, problems: list[str]) -> dict[str, str]:
    refs: dict[str, str] = {}
    fx = raw.get("fixtures") or {}
    if not isinstance(fx, Mapping):
        problems.append(f"{where}.fixtures がオブジェクトではありません")
        return refs
    for k in fx:
        if k not in FIXTURE_KIND.values():
            problems.append(f"{where}.fixtures に知らない項目があります: {k}")
    for kind, key in FIXTURE_KIND.items():
        items = fx.get(key) or []
        if not isinstance(items, list):
            problems.append(f"{where}.fixtures.{key} は配列です")
            continue
        for i, item in enumerate(items):
            w = f"{where}.fixtures.{key}[{i}]"
            if not isinstance(item, Mapping):
                problems.append(f"{w}: オブジェクトではありません")
                continue
            ref = item.get("ref")
            if not isinstance(ref, str) or not _REF.match(ref):
                problems.append(f"{w}.ref の形が違います: {ref!r}")
            elif ref in refs:
                problems.append(f"{w}.ref が重複しています: {ref}")
            else:
                refs[ref] = kind
            if not _str(item.get("name")):
                problems.append(f"{w}.name がありません")
    return refs


def parse_task(raw: Mapping[str, Any]) -> Task:
    """1 課題を読み、形が違えば TaskError（理由を全部入れる）。"""
    problems: list[str] = []
    if not isinstance(raw, Mapping):
        raise TaskError(["課題がオブジェクトではありません"])
    tid = raw.get("id")
    where = f"課題 {tid!r}"
    if not isinstance(tid, str) or not _TASK_ID.match(tid):
        problems.append(f"{where}: id の形が違います")
    known = {
        "id",
        "app",
        "kind",
        "requires",
        "phrasings",
        "clarifications",
        "fixtures",
        "expect",
        "roles",
        "run_cases",
        "reference",
        "notes",
        "ask_terms",
        "date_dependent",
    }
    for k in raw:
        if k not in known:
            problems.append(f"{where}: 知らない項目があります: {k}")
    kind = raw.get("kind")
    if kind not in KINDS:
        problems.append(f"{where}.kind は {KINDS} のどれかです")
    reqs = raw.get("requires") or []
    if not isinstance(reqs, list) or not all(isinstance(r, str) for r in reqs):
        problems.append(f"{where}.requires は文字列の配列です")
        reqs = []
    for r in reqs:
        head, _, rest = r.partition(":")
        if head not in REQUIRE_KINDS or not rest:
            problems.append(f"{where}.requires の形が違います（{REQUIRE_KINDS}:<名前>）: {r!r}")
    ph = raw.get("phrasings")
    if (
        not isinstance(ph, Mapping)
        or set(ph) != set(PHRASINGS)
        or not all(_str(ph[k]) for k in PHRASINGS)
    ):
        problems.append(f"{where}.phrasings は {PHRASINGS} の 3 つの空でない文です")
        ph = {k: "" for k in PHRASINGS}
    if not _str(raw.get("clarifications")):
        problems.append(f"{where}.clarifications（聞き返しへの答え）がありません")
    terms = raw.get("ask_terms", [])
    if not isinstance(terms, list) or not all(_str(t) for t in terms):
        problems.append(f"{where}.ask_terms（聞き返しと判定する語）は空でない文字列の配列です")
    if "date_dependent" in raw and not _bool(raw["date_dependent"]):
        problems.append(f"{where}.date_dependent（実行の日に決まる値を持つか）は真偽です")
    refs = _check_fixture_refs(raw, where, problems)
    roles: dict[str, Role] = {}
    if kind == "workflow_set":
        if "expect" in raw:
            problems.append(f"{where}: workflow_set は expect を roles に書きます")
        for i, r in enumerate(raw.get("roles") or []):
            w = f"{where}.roles[{i}]"
            if not isinstance(r, Mapping) or not _str(r.get("role")):
                problems.append(f"{w}.role がありません")
                continue
            if r.get("trigger_kind") not in TRIGGER_KINDS:
                problems.append(f"{w}.trigger_kind は {TRIGGER_KINDS} のどれかです")
            roles[r["role"]] = Role(
                r["role"],
                r.get("trigger_kind", "interactive"),
                _parse_expect(r.get("expect"), f"{w}.expect", problems),
            )
        if len(roles) < 2:
            problems.append(f"{where}: workflow_set の roles は 2 つ以上です")
        kinds = [r.trigger_kind for r in roles.values()]
        if len(set(kinds)) != len(kinds):
            problems.append(f"{where}: 役割はきっかけの種類で割り当てるので、種類を重ねません")
    else:
        if "roles" in raw:
            problems.append(f"{where}: roles は workflow_set だけに書きます")
        exp = _parse_expect(raw.get("expect"), f"{where}.expect", problems)
        tk = (exp.trigger or {}).get("kind", "interactive")
        roles["main"] = Role("main", tk, exp)
    cases: list[RunCase] = []
    rc = raw.get("run_cases")
    if not isinstance(rc, list) or not rc:
        problems.append(f"{where}.run_cases は空でない配列です")
        rc = []
    for i, c in enumerate(rc):
        case = _parse_case(c, f"{where}.run_cases[{i}]", kind, roles, refs, problems)
        if case:
            cases.append(case)
    ids = [c.id for c in cases]
    if len(set(ids)) != len(ids):
        problems.append(f"{where}: run_case の id が重複しています")
    ref = raw.get("reference")
    if not isinstance(ref, Mapping):
        problems.append(f"{where}.reference がありません")
        ref = {}
    irs: Mapping = {}
    if kind == "workflow_set":
        irs = ref.get("irs") if isinstance(ref.get("irs"), Mapping) else {}
        if set(irs) != set(roles):
            problems.append(f"{where}.reference.irs の役割が roles と違います")
    else:
        irs = {"main": ref.get("ir")} if isinstance(ref.get("ir"), Mapping) else {}
        if not irs:
            problems.append(f"{where}.reference.ir（正解の IR）がありません")
    for role, ir in irs.items():
        if not isinstance(ir, Mapping) or not isinstance(ir.get("nodes"), list):
            problems.append(f"{where}.reference（{role}）が IR ではありません")
            continue
        for at, k, r in iter_placeholders(ir):
            if k == "?":
                problems.append(f"{where}.reference（{role}）{at}: 置き場所の印の形が違います: {r}")
            elif refs.get(r) != k:
                problems.append(f"{where}.reference（{role}）{at}: fixture に無い {k} です: {r}")
    if problems:
        raise TaskError(problems)
    dated = _date_dependent(raw, cases)
    roles = {
        k: dataclasses.replace(r, expect=dataclasses.replace(r.expect, date_dependent=dated))
        for k, r in roles.items()
    }
    return Task(
        id=tid,
        app=raw.get("app"),
        kind=kind,
        requires=tuple(reqs),
        phrasings=dict(ph),
        clarifications=raw["clarifications"],
        fixtures=dict(raw.get("fixtures") or {}),
        roles=tuple(roles.values()),
        run_cases=tuple(cases),
        reference=dict(ref),
        raw=raw,
    )


def load_tasks(path: Path = CORE_TASKS) -> list[Task]:
    """課題集を読む（形の誤りは全課題の分をまとめて TaskError）。"""
    doc = json.loads(Path(path).read_text(encoding="utf-8"))
    if (
        not isinstance(doc, Mapping)
        or doc.get("version") != 1
        or not isinstance(doc.get("tasks"), list)
    ):
        raise TaskError([f'{path}: {{"version": 1, "tasks": [...]}} の形ではありません'])
    problems: list[str] = []
    tasks: list[Task] = []
    seen: set[str] = set()
    for raw in doc["tasks"]:
        try:
            t = parse_task(raw)
        except TaskError as e:
            problems += e.problems
            continue
        if t.id in seen:
            problems.append(f"課題の id が重複しています: {t.id}")
        seen.add(t.id)
        tasks.append(t)
    if problems:
        raise TaskError(problems)
    return tasks


def stub_cases(task: Task) -> list[RunCase]:
    """stub の LLM で流してよい run_case。"""
    return [c for c in task.run_cases if c.stub_ok]

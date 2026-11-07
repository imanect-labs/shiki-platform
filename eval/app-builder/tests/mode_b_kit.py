"""Mode B のオフラインのテストの部品（偽の時計・文脈・台本）。"""

from __future__ import annotations

import datetime as dt

from abeval import fixtures
from abeval.approvals import ApprovalPolicy
from abeval.cache import TrialCache
from abeval.config import load_config
from abeval.grade.static import LintResult
from abeval.mode_b import ModeBContext
from abeval.shiki import ShikiClient
from abeval.tasks import load_tasks
from abeval.trial import TrialSpec
from fake_chat import ChatFake
from recorded import catalog

TASKS = {t.id: t for t in load_tasks()}
# 偽の「今」（日本時間 2026-10-21 水曜 10 時）。課題の予定の週（2026-11-09〜15）から外す（測る日の守り・BUG-247）。
FIXED_NOW = dt.datetime(2026, 10, 21, 1, 0, tzinfo=dt.UTC)


class FakeClock:
    def __init__(self):
        self.t = 0.0
        self.sleeps: list[float] = []

    def __call__(self) -> float:
        return self.t

    def sleep(self, s: float) -> None:
        self.sleeps.append(s)
        self.t += s


def fake_lint(items):
    return {i: LintResult(id=i) for i, _ in items}


def make_ctx(
    fake: ChatFake, *, approve=(), stub=False, ctrl=None, run_id=None, now=FIXED_NOW
) -> ModeBContext:
    cfg = load_config()
    client = ShikiClient("http://localhost:8080", "alice", transport=fake.transport())
    fixtures.init_sentinel(client)
    clock = FakeClock()
    return ModeBContext(
        cfg=cfg,
        client=client,
        policy=ApprovalPolicy(frozenset(approve)),
        lint=fake_lint,
        catalog=catalog(),
        cache=TrialCache(cfg.paths.cache),
        binary_sha="b" * 64,
        config_sha="c" * 64,
        capture_sha="a" * 64,
        ctrl=ctrl,
        run_id=run_id,
        stub=stub,
        clock=clock,
        sleep=clock.sleep,
        now=lambda: now,
    )


def spec(task: str, n: int = 0, effort: str | None = "medium", phrasing: str = "flow") -> TrialSpec:
    return TrialSpec("e0", "b", "v0", task, phrasing, n, effort)


def tool_call(i: str, name: str, inp: dict, step: int = 0) -> dict:
    return {"type": "tool_call", "id": i, "name": name, "input": inp, "step": step}


def tool_result(i: str, ok: bool, content: str = "") -> dict:
    return {"type": "tool_result", "tool_call_id": i, "ok": ok, "content": content}


def workflow_ref(wid: str, name: str, version: int = 1) -> dict:
    return {
        "type": "workflow_ref",
        "workflow": {"id": wid, "name": name, "display_name": name, "version": version},
    }

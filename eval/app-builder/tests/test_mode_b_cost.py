"""Mode B の費用の割り当て（会話の分は会話 ID・run_case の分は active）と、製品の会計との突き合わせ。"""

from __future__ import annotations

from abeval import mode_b
from abeval.config import load_config
from abeval.ledger import Ledger
from fake_chat import ChatFake, RunScript, text_message
from fake_upstream import FakeUpstream
from inproc import running_proxy
from mode_b_kit import TASKS, make_ctx, spec, workflow_ref


def test_reconcile_tolerance():
    assert mode_b.reconcile(10_000, 10_050)["ok"]  # 0.5%
    assert not mode_b.reconcile(10_000, 10_200)["ok"]  # 2%
    assert mode_b.reconcile(0, 1)["ok"]  # 丸めの 1 マイクロ USD
    assert mode_b.reconcile(5, None) == {"ledger_micros": 5, "usage_micros": None, "ok": None}


def _charge(fake: ChatFake, ts, run_id: str, chat: int, usage: int):
    """会話が終わったときに、abeval.proxy が書くはずの行（会話 ID つき）を台帳に足し、製品の会計も増やす。"""

    def done(thread_id: str):
        Ledger(load_config().paths.ledger).append(
            {
                "cost_micros": chat,
                "trial": ts.trial_id,
                "run_id": run_id,
                "session": thread_id,
                "exp": "e0",
            }
        )
        fake.usage_micros += usage

    return done


def test_costs_are_split_by_session_and_active_and_reconciled():
    task = TASKS["basic-summary-file"]
    ts = spec("basic-summary-file")
    ir = task.reference_irs()["main"]
    fake = ChatFake()
    wid = "00000000-0000-4000-8000-000000000001"
    fake.workflows[wid] = {
        "id": wid,
        "tenant": "a-corp",
        "name": "summary",
        "ir": ir,
        "version": 1,
        "updated_at": "x",
    }
    with FakeUpstream() as up, running_proxy(up.url) as proxy, proxy.control() as ctrl:
        ctrl.register_run("r1", "e0", 1.0)
        calls = []
        real_set = ctrl.set_active

        def spy(run_id, trial=None):
            calls.append((run_id, trial))
            if run_id is not None:
                # run_case の llm.invoke の分（会話 ID の無い要求）は active の試行に入る。
                Ledger(load_config().paths.ledger).append(
                    {
                        "cost_micros": 700,
                        "trial": trial,
                        "run_id": run_id,
                        "session": None,
                        "exp": "e0",
                    }
                )
                fake.usage_micros += 700
            return real_set(run_id, trial)

        ctrl.set_active = spy
        fake.scripts.append(
            RunScript(
                messages=[text_message("保存しました。", workflow_ref(wid, "summary"))],
                on_done=_charge(fake, ts, "r1", 3_000, 3_000),
            )
        )
        ctx = make_ctx(fake, ctrl=ctrl, run_id="r1")
        trial, errors = mode_b.run_trial(ctx, task, ts)
        assert proxy.state.spend()["active"] is None
    assert [e for e in errors if e["kind"] != "leftover"] == []
    assert calls == [("r1", ts.trial_id), (None, None)]
    c = trial["cost"]
    assert c["chat_micros"] == 3_000 and c["run_case_micros"] == 700 and c["total_micros"] == 3_700
    assert c["reconcile"]["ok"] is True and c["reconcile"]["usage_micros"] == 3_700


def test_mismatch_with_the_product_accounting_is_reported():
    task = TASKS["basic-summary-file"]
    ts = spec("basic-summary-file", n=1)
    fake = ChatFake()
    fake.scripts.append(
        RunScript(
            messages=[text_message("できません。")], on_done=_charge(fake, ts, "r1", 1_000, 5_000)
        )
    )
    with FakeUpstream() as up, running_proxy(up.url) as proxy, proxy.control() as ctrl:
        ctrl.register_run("r1", "e0", 1.0)
        trial, _ = mode_b.run_trial(make_ctx(fake, ctrl=ctrl, run_id="r1"), task, ts)
    rec = trial["cost"]["reconcile"]
    assert rec["ok"] is False and rec["ledger_micros"] == 1_000 and rec["usage_micros"] == 5_000


def test_stub_runs_have_no_cost_lines():
    fake = ChatFake(scripts=[RunScript(messages=[text_message("回答: x")])])
    ctx = make_ctx(fake, stub=True)
    trial, _ = mode_b.run_trial(ctx, TASKS["basic-summary-file"], spec("basic-summary-file"))
    assert trial["cost"]["total_micros"] == 0
    assert fake.posted[0] == {
        "text": TASKS["basic-summary-file"].phrasings["flow"],
        "model": "default",
    }

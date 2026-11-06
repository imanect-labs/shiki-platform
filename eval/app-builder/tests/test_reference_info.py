"""依頼文に添える参考情報（入力の項目の key・fixture の id）。Mode A と Mode B で同じものを添える（GAP-30）。

run_case は課題の key で入力を送るので、key を知らせないと、意味として正しいワークフローでも名前の違いだけで
input_ref・flow_through（S2s）と 400 invalid_input（S3・S4）になる。
"""

from __future__ import annotations

import pytest

from abeval.tasks import load_tasks
from abeval.trial import input_lines, placeholder_ids, prompt_text, reference_info

TASKS = {t.id: t for t in load_tasks()}


@pytest.mark.parametrize("task", list(TASKS.values()), ids=lambda t: t.id)
def test_every_input_key_of_the_task_is_named_in_the_reference(task):
    info = reference_info(task, placeholder_ids(task))
    for role in task.roles:
        for spec in role.expect.inputs:
            assert f"{spec.key}（{spec.terms[0]}・" in info, (task.id, spec.key)
    # run_case が送る key は、どれも参考情報にある。
    for case in task.run_cases:
        for step in case.steps:
            for key in step.input:
                assert f"{key}（" in info, (task.id, case.id, key)


def test_types_come_from_the_run_case_values():
    lines = "\n".join(input_lines(TASKS["basic-map-rows"]))
    assert "event（イベント名・文字）" in lines and "names（氏名・文字の配列）" in lines
    assert "amount（金額・数値）" in "\n".join(input_lines(TASKS["basic-branch-amount"]))


def test_workflow_sets_name_the_role_by_its_trigger():
    lines = input_lines(TASKS["app03-pair"])
    assert len(lines) == 1 and lines[0].startswith("- 手で動かすワークフローの入力の項目")


def test_tasks_without_inputs_or_fixtures_have_no_input_line():
    assert input_lines(TASKS["app03-weekly-summary"]) == []


def test_the_prompt_uses_real_ids_and_the_key_uses_placeholders():
    t = TASKS["basic-summary-file"]
    real = prompt_text(
        t, "flow", {"out": "11111111-1111-4111-8111-111111111111"}, with_reference=True
    )
    key = prompt_text(t, "flow", placeholder_ids(t), with_reference=True)
    assert real.startswith(t.phrasings["flow"] + "\n\n参考情報")
    assert "11111111-1111-4111-8111-111111111111" in real and "@folder:out" in key
    assert "title（件名・文字）、body（本文・文字）" in real

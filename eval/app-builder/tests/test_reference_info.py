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


def test_tables_with_a_state_flow_name_the_status_column_the_first_state_and_the_moves():
    """状態の進め方のあるテーブルでは、足した行の状態が最初の状態に決まり、状態の列はほかの書き方で変えられない。
    テーブルの性質なので参考情報に載せる（どのブロックで進めるかは書かない・GAP-113）。"""
    info = reference_info(TASKS["app12-faq-table"], placeholder_ids(TASKS["app12-faq-table"]))
    line = next(x for x in info.splitlines() if "問い合わせ記録" in x)
    assert line.endswith(
        "。状態の進め方: 状態の列は status、追加したときの状態は「自動回答」、進め方は 自動回答 → エスカレーション"
    )
    assert "data.transition" not in info
    claims = reference_info(TASKS["app01-posting-batch"], {"claims": "c", "journal": "j"})
    assert "進め方は 申請中 → 承認済、申請中 → 差戻し、差戻し → 申請中、承認済 → 計上済" in claims
    # 状態の進め方の無いテーブルには載せない。
    assert "状態の進め方" not in next(x for x in claims.splitlines() if "仕訳台帳" in x)
    assert "状態の進め方" not in reference_info(
        TASKS["app06-stock-move"], placeholder_ids(TASKS["app06-stock-move"])
    )

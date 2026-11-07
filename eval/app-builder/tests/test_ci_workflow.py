"""CI の起こし方（GAP-110）: dev-up.sh だけを変える PR と main への push でも、このテスト群が走ること。

ci.yml は `on:` の paths-ignore で `.claude/**` を外す（docs だけの PR で重いジョブを起こさない・変えない）。
eval-app-builder の手順は `.github/workflows/eval-app-builder.yml` にだけ置き、ci.yml はそれを呼ぶ。あちらは
`.claude/skills/pr/scripts/**` の変更でも自分で起きる。YAML の読み手を足さないよう、行で確かめる。
"""

from __future__ import annotations

import re

from abeval.config import ROOT

WORKFLOWS = ROOT.parent.parent / ".github" / "workflows"
EVAL = WORKFLOWS / "eval-app-builder.yml"
CI = WORKFLOWS / "ci.yml"
SCRIPTS = '".claude/skills/pr/scripts/**"'


def block(text: str, key: str, indent: int) -> list[str]:
    """`key:` の行（字下げ indent）の下の、より深く字下げした行（次の同じ深さの行の前まで）。"""
    lines = text.splitlines()
    pad = " " * indent
    start = next(i for i, line in enumerate(lines) if line == f"{pad}{key}:")
    out = []
    for line in lines[start + 1 :]:
        if line.strip() and not line.startswith(pad + " "):
            break
        out.append(line)
    return out


def test_the_eval_workflow_runs_for_dev_up_changes_and_when_called():
    text = EVAL.read_text(encoding="utf-8")
    on = block(text, "on", 0)
    assert "  workflow_call:" in on
    pr = block("\n".join(on), "pull_request", 2)
    push = block("\n".join(on), "push", 2)
    assert any(SCRIPTS in line for line in pr), pr
    assert any(SCRIPTS in line for line in push) and any("[main]" in line for line in push), push
    # 手順は CI と同じ組（not server・not lint_bin）。dev-up の記録と比べるテスト（shell の印）は外さない。
    assert 'uv run pytest -m "not server and not lint_bin" -q' in text
    assert "shell" not in re.search(r'pytest -m "([^"]+)"', text).group(1)


def test_ci_calls_the_eval_workflow_and_keeps_its_triggers():
    ci = CI.read_text(encoding="utf-8")
    job = block(ci, "eval-app-builder", 2)
    assert "    uses: ./.github/workflows/eval-app-builder.yml" in job
    # 手順を二重に持たない（steps は呼ぶ先にだけある）。
    assert not any(line.strip() == "steps:" for line in job)
    # ci.yml の起こし方は変えない（docs・**.md・.claude/** だけの変更では起こさない）。
    for event in ("pull_request", "push"):
        ignore = block("\n".join(block(ci, "on", 0)), event, 2)
        assert '      - ".claude/**"' in ignore and '      - "docs/**"' in ignore, ignore


def test_actions_are_pinned_to_the_same_shas_as_ci():
    ci = CI.read_text(encoding="utf-8")
    uses = re.findall(r"uses: ([\w./-]+)@([0-9a-f]{40})", EVAL.read_text(encoding="utf-8"))
    assert uses, "SHA で固定した action がありません"
    for action, sha in uses:
        assert f"uses: {action}@{sha}" in ci, (action, sha)
    assert "@v" not in re.sub(r"#.*", "", EVAL.read_text(encoding="utf-8"))

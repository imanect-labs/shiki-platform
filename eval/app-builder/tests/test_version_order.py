"""版の順（`eval.toml` の `[report] version_order`・まとめで各版を直前の版と比べる）。

版の順は手で書くので、書き忘れと逆の順をここで捕まえる: 版の記録（`data/versions/`）の版がすべて載っていること、
git の先祖の順に逆らわないこと（後に書いた版の commit が前に書いた版の commit の先祖なら逆）。先祖の関係の無い組
（載せ直した v1 と vn など）は、どちらの順でもよい。
"""

from __future__ import annotations

import json
import subprocess

import pytest

from abeval.config import ROOT, ConfigError, load_config
from abeval.versions import VERSIONS_DIR

REPO = ROOT.parent.parent


def _write(tmp_path, report: str):
    from abeval.config import DEFAULT_CONFIG

    p = tmp_path / "eval.toml"
    p.write_text(DEFAULT_CONFIG.read_text(encoding="utf-8").split("\n[report]")[0] + report)
    return p


def test_the_order_is_read_and_optional(tmp_path):
    cfg = load_config(_write(tmp_path, '\n[report]\nversion_order = ["v0", "vm"]\n'))
    assert cfg.version_order == ("v0", "vm")
    # 書かなければ空（直前の版とは比べない）。
    assert load_config(_write(tmp_path, "\n")).version_order == ()


@pytest.mark.parametrize(
    "bad", ['version_order = ["v0", "v0"]', 'version_order = "v0"', 'version_order = ["v0", ""]']
)
def test_a_broken_order_is_refused(tmp_path, bad):
    with pytest.raises(ConfigError, match="version_order"):
        load_config(_write(tmp_path, f"\n[report]\n{bad}\n"))


def _records() -> dict[str, str]:
    """版の記録（リポジトリの data/versions/）のラベル → commit。"""
    return {
        p.stem: json.loads(p.read_text(encoding="utf-8"))["commit"]
        for p in sorted(VERSIONS_DIR.glob("*.json"))
    }


def test_every_recorded_version_is_in_the_order():
    order = load_config().version_order
    records = _records()
    assert records, "版の記録がありません"
    missing = sorted(set(records) - set(order))
    assert not missing, f"eval.toml の [report] version_order に足してください: {missing}"


def _present(commit: str) -> bool:
    r = subprocess.run(
        ["git", "cat-file", "-e", f"{commit}^{{commit}}"], cwd=REPO, capture_output=True
    )
    return r.returncode == 0


@pytest.mark.shell
def test_the_order_does_not_go_against_the_commit_history():
    order = load_config().version_order
    records = _records()
    commits = {label: records[label] for label in order if label in records}
    here = {label: c for label, c in commits.items() if _present(c)}
    checked = 0
    for i, earlier in enumerate(order):
        for later in order[i + 1 :]:
            a, b = here.get(earlier), here.get(later)
            if not a or not b or a == b:
                continue
            r = subprocess.run(
                ["git", "merge-base", "--is-ancestor", b, a], cwd=REPO, capture_output=True
            )
            assert r.returncode in (0, 1), r.stderr
            assert r.returncode == 1, f"{later} の commit が {earlier} の先祖です（順が逆）"
            checked += 1
    if not checked:
        pytest.skip("版の commit が手元にありません（浅い clone など）")

"""1 ファイル 1000 行以内（リポジトリの Rust の決まりを Python にもそろえる）。"""

from __future__ import annotations

from abeval.config import ROOT

LIMIT = 1000


def test_python_files_are_within_the_limit():
    files = sorted([*(ROOT / "abeval").rglob("*.py"), *(ROOT / "tests").rglob("*.py")])
    assert files, "対象のファイルが無い"
    over = {}
    for f in files:
        n = len(f.read_text(encoding="utf-8").splitlines())
        if n > LIMIT:
            over[str(f.relative_to(ROOT))] = n
    assert not over, f"{LIMIT} 行を超えています: {over}"

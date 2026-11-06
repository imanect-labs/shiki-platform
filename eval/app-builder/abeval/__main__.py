"""`python -m abeval <コマンド>`。

| コマンド | 中身 |
| --- | --- |
| `capture --label L --exp E` | 説明文と system を取り込む（$0・abeval.proxy の capture_only） |
| `doctor [--mode eval|stub]` | 評価の前の点検（abeval.doctor） |
| `plan --mode a|b --label L --exp E ...` | 計画と見積もり（費用を使わない） |
| `run --mode a|b --label L --exp E ... [--go]` | 既定は計画だけ。`--go` で実行する（`--stub`・`--regrade-from`） |
| `grade --exp E` | 結果の IR を ir_lint で採点し直す（サーバー不要） |
| `report [--exp E ...]` | results/summary.md を作る |
| `budget [--json]` | 費用の累計と残り |
| `serve --label L [--down]` | 版のサーバーを起動・停止する（abeval.versions） |

中継は `python -m abeval.proxy`。
"""

from __future__ import annotations

import sys


def _serve(argv: list[str]) -> int:
    from abeval import versions

    return versions.main(argv)


def _doctor(argv: list[str]) -> int:
    from abeval import doctor

    return doctor.main(argv)


def _budget(argv: list[str]) -> int:
    from abeval import budget

    return budget.main(argv)


def _cli(name: str):
    def run(argv: list[str]) -> int:
        from abeval import cli

        return cli.main(argv, command=name)

    return run


COMMANDS = {
    "serve": _serve,
    "doctor": _doctor,
    "budget": _budget,
    **{name: _cli(name) for name in ("capture", "plan", "run", "grade", "report")},
}


def main(argv: list[str] | None = None) -> int:
    argv = sys.argv[1:] if argv is None else argv
    if not argv or argv[0] not in COMMANDS:
        print(f"使い方: python -m abeval {{{','.join(COMMANDS)}}} ...", file=sys.stderr)
        return 2
    return COMMANDS[argv[0]](argv[1:])


if __name__ == "__main__":
    raise SystemExit(main())

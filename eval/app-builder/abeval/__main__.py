"""`python -m abeval <コマンド>`。コマンドは各モジュールの main に渡す（後の段が表に足す）。

- `serve --label L [--down]`: 版のサーバーを起動・停止する（abeval.versions）。
- 点検は `python -m abeval.doctor`、中継は `python -m abeval.proxy`。
"""

from __future__ import annotations

import sys


def _serve(argv: list[str]) -> int:
    from abeval import versions

    return versions.main(argv)


def _doctor(argv: list[str]) -> int:
    from abeval import doctor

    return doctor.main(argv)


COMMANDS = {"serve": _serve, "doctor": _doctor}


def main(argv: list[str] | None = None) -> int:
    argv = sys.argv[1:] if argv is None else argv
    if not argv or argv[0] not in COMMANDS:
        print(f"使い方: python -m abeval {{{','.join(COMMANDS)}}} ...", file=sys.stderr)
        return 2
    return COMMANDS[argv[0]](argv[1:])


if __name__ == "__main__":
    raise SystemExit(main())

"""偽の /proc（pid の下の exe・environ・stat・cwd と /proc/stat の btime）と、`ss` の出力の偽物。

`abeval.server_guard` を、本物のプロセスを起こさずに確かめるための部品。exe は本物と同じく symlink にする
（行き先の文字が /proc/<pid>/exe の readlink の値・開くと実行ファイルの中身）。
"""

from __future__ import annotations

import os
from pathlib import Path

BTIME = 1_700_000_000
CLK = os.sysconf("SC_CLK_TCK")


class FakeProc:
    def __init__(self, root: Path):
        self.root = Path(root)
        self.root.mkdir(parents=True, exist_ok=True)
        (self.root / "stat").write_text(f"cpu  1 2 3 4\nintr 0\nbtime {BTIME}\nprocesses 9\n")
        self.listening: list[int] = []

    def add(
        self,
        pid: int,
        *,
        exe: Path | str,
        env: dict[str, str],
        started_at: float,
        cwd: Path | None = None,
        listen: bool = True,
    ) -> int:
        d = self.root / str(pid)
        d.mkdir(parents=True, exist_ok=True)
        for name in ("exe", "cwd"):
            if (d / name).is_symlink():
                (d / name).unlink()
        (d / "exe").symlink_to(exe)
        if cwd is not None:
            (d / "cwd").symlink_to(cwd)
        (d / "environ").write_bytes(b"".join(f"{k}={v}".encode() + b"\0" for k, v in env.items()))
        ticks = round((started_at - BTIME) * CLK)
        # comm に空白と括弧を入れる（本物の stat の読み方と同じく、最後の ")" の後から数える）。
        fields = ["S", "1"] + ["0"] * 17 + [str(ticks)] + ["0"] * 10
        (d / "stat").write_text(f"{pid} (shiki (server) x) " + " ".join(fields) + "\n")
        if listen:
            self.listening = [pid]
        return pid

    def ss(self, port: int) -> str:
        return "".join(
            f'LISTEN 0 1024 127.0.0.1:{port} 0.0.0.0:* users:(("shiki-server",pid={p},fd=9))\n'
            for p in self.listening
        )

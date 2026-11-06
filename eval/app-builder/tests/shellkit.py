"""シェルのスクリプトを偽のコマンド（shim）で流すための補助（test_dev_up_args・test_build_version が使う）。

- shim は呼ばれた引数を 1 行ずつ `SHIM_LOG` に書き、決まった応答を返す（docker・cargo・curl などを本物で動かさない）。
- 流すときの環境は最小にする（PATH は shim → /usr/bin → /bin・HOME と TMPDIR はテストの一時の場所）。
  呼び出し側の SHIKI__* などを引き継がない。
"""

from __future__ import annotations

import contextlib
import os
import shutil
import signal
import subprocess
from dataclasses import dataclass
from pathlib import Path

from abeval.config import ROOT

REPO_ROOT = ROOT.parent.parent
DEV_UP = REPO_ROOT / ".claude" / "skills" / "pr" / "scripts" / "dev-up.sh"
BUILD_VERSION = ROOT / "scripts" / "build-version.sh"

# docker の shim: compose の健康・ポート・コンテナの env・psql に決まった値で答える。
DOCKER = r"""#!/usr/bin/env bash
printf 'docker %s\n' "$*" >> "$SHIM_LOG"
a="$*"
case "$a" in
  *" ps "*"--format"*) echo healthy ;;
  *"ps -q shiki-server"*) : ;;
  *" ps -q "*) echo cid ;;
  *"printenv POSTGRES_USER"*) echo postgres ;;
  *"printenv RUSTFS_ACCESS_KEY"*|*"printenv RUSTFS_SECRET_KEY"*) echo minioadmin ;;
  *"printenv"*) : ;;
  *"psql"*"SELECT 1 FROM pg_database"*) [ -n "${FAKE_DB_EXISTS:-}" ] && echo 1 ;;
  *"psql"*"to_regclass"*) echo f ;;
  *"psql"*"EXISTS"*) echo f ;;
  *"psql"*"shobj_description"*) printf '%s\n' "${FAKE_DB_TAG:-}" ;;
  *"psql"*) echo OK ;;
esac
exit 0
"""

# curl の shim: 応答コードを問われたら 200、それ以外は成功だけ返す。
CURL = r"""#!/usr/bin/env bash
printf 'curl %s\n' "$*" >> "$SHIM_LOG"
case "$*" in
  *"%{http_code}"*) printf 200 ;;
esac
exit 0
"""

# setsid の shim: 起動したことだけ記録し、生きているプロセスとして残る（kill -0 が通る）。
SETSID = r"""#!/usr/bin/env bash
printf 'setsid %s\n' "$*" >> "$SHIM_LOG"
exec sleep 60
"""

LOG_ONLY = r"""#!/usr/bin/env bash
printf '%s %s\n' "$(basename "$0")" "$*" >> "$SHIM_LOG"
exit 0
"""

LOG_FAIL = r"""#!/usr/bin/env bash
printf '%s %s\n' "$(basename "$0")" "$*" >> "$SHIM_LOG"
exit 1
"""

SHIMS = {
    "docker": DOCKER,
    "curl": CURL,
    "setsid": SETSID,
    "nohup": LOG_ONLY,
    "cargo": LOG_ONLY,
    "pnpm": LOG_ONLY,
    "fuser": LOG_FAIL,
    "pgrep": LOG_FAIL,
}


def write_exe(path: Path, text: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")
    path.chmod(0o755)
    return path


def make_shims(where: Path, overrides: dict[str, str] | None = None) -> Path:
    for name, text in {**SHIMS, **(overrides or {})}.items():
        write_exe(where / name, text)
    return where


def base_env(tmp: Path, shim_dir: Path | None, log: Path, extra: dict[str, str] | None = None):
    home = tmp / "home"
    home.mkdir(parents=True, exist_ok=True)
    tmpdir = tmp / "tmpdir"
    tmpdir.mkdir(parents=True, exist_ok=True)
    path = "/usr/bin:/bin"
    if shim_dir is not None:
        path = f"{shim_dir}:{path}"
    env = {
        "PATH": path,
        "HOME": str(home),
        "TMPDIR": str(tmpdir),
        "LANG": "C.UTF-8",
        "SHIM_LOG": str(log),
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": str(home / ".gitconfig"),
        "GIT_AUTHOR_NAME": "abeval",
        "GIT_AUTHOR_EMAIL": "abeval@example.invalid",
        "GIT_COMMITTER_NAME": "abeval",
        "GIT_COMMITTER_EMAIL": "abeval@example.invalid",
    }
    env.update(extra or {})
    return env


@dataclass
class Run:
    code: int
    out: str
    err: str
    log: list[str]


def run_bash(script: Path, args: list[str], *, cwd: Path, env: dict, log: Path, timeout=60) -> Run:
    log.write_text("", encoding="utf-8")
    p = subprocess.run(
        ["/usr/bin/bash", str(script), *args],
        cwd=cwd,
        env=env,
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )
    lines = log.read_text(encoding="utf-8").splitlines() if log.exists() else []
    return Run(p.returncode, p.stdout, p.stderr, lines)


def git(cwd: Path, *args: str, env: dict) -> str:
    p = subprocess.run(
        ["/usr/bin/git", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main", *args],
        cwd=cwd,
        env=env,
        capture_output=True,
        text=True,
        check=True,
    )
    return p.stdout.strip()


def kill_pidfiles(*dirs: Path) -> None:
    """PID ファイルに書かれたプロセス（setsid の shim の sleep）を止める。"""
    for d in dirs:
        if not d.is_dir():
            continue
        for f in d.glob("*.pid"):
            try:
                pid = int(f.read_text().strip())
            except ValueError:
                continue
            with contextlib.suppress(ProcessLookupError):
                os.kill(pid, signal.SIGKILL)


def bash_available() -> bool:
    return shutil.which("bash", path="/usr/bin:/bin") is not None and Path("/usr/bin/git").exists()

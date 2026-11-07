"""版のサーバーのプロセスを /proc から確かめる（BUG-246・GAP-30 §4.1・§4.2）。

`abeval run`（Mode A・Mode B・取り込み・`--regrade-from`）と `abeval capture` は、API のポート（:10067）を握る
プロセスを `ss` と /proc で読み、次を確かめてから流す（`--skip-doctor` でも飛ばさない・組み立ては
`doctor.version_guard`）。

- 版（`identity_checks`）: プロセスの実行ファイル（/proc/<pid>/exe の行き先）が、serve が渡す退避したバイナリ
  （`<home>/bin/shiki-server-<label>`）で、動いている実行ファイルそのもの（/proc/<pid>/exe を開いたもの）の sha256 が
  版の記録の binary_sha256 と同じ。CSV の集計のランナー（環境の `SHIKI__TABULAR__RUNNER_PATH`）も版のランナーで、
  sha が記録の runner_sha256 と同じ。
- LLM の行き先（`route_problem`・取り込みと Mode B の発話の前）: `SHIKI_CONFIG` の anthropic が abeval.proxy を指し、
  鍵がダミー。`SHIKI__LLM__*`（`SHIKI__LLM__BACKEND` を除く）の上書きが無い。設定のファイルがプロセスの起動の後に
  書き換えられていない（書き換えられていると、サーバーが起動のときに読んだ中身を確かめられない）。
- 条件（`condition_checks`・Mode B）: 環境の `SHIKI__CHAT__MAX_STEPS` が `--max-steps` と同じ（省いたなら無い）、
  `SHIKI__CHAT__BUDGET_MAX_COST_USD_MICROS` が `--budget-micros` と同じ（serve が渡す値）。
- 確かめたプロセス（pid・起動の時刻・実行ファイル）を `ServerIdentity` に控え、取り込みの前・Mode B の各試行の
  発話の前と結果を残す前・結果を書く前に、同じプロセスのままか（と LLM の行き先・条件）を確かめ直す（`recheck`）。
  pid と起動の時刻が同じなら動いている実行ファイルは替わらないので、sha は最初の 1 回だけ数える。

読めないもの（`ss` が無い・別の利用者のプロセス・/proc が読めない）は、確かめられないので NG にする。
"""

from __future__ import annotations

import os
import re
import subprocess
import tomllib
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from pathlib import Path

from abeval.config import Config, display_path
from abeval.proxy import DUMMY_CREDENTIAL
from abeval.versions import (
    SHA_RE,
    VersionError,
    binary_path,
    file_sha256,
    read_record,
    runner_path,
)

PROC = Path("/proc")
MAX_STEPS_ENV = "SHIKI__CHAT__MAX_STEPS"
BUDGET_ENV = "SHIKI__CHAT__BUDGET_MAX_COST_USD_MICROS"
RUNNER_ENV = "SHIKI__TABULAR__RUNNER_PATH"
# 設定の mtime とプロセスの起動の時刻を比べるときの許し（起動の時刻は btime と clock tick から出すので秒の精度）。
CONFIG_MTIME_SLACK_SECS = 2.0


@dataclass
class Check:
    name: str
    status: str  # ok | ng | warn | info
    detail: str = ""

    def line(self) -> str:
        mark = {"ok": "[ok]  ", "ng": "[NG]  ", "warn": "[warn]", "info": "[info]"}[self.status]
        return f"{mark} {self.name}: {self.detail}" if self.detail else f"{mark} {self.name}"


def run_ss(port: int) -> str | None:
    """`ss -ltnpH 'sport = :<port>'` の出力（ss が無ければ None）。"""
    try:
        return subprocess.run(
            ["ss", "-ltnpH", f"sport = :{port}"], capture_output=True, text=True, check=False
        ).stdout
    except FileNotFoundError:
        return None


@dataclass(frozen=True)
class Process:
    pid: int
    # /proc/<pid>/exe の行き先（消された実行ファイルは末尾に " (deleted)" が付く）。
    exe: str
    env: Mapping[str, str]
    # 起動の時刻（エポック秒・読めなければ None）。
    started_at: float | None
    # 作業の場所（SHIKI_CONFIG が相対のときに使う・読めなければ None）。
    cwd: str | None = None


def _started_at(pid: int, proc_root: Path) -> float | None:
    """/proc/<pid>/stat の starttime（起動からの clock tick）と /proc/stat の btime から出す。"""
    try:
        stat = (proc_root / str(pid) / "stat").read_text()
        rest = stat[stat.rindex(")") + 2 :].split()
        ticks = int(rest[19])  # 22 番目の項目（3 番目の state から数えて 20 個目）
        btime = next(
            int(line.split()[1])
            for line in (proc_root / "stat").read_text().splitlines()
            if line.startswith("btime ")
        )
    except (OSError, ValueError, IndexError, StopIteration):
        return None
    return btime + ticks / os.sysconf("SC_CLK_TCK")


def read_process(pid: int, proc_root: Path | None = None) -> Process | str:
    """プロセスの実行ファイル・環境・起動の時刻（読めなければ理由）。"""
    proc_root = proc_root or PROC
    base = proc_root / str(pid)
    try:
        exe = os.readlink(base / "exe")
        raw = (base / "environ").read_bytes().split(b"\0")
    except OSError as e:
        return (
            f"pid {pid} の /proc を読めません（{type(e).__name__}・同じ利用者のプロセスだけ読める）"
        )
    env = {}
    for kv in raw:
        k, sep, v = kv.partition(b"=")
        if sep:
            env[k.decode(errors="replace")] = v.decode(errors="replace")
    try:
        cwd: str | None = os.readlink(base / "cwd")
    except OSError:
        cwd = None
    return Process(pid, exe, env, _started_at(pid, proc_root), cwd)


def process_on_port(
    port: int, *, proc_root: Path | None = None, ss: Callable[[int], str | None] | None = None
) -> Process | str:
    """そのポートで待ち受けているただ 1 つのプロセス（違えば・読めなければ理由）。"""
    out = (ss or run_ss)(port)
    if out is None:
        return "ss がありません（ポートを握るプロセスを確かめられない）"
    lines = [line for line in out.splitlines() if line.strip()]
    if not lines:
        return f":{port} で待ち受けているプロセスがありません（版のサーバーを serve で起動する）"
    pids = sorted({int(p) for p in re.findall(r"pid=(\d+)", out)})
    if not pids:
        return f":{port} を握るプロセスが見えません（別の利用者のプロセス）"
    if len(pids) != 1:
        return f":{port} を握るプロセスが 1 つではありません: {pids}"
    return read_process(pids[0], proc_root)


def anthropic_points_to_the_proxy(path: Path, cfg: Config) -> str | None:
    """製品の設定の anthropic が abeval.proxy を指し、鍵がダミーか（違えば理由・値は出さない）。"""
    try:
        raw = tomllib.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as e:
        return f"{display_path(path)} を読めません: {type(e).__name__}"
    provs = [
        p for p in (raw.get("llm") or {}).get("providers") or [] if p.get("backend") == "anthropic"
    ]
    if len(provs) != 1:
        return f"anthropic のプロバイダが {len(provs)} 個"
    proxy = f"http://{cfg.proxy.host}:{cfg.proxy.port}"
    if str(provs[0].get("base_url", "")).rstrip("/") != proxy:
        return f"anthropic の base_url が abeval.proxy（{proxy}）ではない"
    if provs[0].get("api_key") != DUMMY_CREDENTIAL:
        return "anthropic の api_key がダミーではない（値は出さない）"
    return None


def config_path(proc: Process) -> Path | None:
    """プロセスの SHIKI_CONFIG（相対ならプロセスの作業の場所から）。無ければ None。"""
    raw = proc.env.get("SHIKI_CONFIG")
    if not raw:
        return None
    p = Path(raw).expanduser()
    if not p.is_absolute() and proc.cwd:
        p = Path(proc.cwd) / p
    return p


def route_problem(proc: Process, cfg: Config) -> str | None:
    """サーバーの anthropic が abeval.proxy を指していないなどの理由（無ければ None・値は出さない）。"""
    over = sorted(
        k for k in proc.env if k.startswith("SHIKI__LLM__") and k != "SHIKI__LLM__BACKEND"
    )
    if over:
        return f"LLM の設定を環境で上書きしています: {over}"
    path = config_path(proc)
    if path is None:
        return "SHIKI_CONFIG がありません"
    why = anthropic_points_to_the_proxy(path, cfg)
    if why:
        return why
    if proc.started_at is None:
        return f"pid {proc.pid} の起動の時刻を読めません（設定が起動の後に変わっていないか確かめられない）"
    try:
        mtime = path.stat().st_mtime
    except OSError as e:
        return f"{display_path(path)} を読めません: {type(e).__name__}"
    if mtime > proc.started_at + CONFIG_MTIME_SLACK_SECS:
        return (
            f"{display_path(path)} がサーバーの起動の後に書き換えられています（サーバーが読んだ中身を"
            "確かめられない・serve で起動し直す）"
        )
    return None


def identity_checks(
    proc: Process,
    label: str,
    home: Path,
    *,
    versions_dir: Path | None = None,
    proc_root: Path | None = None,
) -> list[Check]:
    """そのプロセスが label の版のサーバーか（実行ファイル・sha・ランナー）。"""
    proc_root = proc_root or PROC
    try:
        rec = read_record(label, versions_dir)
    except VersionError as e:
        return [Check("版の記録", "ng", str(e)[:300])]
    if not rec.tree_clean:
        return [Check("版の記録", "ng", f"{label} はきれいな commit のビルドではありません")]
    out: list[Check] = []
    want = binary_path(label, home).resolve()
    if proc.exe.endswith(" (deleted)"):
        out.append(
            Check(
                "版のバイナリ",
                "ng",
                f"pid {proc.pid} の実行ファイルは消された（置き換えられた）もの",
            )
        )
    elif proc.exe != str(want):
        out.append(
            Check(
                "版のバイナリ",
                "ng",
                f"pid {proc.pid} は {display_path(proc.exe)} で動いています（期待 {display_path(want)}）",
            )
        )
    else:
        try:
            got = file_sha256(proc_root / str(proc.pid) / "exe")
        except OSError as e:
            got = None
            out.append(Check("版のバイナリ", "ng", f"実行ファイルを読めません: {type(e).__name__}"))
        if got is not None:
            ok = got == rec.binary_sha256
            out.append(
                Check(
                    "版のバイナリ",
                    "ok" if ok else "ng",
                    f"{display_path(want)}（sha {got[:12]}…"
                    + ("）" if ok else f"・記録 {rec.binary_sha256[:12]}…）"),
                )
            )
    want_runner = rec.extra.get("runner_sha256")
    raw = proc.env.get(RUNNER_ENV)
    if not isinstance(want_runner, str) or not SHA_RE.match(want_runner):
        out.append(Check("版のランナー", "ng", "版の記録にランナーの sha がありません"))
    elif not raw:
        out.append(Check("版のランナー", "ng", f"{RUNNER_ENV} がありません"))
    else:
        rp = Path(raw)
        if not rp.is_absolute() and proc.cwd:
            rp = Path(proc.cwd) / rp
        expect = runner_path(label, home).resolve()
        if rp.resolve() != expect:
            out.append(
                Check(
                    "版のランナー",
                    "ng",
                    f"{display_path(rp)} を使っています（期待 {display_path(expect)}）",
                )
            )
        else:
            try:
                got_r = file_sha256(expect)
            except OSError as e:
                got_r = None
                out.append(Check("版のランナー", "ng", f"読めません: {type(e).__name__}"))
            if got_r is not None:
                ok = got_r == want_runner
                out.append(
                    Check(
                        "版のランナー",
                        "ok" if ok else "ng",
                        f"sha {got_r[:12]}…" + ("" if ok else f"（記録 {want_runner[:12]}…）"),
                    )
                )
    return out


def _env_int(raw: str | None) -> int | None | str:
    if raw is None:
        return None
    try:
        return int(raw)
    except ValueError:
        return raw


@dataclass(frozen=True)
class Conditions:
    """Mode B の条件（版のサーバーの起動の環境と同じであること）。"""

    max_steps: int | None
    budget_micros: int

    def problems(self, env: Mapping[str, str]) -> list[str]:
        out = []
        got = _env_int(env.get(MAX_STEPS_ENV))
        if got != self.max_steps:
            out.append(
                f"版のサーバーの手数の上限は {got if got is not None else '製品の既定'}、指定は "
                f"{self.max_steps if self.max_steps is not None else '製品の既定'}"
                "（serve --max-steps と run --max-steps をそろえる）"
            )
        got_b = _env_int(env.get(BUDGET_ENV))
        if got_b != self.budget_micros:
            out.append(
                f"版のサーバーの 1 発話の予算は {got_b if got_b is not None else '無し'}、指定は "
                f"{self.budget_micros}（serve --budget-micros と run --budget-micros をそろえる）"
            )
        return out


def condition_checks(proc: Process, cond: Conditions) -> list[Check]:
    bad = cond.problems(proc.env)
    if bad:
        return [Check("版のサーバーの条件", "ng", "・".join(bad))]
    steps = cond.max_steps if cond.max_steps is not None else "製品の既定"
    return [
        Check("版のサーバーの条件", "ok", f"手数の上限 {steps}・1 発話の予算 {cond.budget_micros}")
    ]


@dataclass
class ServerIdentity:
    """確かめた版のサーバーのプロセス（`recheck` で、同じプロセスのままかを確かめ直す）。"""

    port: int
    process: Process
    cfg: Config
    conditions: Conditions | None = None
    proc_root: Path | None = None
    ss: Callable[[int], str | None] | None = None

    def recheck(self) -> str | None:
        """替わっていれば・LLM の行き先や条件が合わなければ理由（同じなら None）。"""
        got = process_on_port(self.port, proc_root=self.proc_root, ss=self.ss)
        if isinstance(got, str):
            return f"版のサーバーを確かめ直せません: {got}"
        was = self.process
        if (got.pid, got.started_at, got.exe) != (was.pid, was.started_at, was.exe):
            return (
                f":{self.port} のサーバーが確かめた後に替わりました（pid {was.pid} → {got.pid}）。"
                "serve で版のサーバーを起動し直してから流し直す"
            )
        why = route_problem(got, self.cfg)
        if why:
            return f"版のサーバーの LLM の行き先: {why}"
        if self.conditions is not None:
            bad = self.conditions.problems(got.env)
            if bad:
                return "・".join(bad)
        return None

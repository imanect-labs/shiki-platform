"""版のサーバーのプロセスを /proc から確かめる（BUG-246・GAP-30 §4.1・§4.2）。

`abeval run`（Mode A・Mode B・取り込み・`--regrade-from`）と `abeval capture` は、API のポート（:10067）を握る
プロセスを `ss` と /proc で読み、次を確かめてから流す（`--skip-doctor` でも飛ばさない・組み立ては
`doctor.version_guard`）。

- 版（`identity_checks`）: プロセスの実行ファイル（/proc/<pid>/exe の行き先）が、serve が渡す退避したバイナリ
  （`<home>/bin/shiki-server-<label>`）で、動いている実行ファイルそのもの（/proc/<pid>/exe を開いたもの）の sha256 が
  版の記録の binary_sha256 と同じ。CSV の集計のランナー（環境の `SHIKI__TABULAR__RUNNER_PATH`）も版のランナーで、
  sha が記録の runner_sha256 と同じ。
- LLM の行き先（`route_problem`・取り込みと Mode B の発話の前）: `SHIKI_CONFIG` の anthropic が abeval.proxy を指し、
  鍵がダミー。既定のプロバイダ（モデル `default`・AI が組んだ IR の llm.invoke や製品の中の呼び出しが使う・製品の
  `wiring.rs` の `primary_provider`）が stub: 環境の `SHIKI__LLM__BACKEND` が serve の渡す `stub`、環境に無ければ
  TOML の `[llm] backend` が `stub`。`[llm]` の `base_url`・`api_key` が空。anthropic のほかのプロバイダと
  `[llm.chatgpt]` が無い。`SHIKI__LLM__BACKEND` のほかに llm の表を環境で上書きする名前が無い。環境の名前は、製品の
  figment（`Env::prefixed("SHIKI__").split("__")`）と同じく大文字・小文字を区別せずに読む（`shiki__llm__providers`・
  表をまるごと上書きする `SHIKI__LLM` も見る）。設定のファイルがプロセスの起動の後に書き換えられていない
  （書き換えられていると、サーバーが起動のときに読んだ中身を確かめられない）。
- 条件（`condition_checks`・Mode B）: 環境の `SHIKI__CHAT__MAX_STEPS` が `--max-steps` と同じ（省いたなら無い）、
  `SHIKI__CHAT__BUDGET_MAX_COST_USD_MICROS` が `--budget-micros` と同じ（serve が渡す値）。環境の名前は大文字・
  小文字を区別せずに読む（同じ設定を指す名前が 2 つあれば・chat の表をまるごと上書きしていれば NG）。
  `SHIKI_CONFIG` の `[chat]` に `max_steps`・`budget_max_cost_usd_micros` があれば NG（serve は環境でしか渡さない
  ので、`--max-steps` を省いたときにサーバーは TOML の値で動き、記録の「製品の既定」と食い違う）。
- 確かめたプロセス（pid・起動の時刻・実行ファイル）を `ServerIdentity` に控え、取り込みの前・Mode B の各試行の
  発話の前と結果を残す前・結果を書く前に、同じプロセスのままか（と LLM の行き先・条件）を確かめ直す（`recheck`）。
  pid と起動の時刻が同じなら動いている実行ファイルは替わらないので、sha は最初の 1 回だけ数える。同じプロセスかは
  起動の時刻の生の値（/proc/<pid>/stat の starttime・clock tick）で比べる（エポック秒は /proc/stat の btime から
  出すので、時計を跳ばす調整で動き、同じプロセスを「替わった」と誤って打ち切るため。エポック秒は設定のファイルの
  mtime と比べるときだけに使う）。

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
LLM_BACKEND_ENV = "SHIKI__LLM__BACKEND"
# serve が版のサーバーに渡す既定のプロバイダ（`versions.serve_plan`・llm-eval.toml の `[llm] backend` とそろえる）。
SERVE_BACKEND = "stub"
# Mode B の条件の `[chat]` の項目（環境の名前は SHIKI__CHAT__ に続けて大文字・`SHIKI__CHAT__MAX_STEPS` と
# `SHIKI__CHAT__BUDGET_MAX_COST_USD_MICROS`）。
CHAT_CONDITION_KEYS = ("max_steps", "budget_max_cost_usd_micros")
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
    # 起動の時刻（エポック秒・読めなければ None）。設定のファイルの mtime と比べるときだけに使う（btime は時計の
    # 調整で動くので、同じプロセスかの比べには使わない）。
    started_at: float | None
    # 作業の場所（SHIKI_CONFIG が相対のときに使う・読めなければ None）。
    cwd: str | None = None
    # /proc/<pid>/stat の starttime（OS の起動からの clock tick の生の値・読めなければ None）。同じプロセスかの比べに
    # 使う（時計を跳ばす調整〔timesyncd・chrony の step・うるう秒〕では変わらない）。
    start_ticks: int | None = None


def _start_ticks(pid: int, proc_root: Path) -> int | None:
    """/proc/<pid>/stat の starttime（22 番目の項目・comm の括弧の後から数える）。"""
    try:
        stat = (proc_root / str(pid) / "stat").read_text()
        rest = stat[stat.rindex(")") + 2 :].split()
        return int(rest[19])  # 22 番目の項目（3 番目の state から数えて 20 個目）
    except (OSError, ValueError, IndexError):
        return None


def _started_at(ticks: int | None, proc_root: Path) -> float | None:
    """starttime と /proc/stat の btime からエポック秒を出す（btime は時計の調整で動き得る）。"""
    if ticks is None:
        return None
    try:
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
    ticks = _start_ticks(pid, proc_root)
    return Process(pid, exe, env, _started_at(ticks, proc_root), cwd, ticks)


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


def _read_toml(path: Path) -> dict | str:
    try:
        return tomllib.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as e:
        return f"{display_path(path)} を読めません: {type(e).__name__}"


def anthropic_points_to_the_proxy(path: Path, cfg: Config) -> str | None:
    """製品の設定の anthropic が abeval.proxy を指し、鍵がダミーか（違えば理由・値は出さない）。"""
    raw = _read_toml(path)
    if isinstance(raw, str):
        return raw
    return _proxy_problem(raw, cfg)


def _providers(raw: Mapping) -> list[Mapping]:
    return [p for p in (raw.get("llm") or {}).get("providers") or [] if isinstance(p, Mapping)]


def _proxy_problem(raw: Mapping, cfg: Config) -> str | None:
    provs = [p for p in _providers(raw) if p.get("backend") == "anthropic"]
    if len(provs) != 1:
        return f"anthropic のプロバイダが {len(provs)} 個"
    proxy = f"http://{cfg.proxy.host}:{cfg.proxy.port}"
    if str(provs[0].get("base_url", "")).rstrip("/") != proxy:
        return f"anthropic の base_url が abeval.proxy（{proxy}）ではない"
    if provs[0].get("api_key") != DUMMY_CREDENTIAL:
        return "anthropic の api_key がダミーではない（値は出さない）"
    return None


def default_provider_problem(raw: Mapping, *, backend_from_env: bool) -> str | None:
    """既定のプロバイダ（ID `default`）が stub で、ほかに評価の台帳を通らない行き先が無いか（違えば理由・値は出さない）。

    `backend_from_env` は環境の `SHIKI__LLM__BACKEND` が stub であること（確かめた後）。無ければ TOML の
    `[llm] backend` を見る（製品の既定は vllm）。
    """
    llm = raw.get("llm") if isinstance(raw.get("llm"), Mapping) else {}
    if not backend_from_env and llm.get("backend") != SERVE_BACKEND:
        return (
            f"既定のプロバイダ（[llm] backend）が {SERVE_BACKEND} ではありません（環境の {LLM_BACKEND_ENV} も"
            "無い。モデル default の呼び出しが abeval.proxy を通らない）"
        )
    for key in ("base_url", "api_key"):
        if str(llm.get(key) or "").strip():
            return f"[llm] の {key} が空ではありません（stub の既定のプロバイダは使わない・値は出さない）"
    others = sorted(
        str(p.get("id") or p.get("backend"))
        for p in _providers(raw)
        if p.get("backend") != "anthropic"
    )
    if others:
        return f"anthropic のほかのプロバイダがあります: {others}（評価の要求は abeval.proxy だけを通す）"
    chatgpt = llm.get("chatgpt") if isinstance(llm.get("chatgpt"), Mapping) else {}
    if chatgpt.get("enabled"):
        return "[llm.chatgpt] が有効です（評価の台帳を通らない）"
    return None


def shiki_env_key(name: str) -> list[str] | None:
    """環境の名前が製品の設定のどこを上書きするか（大文字にした段の列。`SHIKI__` で始まらなければ None）。

    製品の figment（`Env::prefixed("SHIKI__").split("__")`）と同じ読み方をする: 前置きは大文字・小文字を区別せず、
    `__` で段に分け、空の段より後は入れ子にならない（`SHIKI__LLM____X` は llm の表をまるごと上書きする）。
    段が無い（`SHIKI__` だけ）なら設定の全体を上書きする。
    """
    up = name.upper()
    if not up.startswith("SHIKI__"):
        return None
    segs = up[len("SHIKI__") :].split("__")
    return segs[: segs.index("")] if "" in segs else segs


def env_overrides(env: Mapping[str, str], table: str) -> dict[str, list[tuple[str, str]]]:
    """その表（大文字の `LLM`・`CHAT`）を環境で上書きする値（表の中の場所 → [(名前, 値)]）。表や設定の全体を
    まるごと上書きする名前は場所 `""` に入れる。"""
    out: dict[str, list[tuple[str, str]]] = {}
    for name, value in env.items():
        segs = shiki_env_key(name)
        if segs is None or (segs and segs[0] != table):
            continue
        out.setdefault("__".join(segs[1:]), []).append((name, value))
    return out


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
    """サーバーの LLM の要求が abeval.proxy か stub にしか行かないと確かめられない理由（無ければ None・値は出さない）。"""
    llm_env = env_overrides(proc.env, "LLM")
    over = sorted(name for key, vs in llm_env.items() if key != "BACKEND" for name, _ in vs)
    if over:
        return f"LLM の設定を環境で上書きしています: {over}"
    backends = sorted({v.strip() for _, v in llm_env.get("BACKEND", [])})
    if backends and backends != [SERVE_BACKEND]:
        return (
            f"既定のプロバイダ（{LLM_BACKEND_ENV}）が {backends} です（serve が渡す {SERVE_BACKEND} だけを通す。"
            "モデル default の呼び出しが abeval.proxy を通らない）"
        )
    path = config_path(proc)
    if path is None:
        return "SHIKI_CONFIG がありません"
    raw = _read_toml(path)
    if isinstance(raw, str):
        return raw
    why = _proxy_problem(raw, cfg) or default_provider_problem(raw, backend_from_env=bool(backends))
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

    def problems(self, env: Mapping[str, str], chat: Mapping | None = None) -> list[str]:
        """違いの理由（`env` はサーバーの環境・`chat` は SHIKI_CONFIG の `[chat]` の表）。"""
        out = []
        chat_env = env_overrides(env, "CHAT")
        whole = sorted(name for name, _ in chat_env.get("", []))
        if whole:
            out.append(f"chat の表を環境でまるごと上書きしています: {whole}")
        in_toml = sorted(k for k in CHAT_CONDITION_KEYS if k in (chat or {}))
        if in_toml:
            out.append(
                f"版のサーバーの SHIKI_CONFIG の [chat] に {in_toml} があります（serve は環境でしか渡さないので、"
                "--max-steps を省くと TOML の値で動き記録の条件と食い違う・設定から消す）"
            )
        values = {}
        for key in CHAT_CONDITION_KEYS:
            got_vals = chat_env.get(key.upper(), [])
            if len(got_vals) > 1:
                names = sorted(name for name, _ in got_vals)
                out.append(f"同じ設定（chat.{key}）を指す環境の名前が複数あります: {names}")
            values[key] = _env_int(got_vals[-1][1]) if got_vals else None
        got = values["max_steps"]
        if got != self.max_steps:
            out.append(
                f"版のサーバーの手数の上限は {got if got is not None else '製品の既定'}、指定は "
                f"{self.max_steps if self.max_steps is not None else '製品の既定'}"
                "（serve --max-steps と run --max-steps をそろえる）"
            )
        got_b = values["budget_max_cost_usd_micros"]
        if got_b != self.budget_micros:
            out.append(
                f"版のサーバーの 1 発話の予算は {got_b if got_b is not None else '無し'}、指定は "
                f"{self.budget_micros}（serve --budget-micros と run --budget-micros をそろえる）"
            )
        return out


def condition_problems(proc: Process, cond: Conditions) -> list[str]:
    """そのプロセスの環境と SHIKI_CONFIG の `[chat]` を、Mode B の条件と比べた違いの理由。"""
    path = config_path(proc)
    if path is None:
        return ["版のサーバーの条件を確かめられません: SHIKI_CONFIG がありません"]
    raw = _read_toml(path)
    if isinstance(raw, str):
        return [f"版のサーバーの条件を確かめられません: {raw}"]
    chat = raw.get("chat") if isinstance(raw.get("chat"), Mapping) else {}
    return cond.problems(proc.env, chat)


def condition_checks(proc: Process, cond: Conditions) -> list[Check]:
    bad = condition_problems(proc, cond)
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
        if got.start_ticks is None:
            return f"pid {got.pid} の起動の時刻を読めません（同じプロセスか確かめられない）"
        if (got.pid, got.start_ticks, got.exe) != (was.pid, was.start_ticks, was.exe):
            return (
                f":{self.port} のサーバーが確かめた後に替わりました（pid {was.pid} → {got.pid}）。"
                "serve で版のサーバーを起動し直してから流し直す"
            )
        why = route_problem(got, self.cfg)
        if why:
            return f"版のサーバーの LLM の行き先: {why}"
        if self.conditions is not None:
            bad = condition_problems(got, self.conditions)
            if bad:
                return "・".join(bad)
        return None

"""版の記録（`data/versions/<label>.json`）と、版のサーバーの起動（dev-up.sh の引数を組む・GAP-30 §4.1・§4.2）。

- 版の記録は `scripts/build-version.sh` が書く（label・commit・tree_clean・binary_sha256・built_at・rustc）。
  ここでは読み、後の段（取り込み・機能の判定）が足す項目を `update_record` で足す。
- `python -m abeval serve --label L [--down]` は、記録とバイナリの sha を確かめてから dev-up.sh を版のサーバー向けの
  引数で呼ぶ（H3 (a)・D-54）。分けるもの: DB `shiki_eval_<label の - を _ に>`・store と バケット
  `shiki-eval-<label>`・Redis の DB 番号（label ごと）・API の :10067・ゲートウェイの 18190/18191。
  LLM の設定は `~/.config/shiki-dev/llm-eval.toml`（`SHIKI_CONFIG`）。呼び出し側のシェルに残った `SHIKI__*` は
  渡さない（条件は手数の上限 `SHIKI__CHAT__MAX_STEPS` と 1 発話の予算 `SHIKI__CHAT__BUDGET_MAX_COST_USD_MICROS`
  だけを、ここで決めて渡す）。
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import stat
import subprocess
import sys
from collections.abc import Mapping
from dataclasses import dataclass, field
from pathlib import Path

from abeval.config import ROOT, load_config

# 英小文字で始まり英小文字か数字で終わる（S3 のバケット名に使えるように・build-version.sh と同じ）。
LABEL_RE = re.compile(r"^[a-z](?:[a-z0-9-]{0,19}[a-z0-9])?$")
SHA_RE = re.compile(r"^[0-9a-f]{64}$")
COMMIT_RE = re.compile(r"^[0-9a-f]{40}$")
VERSIONS_DIR = ROOT / "data" / "versions"
DEV_UP = ROOT.parent.parent / ".claude" / "skills" / "pr" / "scripts" / "dev-up.sh"
LLM_EVAL_CONFIG = Path("~/.config/shiki-dev/llm-eval.toml")
EVAL_PORT = 10067
GATEWAY_PORTS = (18190, 18191)
# 1 発話の予算（マイクロ USD・再送を含めた請求額で製品が判定する・§4.2）。
DEFAULT_BUDGET_MICROS = 150_000
# 版のサーバーの Redis の DB 番号（0 は手元のスタック、3 は確かめ用に空けておく）。
REDIS_DB_FIRST = 4
REDIS_DB_COUNT = 12
REQUIRED = ("label", "commit", "tree_clean", "binary_sha256", "built_at", "rustc")


class VersionError(ValueError):
    """版の記録・バイナリ・設定の誤り（起動しない）。"""


def check_label(label: object) -> str:
    if not isinstance(label, str) or not LABEL_RE.match(label):
        raise VersionError(
            f"label は英小文字で始まり英小文字か数字で終わる、英小文字・数字・- の 21 文字までです: {label!r}"
        )
    return label


@dataclass(frozen=True)
class VersionRecord:
    label: str
    commit: str
    tree_clean: bool
    binary_sha256: str
    built_at: str
    rustc: str
    # 後の段が足す項目（機能の印・振る舞いの指紋など）。
    extra: dict = field(default_factory=dict)

    def to_dict(self) -> dict:
        return {**self.extra, **{k: getattr(self, k) for k in REQUIRED}}


def record_path(label: str, versions_dir: Path = VERSIONS_DIR) -> Path:
    return Path(versions_dir) / f"{check_label(label)}.json"


def parse_record(raw: Mapping, where: str = "<record>") -> VersionRecord:
    missing = [k for k in REQUIRED if k not in raw]
    if missing:
        raise VersionError(f"{where}: 項目がありません: {missing}")
    label = check_label(raw["label"])
    if not isinstance(raw["commit"], str) or not COMMIT_RE.match(raw["commit"]):
        raise VersionError(f"{where}: commit は 40 桁の sha です: {raw['commit']!r}")
    if not isinstance(raw["tree_clean"], bool):
        raise VersionError(f"{where}: tree_clean は真偽値です")
    if not isinstance(raw["binary_sha256"], str) or not SHA_RE.match(raw["binary_sha256"]):
        raise VersionError(f"{where}: binary_sha256 は 64 桁の hex です")
    for k in ("built_at", "rustc"):
        if not isinstance(raw[k], str) or not raw[k]:
            raise VersionError(f"{where}: {k} は空でない文字列です")
    extra = {k: v for k, v in raw.items() if k not in REQUIRED}
    return VersionRecord(
        label=label,
        commit=raw["commit"],
        tree_clean=raw["tree_clean"],
        binary_sha256=raw["binary_sha256"],
        built_at=raw["built_at"],
        rustc=raw["rustc"],
        extra=extra,
    )


def read_record(label: str, versions_dir: Path = VERSIONS_DIR) -> VersionRecord:
    path = record_path(label, versions_dir)
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        raise VersionError(
            f"版の記録がありません: {path}（scripts/build-version.sh {label} <commit> で作る）"
        ) from None
    except ValueError as e:
        raise VersionError(f"{path}: JSON として読めません: {e}") from e
    if not isinstance(raw, dict):
        raise VersionError(f"{path}: オブジェクトではありません")
    rec = parse_record(raw, str(path))
    if rec.label != label:
        raise VersionError(f"{path}: label が食い違います: {rec.label!r}")
    return rec


def write_record(rec: VersionRecord, versions_dir: Path = VERSIONS_DIR) -> Path:
    parse_record(rec.to_dict())  # 書く前に形を確かめる
    path = record_path(rec.label, versions_dir)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    tmp.write_text(
        json.dumps(rec.to_dict(), ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    os.replace(tmp, path)
    return path


def update_record(label: str, fields: Mapping, versions_dir: Path = VERSIONS_DIR) -> VersionRecord:
    """後の段の項目を足す（ビルドの項目は書き換えない）。"""
    clash = [k for k in fields if k in REQUIRED]
    if clash:
        raise VersionError(f"ビルドの項目は書き換えません: {clash}")
    rec = read_record(label, versions_dir)
    new = VersionRecord(**{k: getattr(rec, k) for k in REQUIRED}, extra={**rec.extra, **fields})
    write_record(new, versions_dir)
    return new


def file_sha256(path: Path) -> str:
    h = hashlib.sha256()
    with Path(path).open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def binary_path(label: str, home: Path) -> Path:
    return Path(home) / "bin" / f"shiki-server-{check_label(label)}"


def db_name(label: str) -> str:
    return "shiki_eval_" + check_label(label).replace("-", "_")


def store_name(label: str) -> str:
    return "shiki-eval-" + check_label(label)


def bucket_name(label: str) -> str:
    return "shiki-eval-" + check_label(label)


def run_label(label: str) -> str:
    return "abeval-" + check_label(label)


def redis_db(label: str) -> int:
    """label ごとに決まる Redis の DB 番号（4〜15。pub/sub のチャネルは UUID ごとなので重なっても害は小さい）。"""
    h = int(hashlib.sha256(check_label(label).encode()).hexdigest()[:8], 16)
    return REDIS_DB_FIRST + h % REDIS_DB_COUNT


@dataclass(frozen=True)
class ServePlan:
    argv: list[str]
    env: dict[str, str]
    # 呼び出し側の環境から外した SHIKI__*（表示用）。
    dropped: tuple[str, ...] = ()

    def show(self) -> str:
        keys = (
            "SHIKI_CONFIG",
            "SHIKI__LLM__BACKEND",
            "SHIKI__CHAT__MAX_STEPS",
            "SHIKI__CHAT__BUDGET_MAX_COST_USD_MICROS",
        )
        envs = " ".join(f"{k}={self.env[k]}" for k in keys if k in self.env)
        return f"{envs} {' '.join(self.argv)}".strip()


def _scrubbed_env(base: Mapping[str, str]) -> tuple[dict[str, str], tuple[str, ...]]:
    env, dropped = {}, []
    for k, v in base.items():
        if k.startswith("SHIKI__") or k == "SHIKI_CONFIG":
            dropped.append(k)
        else:
            env[k] = v
    return env, tuple(sorted(dropped))


def check_llm_config(path: Path) -> Path:
    path = Path(path).expanduser()
    try:
        st = path.stat()
    except FileNotFoundError:
        raise VersionError(
            f"LLM の設定がありません: {path}（config/llm-eval.example.toml を写して chmod 600）"
        ) from None
    if stat.S_IMODE(st.st_mode) & 0o077:
        raise VersionError(f"LLM の設定の権限が広すぎます（600 にする）: {path}")
    return path


def down_plan(
    label: str, *, dev_up: Path = DEV_UP, base_env: Mapping[str, str] | None = None
) -> ServePlan:
    env, dropped = _scrubbed_env(os.environ if base_env is None else base_env)
    return ServePlan(["bash", str(dev_up), "--down", "--run-label", run_label(label)], env, dropped)


def serve_plan(
    label: str,
    *,
    home: Path,
    llm_config: Path = LLM_EVAL_CONFIG,
    versions_dir: Path = VERSIONS_DIR,
    max_steps: int | None = None,
    budget_micros: int = DEFAULT_BUDGET_MICROS,
    redis: int | None = None,
    dev_up: Path = DEV_UP,
    base_env: Mapping[str, str] | None = None,
) -> ServePlan:
    """版のサーバーを起動する dev-up.sh の呼び出し（記録・バイナリの sha・設定の権限を確かめてから）。"""
    rec = read_record(label, versions_dir)
    if not rec.tree_clean:
        raise VersionError(f"{label}: きれいな commit のビルドではありません（tree_clean=false）")
    exe = binary_path(label, home)
    if not exe.is_file() or not os.access(exe, os.X_OK):
        raise VersionError(f"版のバイナリがありません: {exe}")
    actual = file_sha256(exe)
    if actual != rec.binary_sha256:
        raise VersionError(
            f"版のバイナリの sha が記録と違います: {exe}（記録 {rec.binary_sha256[:12]}… 実物 {actual[:12]}…）"
        )
    cfg = check_llm_config(llm_config)
    if max_steps is not None and (isinstance(max_steps, bool) or not 1 <= int(max_steps) <= 50):
        raise VersionError(f"max_steps は 1〜50 です: {max_steps!r}")
    if isinstance(budget_micros, bool) or not 1 <= int(budget_micros) <= 10_000_000:
        raise VersionError(f"budget_micros は 1〜10,000,000 です: {budget_micros!r}")
    n = redis_db(label) if redis is None else int(redis)
    if not 0 <= n <= 15:
        raise VersionError(f"redis は 0〜15 です: {n}")
    argv = [
        "bash",
        str(dev_up),
        "--server-bin",
        str(exe.resolve()),
        "--db-name",
        db_name(label),
        "--store-name",
        store_name(label),
        "--bucket",
        bucket_name(label),
        "--redis-db",
        str(n),
        "--port",
        str(EVAL_PORT),
        "--gateway-ports",
        f"{GATEWAY_PORTS[0]},{GATEWAY_PORTS[1]}",
        "--api-only",
        "--run-label",
        run_label(label),
    ]
    env, dropped = _scrubbed_env(os.environ if base_env is None else base_env)
    env["SHIKI_CONFIG"] = str(cfg)
    # llm-eval.toml の backend（stub）とそろえる（dev-up の既定も stub。呼び出し側の値を拾わない）。
    env["SHIKI__LLM__BACKEND"] = "stub"
    env["SHIKI__CHAT__BUDGET_MAX_COST_USD_MICROS"] = str(int(budget_micros))
    if max_steps is not None:
        env["SHIKI__CHAT__MAX_STEPS"] = str(int(max_steps))
    return ServePlan(argv, env, dropped)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        prog="python -m abeval serve", description="版のサーバーを起動・停止する"
    )
    ap.add_argument("--label", required=True)
    ap.add_argument(
        "--down", action="store_true", help="止める（dev-up.sh --down --run-label abeval-<label>）"
    )
    ap.add_argument(
        "--max-steps",
        type=int,
        help="チャットの手数の上限（SHIKI__CHAT__MAX_STEPS・既定は製品の値）",
    )
    ap.add_argument(
        "--budget-micros",
        type=int,
        default=DEFAULT_BUDGET_MICROS,
        help=f"1 発話の予算（マイクロ USD・既定 {DEFAULT_BUDGET_MICROS}）",
    )
    ap.add_argument("--llm-config", type=Path, default=LLM_EVAL_CONFIG)
    ap.add_argument("--redis-db", type=int, help="Redis の DB 番号（既定は label から決める）")
    ap.add_argument("--dry-run", action="store_true", help="呼び出しを表示するだけ")
    args = ap.parse_args(argv)
    try:
        check_label(args.label)
        if args.down:
            plan = down_plan(args.label)
        else:
            cfg = load_config()
            plan = serve_plan(
                args.label,
                home=cfg.paths.home,
                llm_config=args.llm_config,
                max_steps=args.max_steps,
                budget_micros=args.budget_micros,
                redis=args.redis_db,
            )
    except VersionError as e:
        print(f"abeval serve: {e}", file=sys.stderr)
        return 2
    if plan.dropped:
        print(
            f"abeval serve: 呼び出し側の環境から外しました: {', '.join(plan.dropped)}",
            file=sys.stderr,
        )
    print(plan.show(), file=sys.stderr, flush=True)
    if args.dry_run:
        return 0
    return subprocess.run(plan.argv, env=plan.env, cwd=ROOT, check=False).returncode


if __name__ == "__main__":
    raise SystemExit(main())

"""設定（`eval.toml`）の読み込み。

- 鍵の項目は置かせない（名前に key・authorization・secret・password を含む項目と、`sk-ant-` を含む値を拒む）。
  本物の鍵を読むのは手元の中継（`~/.config/shiki-dev/anthropic-relay.py`）だけ。
- 置き場は環境変数で差し替えられる（テストは一時の場所に向ける）: `ABEVAL_CONFIG`（設定のファイル）・
  `ABEVAL_HOME`・`ABEVAL_LEDGER`・`ABEVAL_CACHE_DIR`・`ABEVAL_RELAY_LEDGER`。
- 相対のパスは `eval/app-builder/` からの相対。
"""

from __future__ import annotations

import datetime as dt
import hashlib
import ipaddress
import json
import os
import tomllib
from collections.abc import Mapping
from dataclasses import dataclass, field
from pathlib import Path

from abeval.pricing import ModelPrice, Pricing, Tier, usd_to_micros

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_CONFIG = ROOT / "eval.toml"

# 設定に置かせない名前の部分（小文字で照合）と、鍵の形の値。
FORBIDDEN_NAME_PARTS = ("key", "authorization", "secret", "password")
KEY_MARKER = "sk-ant-"


class ConfigError(ValueError):
    """設定の誤り。"""


def canonical_json(value: object) -> str:
    """キーを並べ替えた区切りの詰まった JSON（sha の材料）。"""
    return json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":"))


def sha256_hex(data: str | bytes) -> str:
    if isinstance(data, str):
        data = data.encode()
    return hashlib.sha256(data).hexdigest()


def is_loopback_host(host: str) -> bool:
    """ループバック（127.0.0.0/8・::1・localhost）か。0.0.0.0 と :: は含めない。"""
    h = host.strip().strip("[]").lower()
    if h == "localhost":
        return True
    try:
        return ipaddress.ip_address(h).is_loopback
    except ValueError:
        return False


def _forbidden_name(name: str) -> bool:
    low = name.lower()
    return any(part in low for part in FORBIDDEN_NAME_PARTS)


def find_secret_like(value: object, at: str = "") -> list[str]:
    """鍵の形の項目の場所（名前に key などを含む項目・`sk-ant-` を含む値）。"""
    found: list[str] = []
    if isinstance(value, Mapping):
        for k, v in value.items():
            here = f"{at}.{k}" if at else str(k)
            if _forbidden_name(str(k)):
                found.append(here)
            found.extend(find_secret_like(v, here))
    elif isinstance(value, list | tuple):
        for i, v in enumerate(value):
            found.extend(find_secret_like(v, f"{at}[{i}]"))
    elif isinstance(value, str) and KEY_MARKER in value.lower():
        found.append(at or "<value>")
    return found


def _strip_secret_like(value: object) -> object:
    if isinstance(value, Mapping):
        return {
            k: _strip_secret_like(v)
            for k, v in value.items()
            if not _forbidden_name(str(k)) and not (isinstance(v, str) and KEY_MARKER in v.lower())
        }
    if isinstance(value, list):
        return [_strip_secret_like(v) for v in value]
    return value


def server_config_sha(path: Path) -> str:
    """製品の設定（llm-eval.toml など）の sha256。鍵の形の項目を除いてから正規化した JSON で数える。"""
    with path.open("rb") as f:
        raw = tomllib.load(f)
    return sha256_hex(canonical_json(_strip_secret_like(raw)))


@dataclass(frozen=True)
class Caps:
    eval_cap_usd: float
    run_cap_usd: float
    global_cap_usd: float

    @property
    def eval_cap_micros(self) -> int:
        return usd_to_micros(self.eval_cap_usd)

    @property
    def run_cap_micros(self) -> int:
        return usd_to_micros(self.run_cap_usd)

    @property
    def global_cap_micros(self) -> int:
        return usd_to_micros(self.global_cap_usd)


@dataclass(frozen=True)
class ProxyConfig:
    host: str
    port: int
    upstream: str
    allowed_models: tuple[str, ...]
    session_header: str
    tag_prefix: str


@dataclass(frozen=True)
class Paths:
    home: Path
    ledger: Path
    cache: Path
    relay_ledger: Path


@dataclass(frozen=True)
class ModeA:
    """Mode A の進め方（生成と validate の並列度・再試行の回数の上限）。"""

    parallel: int = 4
    max_attempts: int = 4


@dataclass(frozen=True)
class ModeB:
    """Mode B の進め方（承認の許可リスト・壁時計の上限・読み直しの間隔・run_case の待ち）。"""

    approve_tools: tuple[str, ...] = ()
    wall_clock_secs: float = 360.0
    poll_secs: float = 2.0
    run_case_timeout_secs: float = 180.0


@dataclass(frozen=True)
class Estimate:
    """1 試行の見積もり（USD・推測。E0 の実測で置き換える）。"""

    central: float
    worst: float
    # 条件ごとの最悪の上書き（effort の名前・手数の上限の数 → USD）。
    effort_worst: dict = field(default_factory=dict)
    steps_worst: dict = field(default_factory=dict)

    def worst_for(self, effort: str | None = None, max_steps: int | None = None) -> float:
        w = self.worst
        if effort is not None and effort in self.effort_worst:
            w = max(w, float(self.effort_worst[effort]))
        if max_steps is not None and str(max_steps) in self.steps_worst:
            w = max(w, float(self.steps_worst[str(max_steps)]))
        return w


DEFAULT_ESTIMATES = {
    "a": Estimate(0.005, 0.012),
    "b": Estimate(0.03, 0.05, {"high": 0.07}, {"12": 0.10}),
}


@dataclass(frozen=True)
class Config:
    source: Path
    sha: str
    pricing: Pricing
    caps: Caps
    proxy: ProxyConfig
    paths: Paths
    api_url: str
    approve_tools: tuple[str, ...]
    mode_a: ModeA = ModeA()
    mode_b: ModeB = ModeB()
    # 取り込んだ system の「今日の日付」を置き換える固定の日付。
    capture_date: dt.date = dt.date(2026, 11, 9)
    estimates: dict = field(default_factory=lambda: dict(DEFAULT_ESTIMATES))


def _section(raw: dict, name: str) -> dict:
    sec = raw.get(name)
    if not isinstance(sec, dict):
        raise ConfigError(f"[{name}] がありません")
    return sec


def _number(sec: dict, name: str, where: str) -> float:
    v = sec.get(name)
    if isinstance(v, bool) or not isinstance(v, int | float) or v <= 0:
        raise ConfigError(f"[{where}] {name} は正の数です: {v!r}")
    return float(v)


def _string(sec: dict, name: str, where: str) -> str:
    v = sec.get(name)
    if not isinstance(v, str) or not v.strip():
        raise ConfigError(f"[{where}] {name} は空でない文字列です: {v!r}")
    return v


def _strings(sec: dict, name: str, where: str) -> tuple[str, ...]:
    v = sec.get(name)
    if not isinstance(v, list) or not all(isinstance(x, str) and x for x in v):
        raise ConfigError(f"[{where}] {name} は文字列の配列です: {v!r}")
    return tuple(v)


_TIER_FIELDS = ("input", "output", "cache_read", "cache_write_5m", "cache_write_1h")


def _tier(sec: object, where: str) -> Tier:
    if not isinstance(sec, dict):
        raise ConfigError(f"[{where}] がありません")
    vals = {}
    for f in _TIER_FIELDS:
        v = sec.get(f)
        if isinstance(v, bool) or not isinstance(v, int) or v < 0:
            raise ConfigError(f"[{where}] {f} は 0 以上の整数です: {v!r}")
        vals[f] = v
    return Tier(**vals)


def _pricing(raw: dict) -> Pricing:
    sec = _section(raw, "pricing")
    threshold = sec.get("tier_threshold_tokens")
    if isinstance(threshold, bool) or not isinstance(threshold, int) or threshold <= 0:
        raise ConfigError(f"[pricing] tier_threshold_tokens は正の整数です: {threshold!r}")
    models = {}
    for name, body in sec.items():
        if name == "tier_threshold_tokens":
            continue
        if not isinstance(body, dict):
            raise ConfigError(f"[pricing] {name} は表です")
        models[name] = ModelPrice(
            low=_tier(body.get("low"), f'pricing."{name}".low'),
            high=_tier(body.get("high"), f'pricing."{name}".high'),
        )
    if not models:
        raise ConfigError("[pricing] にモデルがありません")
    return Pricing(models=models, tier_threshold_tokens=threshold)


def _listen(value: str) -> tuple[str, int]:
    host, sep, port = value.rpartition(":")
    if not sep or not port.isdigit():
        raise ConfigError(f"[proxy] listen は host:port です: {value!r}")
    host = host.strip("[]")
    if not is_loopback_host(host):
        raise ConfigError(f"[proxy] listen はループバックだけです: {value!r}")
    return host, int(port)


def _path(value: str, env: Mapping[str, str], env_name: str) -> Path:
    raw = env.get(env_name) or value
    p = Path(raw).expanduser()
    return p if p.is_absolute() else ROOT / p


def load_config(path: Path | None = None, env: Mapping[str, str] | None = None) -> Config:
    env = os.environ if env is None else env
    source = Path(path or env.get("ABEVAL_CONFIG") or DEFAULT_CONFIG)
    try:
        with source.open("rb") as f:
            raw = tomllib.load(f)
    except (OSError, tomllib.TOMLDecodeError) as e:
        raise ConfigError(f"設定を読めません: {source}: {e}") from e
    bad = find_secret_like(raw)
    if bad:
        raise ConfigError(
            "設定に鍵の形の項目は置けません（鍵は手元の中継だけが読む）: " + ", ".join(bad)
        )
    pricing = _pricing(raw)
    caps_sec = _section(raw, "caps")
    caps = Caps(
        eval_cap_usd=_number(caps_sec, "eval_cap_usd", "caps"),
        run_cap_usd=_number(caps_sec, "run_cap_usd", "caps"),
        global_cap_usd=_number(caps_sec, "global_cap_usd", "caps"),
    )
    if caps.run_cap_usd > caps.eval_cap_usd:
        raise ConfigError("[caps] run_cap_usd が eval_cap_usd を超えています")
    psec = _section(raw, "proxy")
    host, port = _listen(_string(psec, "listen", "proxy"))
    allowed = _strings(psec, "allowed_models", "proxy")
    missing = [m for m in allowed if m not in pricing.models]
    if missing:
        raise ConfigError(f"[proxy] allowed_models に単価の無いモデルがあります: {missing}")
    upstream = _string(psec, "upstream", "proxy").rstrip("/")
    if not upstream.startswith(("http://", "https://")):
        raise ConfigError(f"[proxy] upstream は http(s) の URL です: {upstream!r}")
    proxy = ProxyConfig(
        host=host,
        port=port,
        upstream=upstream,
        allowed_models=allowed,
        session_header=_string(psec, "session_header", "proxy").lower(),
        tag_prefix=_string(psec, "tag_prefix", "proxy"),
    )
    paths_sec = _section(raw, "paths")
    paths = Paths(
        home=_path(_string(paths_sec, "home", "paths"), env, "ABEVAL_HOME"),
        ledger=_path(_string(paths_sec, "ledger", "paths"), env, "ABEVAL_LEDGER"),
        cache=_path(_string(paths_sec, "cache", "paths"), env, "ABEVAL_CACHE_DIR"),
        relay_ledger=_path(_string(paths_sec, "relay_ledger", "paths"), env, "ABEVAL_RELAY_LEDGER"),
    )
    server = _section(raw, "server")
    mode_b = _section(raw, "mode_b")
    approve = mode_b.get("approve_tools")
    if not isinstance(approve, list) or not all(isinstance(x, str) and x for x in approve):
        raise ConfigError(f"[mode_b] approve_tools は文字列の配列です: {approve!r}")
    return Config(
        source=source,
        sha=sha256_hex(canonical_json(raw)),
        pricing=pricing,
        caps=caps,
        proxy=proxy,
        paths=paths,
        api_url=_string(server, "api_url", "server").rstrip("/"),
        approve_tools=tuple(approve),
        mode_a=_mode_a(raw.get("mode_a") or {}),
        mode_b=_mode_b(mode_b, tuple(approve)),
        capture_date=_capture_date(raw.get("capture") or {}),
        estimates=_estimates(raw.get("estimates") or {}),
    )


def _positive_int(sec: dict, name: str, where: str, default: int) -> int:
    v = sec.get(name, default)
    if isinstance(v, bool) or not isinstance(v, int) or v <= 0:
        raise ConfigError(f"[{where}] {name} は正の整数です: {v!r}")
    return v


def _mode_a(sec: dict) -> ModeA:
    return ModeA(
        parallel=_positive_int(sec, "parallel", "mode_a", ModeA.parallel),
        max_attempts=_positive_int(sec, "max_attempts", "mode_a", ModeA.max_attempts),
    )


def _mode_b(sec: dict, approve: tuple[str, ...]) -> ModeB:
    vals = {}
    for name in ("wall_clock_secs", "poll_secs", "run_case_timeout_secs"):
        vals[name] = _number(sec, name, "mode_b") if name in sec else getattr(ModeB, name)
    return ModeB(approve_tools=approve, **vals)


def _capture_date(sec: dict) -> dt.date:
    v = sec.get("fixed_date", "2026-11-09")
    try:
        return dt.date.fromisoformat(str(v))
    except ValueError as e:
        raise ConfigError(f"[capture] fixed_date は YYYY-MM-DD です: {v!r}") from e


def _estimates(sec: dict) -> dict:
    out = dict(DEFAULT_ESTIMATES)
    for mode, body in sec.items():
        if mode not in ("a", "b") or not isinstance(body, dict):
            raise ConfigError(f"[estimates] は a と b の表です: {mode!r}")
        central = _number(body, "central", f"estimates.{mode}")
        worst = _number(body, "worst", f"estimates.{mode}")
        if worst < central:
            raise ConfigError(f"[estimates.{mode}] worst が central より小さい")
        extra = {}
        for k in ("effort_worst", "steps_worst"):
            sub = body.get(k) or {}
            if not isinstance(sub, dict) or not all(
                isinstance(x, int | float) and not isinstance(x, bool) and x > 0
                for x in sub.values()
            ):
                raise ConfigError(f"[estimates.{mode}.{k}] は名前 → 正の数の表です")
            extra[k] = {str(a): float(b) for a, b in sub.items()}
        out[mode] = Estimate(central, worst, **extra)
    return out
